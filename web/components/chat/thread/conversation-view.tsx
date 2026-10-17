"use client";

/**
 * One conversation: the header, the messages, the composer, the details panel's content, and the message menu and
 * details. It owns what the thread's parts share: the conversation and its messages (from the cache when they are
 * there), the outbox's Waiting and Failed sends, the first unread message as it was when you opened it (older pages
 * load first when it is further back), delivered receipts, presence (the typing line and the header's status), and
 * the actions on messages and accounts (thread-actions.tsx), which stay the same object for the whole visit.
 */
import Link from "next/link";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { MessagesSquare, WifiOff } from "lucide-react";
import { Problem } from "@/components/foundation/feedback/problem";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { ButtonLink } from "@/components/foundation/button-link";
import { Button } from "@/components/silicon-ui/button/button";
import { conversationHref, conversationKind, conversationTitle, directParticipants, inboxHref, isGroupAddress, messageUrl, otherParticipants, sameAccount, type ConversationKind } from "@/lib/chat/address";
import { deleteMessage } from "@/lib/chat/api";
import { draftHint, type DraftHint } from "@/lib/chat/drafts";
import { replaceMessage, upsertMessage, type MessagePages } from "@/lib/chat/cache";
import { useConversation, useMessages } from "@/lib/chat/hooks";
import { chatKeys } from "@/lib/chat/keys";
import { discardSend, retrySend, useConversationOutbox } from "@/lib/chat/outbox-store";
import { useParticipantsPresence } from "@/lib/chat/presence";
import { markAllRead, noteDelivered } from "@/lib/chat/receipts";
import { applyMessage } from "@/lib/chat/sync";
import { useTitle } from "@/lib/chat/title";
import type { AccountRef, Message } from "@/lib/chat/types";
import { notify } from "@/lib/notify";
import { useShortcutScope } from "@/lib/shortcuts/react";
import { toggleListen } from "@/lib/voice";
import { Composer, type ComposerHandle } from "../composer/composer";
import { useStartConversation } from "../dialogs/open-conversation";
import { DetailsPanel } from "../details/details-panel";
import { useChat } from "../shell/chat-context";
import { DetailsPortal } from "../shell/details";
import { MessageDetailsDialog } from "./message-details-dialog";
import { MessageList, type MessageListHandle } from "./message-list";
import { MessageMenu, type MenuState } from "./message-menu";
import { ThreadBeginning } from "./thread-beginning";
import { ThreadHeader } from "./thread-header";
import { custodianOf, findFirstUnread, plainFailure, unreadBeyondLoaded, viewedName, type Busy, type FirstUnread } from "./thread-model";
import { copyableText, ThreadActionsProvider, ThreadPeopleProvider, type ThreadActions, type ThreadPeople } from "./thread-actions";
import { useOnline } from "./use-online";
import styles from "./thread.module.css";

/** Older pages loaded on opening so the thread can open at the first unread message. */
const UNREAD_PAGES = 4;

async function copy(text: string, what: string) {
  try {
    await navigator.clipboard.writeText(text);
    notify.success(`${what} copied`);
  } catch {
    notify.error({ status: 0, code: "clipboard_refused", message: "This browser did not let the page copy.", hint: "Select the text and copy it yourself." }, `Could not copy the ${what.toLowerCase()}`);
  }
}

/** Before the conversation is known, its address says who is in it (a direct one) or that it is a group. */
function fromAddress(address: string, perspective: AccountRef): { others: AccountRef[]; kind: ConversationKind; title: string } {
  const pair = directParticipants(address);
  if (pair) {
    const other = pair.find(id => id !== perspective.id) ?? pair[0];
    const type = other.startsWith("si:") ? "silicon" : "carbon";
    return { others: [{ type, id: other }], kind: type, title: other };
  }
  return { others: [], kind: "group", title: address };
}

