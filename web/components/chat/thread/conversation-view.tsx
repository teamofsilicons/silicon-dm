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
import { accountName, conversationHref, conversationKind, conversationTitle, directParticipants, inboxHref, isGroupAddress, messageUrl, otherParticipants, sameAccount, type ConversationKind } from "@/lib/chat/address";
import { deleteMessage } from "@/lib/chat/api";
import { draftHint, type DraftHint } from "@/lib/chat/drafts";
import { replaceMessage, upsertMessage, type MessagePages } from "@/lib/chat/cache";
import { OLDER_PAGE_LIMIT, useConversation, useMessages } from "@/lib/chat/hooks";
import { chatKeys } from "@/lib/chat/keys";
import { discardSend, retrySend, useConversationOutbox } from "@/lib/chat/outbox-store";
import { useParticipantsPresence } from "@/lib/chat/presence";
import { markAllRead, noteDelivered } from "@/lib/chat/receipts";
import { applyMessage } from "@/lib/chat/sync";
import { useTitle } from "@/lib/chat/title";
import type { AccountRef, Message, Presence } from "@/lib/chat/types";
import { notify } from "@/lib/notify";
import { useShortcutScope } from "@/lib/shortcuts/react";
import { toggleListen, useListenAnnouncement } from "@/lib/voice";
import { Composer, type ComposerHandle } from "../composer/composer";
import { useStartConversation } from "../dialogs/open-conversation";
import { DetailsPanel } from "../details/details-panel";
import { openDialog } from "../dialogs/store";
import { useChat } from "../shell/chat-context";
import { DetailsPortal } from "../shell/details";
import { MessageDetailsDialog } from "./message-details-dialog";
import { MessageList, type MessageListHandle } from "./message-list";
import { MessageMenu, type MenuState } from "./message-menu";
import { ThreadBeginning } from "./thread-beginning";
import { ThreadHeader } from "./thread-header";
import { conversationPhrase, custodianOf, findFirstUnread, plainFailure, unreadBeyondLoaded, unreadWithinReach, viewedName, type Busy, type FirstUnread } from "./thread-model";
import { copyableText, ThreadActionsProvider, ThreadPeopleProvider, valueStore, type ThreadActions, type ThreadPeople } from "./thread-actions";
import { takeKeyboardFromOwnRow, takeKeyboardOnOpen } from "./open-focus";
import { useOnline } from "./use-online";
import styles from "./thread.module.css";

/** Older pages loaded on opening so the thread can open at the first unread message. */
const UNREAD_PAGES = 4;
/** Nobody's presence, while this browser is offline. */
const NO_PRESENCE: Map<string, Presence> = new Map();
const OFFLINE_WORDS = "You are offline. New messages arrive when you are back; what you send waits and goes out then.";

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
  const listening = useListenAnnouncement(address);
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
  const heard = useParticipantsPresence(presenceIds, !!conversation && !readOnly);
  // Offline here, nobody's presence is known: what was last heard would go on saying "online" (or "typing…") for
  // someone who has long gone. The header says the browser waits for the network instead; it asks again once back.
  const presence = online ? heard : NO_PRESENCE;
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
  // Only while a few pages can reach it (thread-model.ts unreadWithinReach): a first unread further back opens at once.
  const reachable = unreadWithinReach(conversation?.unread_count, thread.data?.pages[0]?.items.length || 50, OLDER_PAGE_LIMIT, UNREAD_PAGES);
  const needOlder = known && !opened && reachable && unreadBeyondLoaded(conversation, thread.messages, perspective) && !!thread.hasNextPage && (thread.data?.pages.length ?? 0) <= UNREAD_PAGES;
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

  // Typing anywhere in the thread (outside a field or a control), or with the keyboard nowhere (the page itself has it),
  // types into the composer. It listens on window, after the shortcuts (on document) had their turn: a key a shortcut
  // used is never typed.
  useEffect(() => {
    if (readOnly) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey || event.key.length !== 1 || event.isComposing) return;
      const target = event.target as HTMLElement | null;
      const nowhere = !target || target === document.body || target === document.documentElement;
      if (nowhere) {
        // A space alone starts nothing.
        if (event.key === " ") return;
      } else {
        if (target.closest("input, textarea, select, button, a, [contenteditable], [role='dialog'], [role='menu'], [role='listbox']")) return;
        if (!target.closest("main")) return;
        // Space on a selected message is Listen (or nothing where it cannot play), never a space typed into the composer.
        if (event.key === " " && target.closest("[data-message-row]")) return;
      }
      composer.current?.focus();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [readOnly]);

  // Opening by switching (⌥↑/⌥↓, the palette, a row of the list, a fresh load) gives the message field the keyboard,
  // so the next key is the first word (open-focus.ts says when; never on a touch screen). Once per opening: this view
  // is keyed by its address. A read-only view has no field to type into.
  const openedReadOnly = useRef(readOnly);
  useEffect(() => (openedReadOnly.current ? undefined : takeKeyboardOnOpen(() => composer.current?.focus())), []);
  useEffect(() => (readOnly ? undefined : takeKeyboardFromOwnRow(address, () => composer.current?.focus())), [readOnly, address]);

  const recipientDeleted = !group && others.length === 1 && others[0]!.deleted;
  const disabledReason = readOnly
    ? `You are reading ${viewedName(view, viewed, conversation)}'s conversation as its custodian: nothing is sent or marked read for it. You can delete ${viewedName(view, viewed, conversation)}'s own messages.`
    : conversation?.superseded_by
      ? "This conversation is kept for its history. Write in the current one."
      : recipientDeleted
        ? "This account was deleted. The conversation stays for its history."
        : null;

  // Layers opened from messages (menus, popovers, cards, dialogs): while one is open, no toolbar shows.
  const layers = useMemo(() => valueStore(0), []);
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
      // In a custodian's view the viewed Silicon's own messages are deletable (DELETE …?for=si:x); nothing else is.
      canDelete: message => !message.deleted_at && sameAccount(message.sender, perspective) && (!readOnly || !!view),
      requestDelete: message => setDeleting(message["message-id"]),
      cancelDelete: code => setDeleting(current => (code === undefined || current === code ? null : current)),
      confirmDelete: async message => {
        const tombstone: Message = { ...message, message: null, attachments: [], attachment_details: [], voice_transcript: null, reply: null, history: [], deleted_at: new Date().toISOString(), voice: null, gif: null, speech: null };
        queryClient.setQueryData<MessagePages>(messagesKey, data => upsertMessage(data, tombstone));
        try {
          applyMessage(queryClient, perspective, await deleteMessage(address, message["message-id"], crypto.randomUUID(), view));
          setDeleting(null);
          // The keyboard was on the question, which goes: back to the message. Not when it moved on meanwhile (the
          // deletion shows at once, and the next message may already have been selected): that stays.
          const row = document.querySelector<HTMLElement>(`[data-message-code="${CSS.escape(message["message-id"])}"]`);
          const active = document.activeElement;
          if (!active || active === document.body || row?.contains(active)) row?.focus({ preventScroll: true });
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
      layers,
    };
  }, [address, view, readOnly, perspective, me.account, queryClient, startConversation, layers]);

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
    // Words for the Carbon who followed an old link or mistyped one: the service's own hint is for the CLI and HTTP
    // clients ("dm conversations list…"), so it stays out.
    const invited = failure.code === "invitation_pending";
    const what = isGroupAddress(address) ? "group" : "conversation";
    return (
      <Problem layout="inset" icon={<MessagesSquare strokeWidth={1.5} />}
        title={invited ? "You are invited to this group" : failure.status === 403 ? `This ${what} is not yours to read` : `This ${what} is not here`}
        actions={(
          <>
            {invited && !view ? <Button onClick={() => openDialog("invitations")}>Open your invitations</Button> : null}
            <ButtonLink href={inboxHref(view)} variant={invited && !view ? "secondary" : undefined}>Back to your conversations</ButtonLink>
          </>
        )}>
        <p>
          {invited
            ? "Accept the invitation to read its messages and write in it."
            : failure.status === 403
              ? `You are not in this ${what}, so DM does not show it here.`
              : <>There is no {what} <code className={styles.problemAddress}>{address}</code> that you are in: the link may be wrong, or you may have left it.</>}
          {invited ? null : view ? ` ${viewedName(view, viewed, conversation)}'s conversations are in the list.` : " Your conversations are in the list."}
        </p>
      </Problem>
    );
  }

  const name = group ? title : others[0] ? (others[0].display_name?.split(/\s+/)[0] ?? others[0].id) : title;
  // How sentences name this conversation ("your conversation with Sol", "Sol's conversation with Wren", "Launch week").
  const named = !group || !!conversation?.group || isGroupAddress(address);
  const where = conversationPhrase({
    whose: view ? `${viewedName(view, viewed, conversation)}'s` : "your",
    others: named && group ? [] : others.filter(account => !sameAccount(account, perspective)).map(accountName),
    title,
  });
  // Whether the service has a draft here, as the conversation list says (once the thread loaded without one: ask).
  const hint: DraftHint = conversation ? draftHint(conversation) : conversationError || thread.isSuccess || thread.isError ? "unknown" : "pending";
  const empty = thread.isSuccess && !thread.messages.length && !pending.length;
  const unloaded = !!failure && !thread.messages.length;

  return (
    <ThreadActionsProvider value={actions}>
      <ThreadPeopleProvider value={people}>
        <section className={styles.thread} aria-labelledby="thread-title">
          <ThreadHeader address={address} view={view} title={title} kind={kind} conversation={conversation} others={others} presence={presence} busy={busy} offline={!online && !readOnly}
            onCopy={(text, what) => void copy(text, what)}
            onMarkRead={!readOnly && conversation ? () => void markAllRead([conversation]) : null} />
          {conversation?.superseded_by ? (
            <p className={styles.banner} role="note">
              This history continues in <Link href={conversationHref(conversation.superseded_by, { view })}>the current conversation</Link>.
            </p>
          ) : null}
          {/* The thread's one polite region, always there (a region added with its words is often not read): going
              offline, and what Listen on a message is doing. What it says is drawn elsewhere (the banner, the bubble). */}
          <span className="sr-only" role="status">{online ? listening : OFFLINE_WORDS}</span>
          {!online ? (
            <p className={styles.banner} data-tone="offline" aria-hidden="true">
              <WifiOff size={14} strokeWidth={1.75} aria-hidden="true" />
              <span className={styles.bannerLong}>{OFFLINE_WORDS}</span>
              <span className={styles.bannerShort}>Offline: what you send goes out when you are back.</span>
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
        <MessageDetailsDialog message={detailsFor} address={address} view={view} where={where} onClose={() => setDetailsFor(null)} />
        <DetailsPortal title={isGroupAddress(address) ? "Group details" : "Details"}>
          <DetailsPanel conversation={conversation} address={address} presence={presence} />
        </DetailsPortal>
      </ThreadPeopleProvider>
    </ThreadActionsProvider>
  );
}