export function ConversationView({ address }: { address: string }) {
  const { me, view, viewed, perspective, readOnly } = useChat();
  const startConversation = useStartConversation();
  const queryClient = useQueryClient();
  const online = useOnline();
  const { conversation, error: conversationError } = useConversation(address, view);
  const thread = useMessages(address, view);
  const outbox = useConversationOutbox(address);
  const pending = readOnly ? [] : outbox;
  const composer = useRef<ComposerHandle>(null);
  const list = useRef<MessageListHandle>(null);
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [detailsFor, setDetailsFor] = useState<Message | null>(null);
  useShortcutScope("thread", true);

  const guess = useMemo(() => fromAddress(address, perspective), [address, perspective]);
  const others = useMemo(() => (conversation ? otherParticipants(conversation, perspective) : guess.others), [conversation, perspective, guess.others]);
  const kind = conversation ? conversationKind(conversation, perspective) : guess.kind;
  const title = conversation ? conversationTitle(conversation, perspective) : guess.title;
  const group = kind === "group";
  useTitle(`${title} · DM`);

  // Presence of the others only (a group with nobody else yet lists you as its "others").
  const presenceIds = useMemo(() => (readOnly ? [] : others.filter(account => !sameAccount(account, perspective)).slice(0, 8).map(account => account.id)), [others, readOnly, perspective]);
  const presence = useParticipantsPresence(presenceIds, !!conversation && !readOnly);
  // Who is busy, as a value that only changes when someone starts or stops (presence polls make a new map each time).
  const busySignature = others.map(account => `${account.id}:${presence.get(account.id)?.activity ?? ""}`).join("|");
  const busy = useMemo<Busy[]>(
    () => others.flatMap(account => {
      const activity = presence.get(account.id)?.activity;
      return activity ? [{ account, activity }] : [];
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- the signature says when the list changes
    [busySignature],
  );

  // The first unread is decided once per opening (this view is keyed by its address): reading moves the read
  // position, not the divider. When it is older than the first page, a few older pages load first.
  const [opened, setOpened] = useState<{ firstUnread: FirstUnread | null } | null>(null);
  const known = thread.isSuccess && (!!conversation || !!conversationError);
  const needOlder = known && !opened && unreadBeyondLoaded(conversation, thread.messages, perspective) && !!thread.hasNextPage && (thread.data?.pages.length ?? 0) <= UNREAD_PAGES;
  if (!opened && known && !needOlder && !thread.isFetchingNextPage) setOpened({ firstUnread: findFirstUnread(conversation, thread.messages, perspective) });
  const { isFetchingNextPage, fetchNextPage } = thread;
  useEffect(() => {
    if (needOlder && !isFetchingNextPage) void fetchNextPage();
  }, [needOlder, isFetchingNextPage, fetchNextPage]);
  const firstUnread = opened?.firstUnread ?? null;

  // What this tab fetched from others is delivered.
  useEffect(() => {
    if (readOnly) return;
    const messages = thread.messages;
    for (let index = messages.length - 1; index >= 0; index--) {
      const message = messages[index]!;
      if (sameAccount(message.sender, perspective) || message.deleted_at) continue;
      if (!message.delivered_at) noteDelivered(address, message["message-id"]);
      break;
    }
  }, [thread.messages, readOnly, perspective, address]);

  // Typing anywhere in the thread (outside a field or a control) types into the composer. It listens on window, after
  // the shortcuts (on document) had their turn: a key a shortcut used is never typed.
  useEffect(() => {
    if (readOnly) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey || event.key.length !== 1 || event.isComposing) return;
      const target = event.target as HTMLElement | null;
      if (target && target.closest("input, textarea, select, button, a, [contenteditable], [role='dialog'], [role='menu'], [role='listbox']")) return;
      if (target && !target.closest("main")) return;
      // Space on a selected message is Listen (or nothing where it cannot play), never a space typed into the composer.
      if (event.key === " " && target?.closest("[data-message-row]")) return;
      composer.current?.focus();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [readOnly]);

  const recipientDeleted = !group && others.length === 1 && others[0]!.deleted;
  const disabledReason = readOnly
    ? `You are reading ${viewedName(view, viewed, conversation)}'s conversation as its custodian. It is read only: nothing is sent or marked read for it.`
    : conversation?.superseded_by
      ? "This conversation is kept for its history. Write in the current one."
      : recipientDeleted
        ? "This account was deleted. The conversation stays for its history."
        : null;

  const actions = useMemo<ThreadActions>(() => {
    const messagesKey = chatKeys.messages(address, view);
    return {
      address,
      view,
      readOnly,
      isMine: message => sameAccount(message.sender, perspective),
      isSelf: account => sameAccount(account, me.account),
      reply: message => composer.current?.reply(message),
      edit: message => composer.current?.edit(message),
      copyText: message => void copy(copyableText(message), "Text"),
      copyLink: message => void copy(messageUrl(window.location.origin, address, message["message-id"], view), "Link"),
      requestDelete: message => setDeleting(message["message-id"]),
      cancelDelete: () => setDeleting(null),
      confirmDelete: async message => {
        const tombstone: Message = { ...message, message: null, attachments: [], attachment_details: [], voice_transcript: null, reply: null, history: [], deleted_at: new Date().toISOString(), voice: null, gif: null, speech: null };
        queryClient.setQueryData<MessagePages>(messagesKey, data => upsertMessage(data, tombstone));
        try {
          applyMessage(queryClient, perspective, await deleteMessage(address, message["message-id"], crypto.randomUUID()));
          setDeleting(null);
          document.querySelector<HTMLElement>(`[data-message-code="${CSS.escape(message["message-id"])}"]`)?.focus({ preventScroll: true });
        } catch (failure) {
          queryClient.setQueryData<MessagePages>(messagesKey, data => replaceMessage(data, message));
          notify.error(plainFailure(failure), "The message was not deleted");
          throw failure;
        }
      },
      openDetails: message => setDetailsFor(message),
      openMenu: (message, at) => setMenu({ message, ...at }),
      jumpTo: code => {
        window.history.replaceState(window.history.state, "", `${conversationHref(address, { view })}#${code}`);
        list.current?.jumpTo(code);
      },
      listen: message => toggleListen(address, message),
      // The same path as New conversation: into the cache, opened, its message field ready for the first word.
      openDirect: account => {
        startConversation([account.id]).catch(failure => notify.error(plainFailure(failure), `Your conversation with ${account.display_name || account.id} could not be opened`));
      },
      copyId: account => void copy(account.id, "Id"),
      retrySend,
      discardSend,
      focusComposer: () => composer.current?.focus(),
    };
  }, [address, view, readOnly, perspective, me.account, queryClient, startConversation]);

  const people = useMemo<ThreadPeople>(() => ({
    conversation,
    presence,
    custodianOf: account => custodianOf(account, conversation, me),
  }), [conversation, presence, me]);

  const onSent = useCallback(() => list.current?.scrollToBottom(), []);
  const onSelectLast = useCallback(() => list.current?.focusLast() ?? false, []);
  const onLeave = useCallback(() => {
    document.querySelector<HTMLElement>("[role='log']")?.focus();
  }, []);

  const failure = thread.error ? plainFailure(thread.error) : null;
  if (failure && !thread.messages.length && (failure.status === 404 || failure.status === 403)) {
    return (
      <Problem layout="inset" icon={<MessagesSquare strokeWidth={1.5} />} title={failure.status === 403 ? "This conversation is not yours to read" : "This conversation is not here"}
        actions={<ButtonLink href={inboxHref(view)}>Back to your conversations</ButtonLink>}>
        <p>{failure.message} {failure.hint}</p>
      </Problem>
    );
  }

  const name = group ? title : others[0] ? (others[0].display_name?.split(/\s+/)[0] ?? others[0].id) : title;
  // Whether the service has a draft here, as the conversation list says (once the thread loaded without one: ask).
  const hint: DraftHint = conversation ? draftHint(conversation) : conversationError || thread.isSuccess || thread.isError ? "unknown" : "pending";
  const empty = thread.isSuccess && !thread.messages.length && !pending.length;
  const unloaded = !!failure && !thread.messages.length;

  return (
    <ThreadActionsProvider value={actions}>
      <ThreadPeopleProvider value={people}>
        <section className={styles.thread} aria-labelledby="thread-title">
          <ThreadHeader address={address} view={view} title={title} kind={kind} conversation={conversation} others={others} presence={presence} busy={busy}
            onCopy={(text, what) => void copy(text, what)}
            onMarkRead={!readOnly && conversation ? () => void markAllRead([conversation]) : null} />
          {conversation?.superseded_by ? (
            <p className={styles.banner} role="note">
              This history continues in <Link href={conversationHref(conversation.superseded_by, { view })}>the current conversation</Link>.
            </p>
          ) : null}
          {!online ? (
            <p className={styles.banner} data-tone="offline" role="status">
              <WifiOff size={14} strokeWidth={1.75} aria-hidden="true" /> You are offline. New messages arrive when you are back; what you send waits and goes out then.
            </p>
          ) : null}
          {failure && !unloaded ? (
            <div className={styles.inlineError}>
              <ErrorAlert error={failure} title="Newer messages could not be loaded" action={<Button size="sm" variant="secondary" onClick={() => void thread.refetch()}>Try again</Button>} />
            </div>
          ) : null}
          {unloaded ? (
            <div className={styles.loadFailed}>
              <ErrorAlert error={failure} title="The messages could not be loaded" action={<Button size="sm" variant="secondary" onClick={() => void thread.refetch()}>Try again</Button>} />
            </div>
          ) : (
            <MessageList
              ref={list}
              address={address}
              title={title}
              messages={thread.messages}
              pending={pending}
              perspective={perspective}
              group={group}
              hasOlder={!!thread.hasNextPage}
              loadingOlder={thread.isFetchingNextPage}
              loadOlder={thread.fetchNextPage}
              firstUnread={firstUnread}
              loading={thread.isPending || !opened}
              busy={busy}
              deleting={deleting}
              beginning={<ThreadBeginning kind={kind} others={others} title={title} conversation={conversation} empty={empty} />}
            />
          )}
          <Composer ref={composer} address={address} name={name} messages={thread.messages} me={perspective} disabledReason={disabledReason} draftHint={hint} onSent={onSent} onSelectLast={onSelectLast} onLeave={onLeave} />
        </section>
        <MessageMenu state={menu} onClose={() => setMenu(null)} />
        <MessageDetailsDialog message={detailsFor} address={address} view={view} onClose={() => setDetailsFor(null)} />
        <DetailsPortal title={isGroupAddress(address) ? "Group details" : "Details"}>
          <DetailsPanel conversation={conversation} address={address} presence={presence} />
        </DetailsPortal>
      </ThreadPeopleProvider>
    </ThreadActionsProvider>
  );
}
