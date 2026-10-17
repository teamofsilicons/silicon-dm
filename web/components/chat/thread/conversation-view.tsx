"use client";

/**
 * One conversation: the header, the messages, the composer, the details panel's content and the message menu and
 * details. It owns what the thread's parts share: the conversation and its messages (from the cache when they are
 * there), the outbox's Waiting and Failed sends, the first unread message as it was when you opened it, delivered and
 * read receipts, presence, and the actions on messages (thread-actions.tsx).
 */
import Link from "next/link";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { MessagesSquare } from "lucide-react";
import { Problem } from "@/components/foundation/feedback/problem";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { ButtonLink } from "@/components/foundation/button-link";
import { Button } from "@/components/silicon-ui/button/button";
import { conversationHref, conversationKind, conversationTitle, directParticipants, inboxHref, isGroupAddress, messageUrl, otherParticipants, sameAccount, type ConversationKind } from "@/lib/chat/address";
import { deleteMessage } from "@/lib/chat/api";
import { replaceMessage, upsertMessage, type MessagePages } from "@/lib/chat/cache";
import { compareCodes } from "@/lib/chat/codes";
import { useConversation, useMessages } from "@/lib/chat/hooks";
import { chatKeys } from "@/lib/chat/keys";
import { discardSend, retrySend, useConversationOutbox } from "@/lib/chat/outbox-store";
import { useParticipantsPresence } from "@/lib/chat/presence";
import { markAllRead, noteDelivered } from "@/lib/chat/receipts";
import { applyMessage } from "@/lib/chat/sync";
import { useTitle } from "@/lib/chat/title";
import type { AccountRef, ConversationSummary, Message } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { notify } from "@/lib/notify";
import { useShortcutScope } from "@/lib/shortcuts/react";
import { Composer, type ComposerHandle } from "../composer/composer";
import { DetailsPanel } from "../details/details-panel";
import { useChat } from "../shell/chat-context";
import { DetailsPortal } from "../shell/details";
import { MessageDetailsDialog } from "./message-details-dialog";
import { MessageList, type MessageListHandle } from "./message-list";
import { MessageMenu, type MenuState } from "./message-menu";
import { ThreadHeader } from "./thread-header";
import { copyableText, ThreadActionsProvider, type ThreadActions } from "./thread-actions";
import styles from "./thread.module.css";

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

/** The first unread message from someone else, and how many there are from it on. */
function findFirstUnread(conversation: ConversationSummary | undefined, messages: Message[], perspective: AccountRef): { code: string; count: number } | null {
  const fromOthers = messages.filter(message => !sameAccount(message.sender, perspective) && !message.deleted_at);
  let first: Message | undefined;
  if (conversation && conversation.last_read_message_id !== undefined) {
    if (!conversation.unread_count) return null;
    const read = conversation.last_read_message_id;
    first = fromOthers.find(message => !read || compareCodes(message["message-id"], read) > 0);
  } else if (conversation && !conversation.group && conversation.participants.length <= 2) {
    // An older service has no read position: in a direct conversation, read_at is the other's own read.
    first = fromOthers.find(message => !message.read_at);
  }
  if (!first) return null;
  const count = fromOthers.filter(message => compareCodes(message["message-id"], first["message-id"]) >= 0).length;
  return { code: first["message-id"], count };
}

export function ConversationView({ address }: { address: string }) {
  const { view, perspective, readOnly } = useChat();
  const queryClient = useQueryClient();
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

  const guess = fromAddress(address, perspective);
  const others = conversation ? otherParticipants(conversation, perspective) : guess.others;
  const kind = conversation ? conversationKind(conversation, perspective) : guess.kind;
  const title = conversation ? conversationTitle(conversation, perspective) : guess.title;
  const group = kind === "group";
  useTitle(`${title} · DM`);

  const presenceIds = useMemo(() => (readOnly ? [] : others.slice(0, 8).map(account => account.id)), [others, readOnly]);
  const presence = useParticipantsPresence(presenceIds, !!conversation && !readOnly);

  // The first unread is decided once per opening (this view is keyed by its address): reading moves the read
  // position, not the divider.
  const [opened, setOpened] = useState<{ firstUnread: { code: string; count: number } | null } | null>(null);
  if (!opened && thread.isSuccess && (conversation || conversationError)) setOpened({ firstUnread: findFirstUnread(conversation, thread.messages, perspective) });
  const firstUnread = opened?.firstUnread ?? null;

  // What this tab fetched from others is delivered.
  useEffect(() => {
    if (readOnly) return;
    const newest = [...thread.messages].reverse().find(message => !sameAccount(message.sender, perspective) && !message.deleted_at);
    if (newest && !newest.delivered_at) noteDelivered(address, newest["message-id"]);
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
      composer.current?.focus();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [readOnly]);

  const recipientDeleted = !group && others.length === 1 && others[0]!.deleted;
  const disabledReason = readOnly
    ? `You are reading ${view}'s conversation as its custodian. It is read only: nothing is sent or marked read for it.`
    : conversation?.superseded_by
      ? "This conversation is kept for its history. Write in the current one."
      : recipientDeleted
        ? "This account was deleted. The conversation stays for its history."
        : null;

  const messagesKey = chatKeys.messages(address, view);
  const actions = useMemo<ThreadActions>(() => ({
    address,
    view,
    readOnly,
    isMine: message => sameAccount(message.sender, perspective),
    reply: message => composer.current?.reply(message),
    edit: message => composer.current?.edit(message),
    copyText: message => void copy(copyableText(message), "Text"),
    copyLink: message => void copy(messageUrl(window.location.origin, address, message["message-id"], view), "Link"),
    requestDelete: message => setDeleting(message["message-id"]),
    cancelDelete: () => setDeleting(null),
    confirmDelete: async message => {
      const tombstone: Message = { ...message, message: null, attachments: [], voice_transcript: null, reply: null, history: [], deleted_at: new Date().toISOString(), voice: null, gif: null, speech: null };
      queryClient.setQueryData<MessagePages>(messagesKey, data => upsertMessage(data, tombstone));
      try {
        applyMessage(queryClient, perspective, await deleteMessage(address, message["message-id"], crypto.randomUUID()));
        setDeleting(null);
      } catch (failure) {
        queryClient.setQueryData<MessagePages>(messagesKey, data => replaceMessage(data, message));
        notify.error(failure, "The message was not deleted");
        throw failure;
      }
    },
    deleting,
    openDetails: message => setDetailsFor(message),
    openMenu: (message, at) => setMenu({ message, ...at }),
    jumpTo: code => {
      window.history.replaceState(window.history.state, "", `${conversationHref(address, { view })}#${code}`);
      list.current?.jumpTo(code);
    },
    retrySend,
    discardSend,
    focusComposer: () => composer.current?.focus(),
  }), [address, view, readOnly, perspective, deleting, queryClient, messagesKey]);

  const onSent = useCallback(() => list.current?.scrollToBottom(), []);
  const onSelectLast = useCallback(() => list.current?.focusLast() ?? false, []);
  const onLeave = useCallback(() => {
    document.querySelector<HTMLElement>("[role='log']")?.focus();
  }, []);

  const failure = thread.error ? ApiError.from(thread.error) : null;
  if (failure && !thread.messages.length && (failure.status === 404 || failure.status === 403)) {
    return (
      <Problem layout="inset" icon={<MessagesSquare strokeWidth={1.5} />} title={failure.status === 403 ? "This conversation is not yours to read" : "This conversation is not here"}
        actions={<ButtonLink href={inboxHref(view)}>Back to your conversations</ButtonLink>}>
        <p>{failure.message} {failure.hint}</p>
      </Problem>
    );
  }

  const name = group ? title : others[0] ? (others[0].display_name?.split(/\s+/)[0] ?? others[0].id) : title;

  return (
    <ThreadActionsProvider value={actions}>
      <section className={styles.thread} aria-labelledby="thread-title">
        <ThreadHeader address={address} view={view} title={title} kind={kind} conversation={conversation} others={others} presence={presence}
          onCopy={(text, what) => void copy(text, what)}
          onMarkRead={!readOnly && conversation ? () => void markAllRead([conversation]) : null} />
        {conversation?.superseded_by ? (
          <p className={styles.banner} role="note">
            This history continues in <Link href={conversationHref(conversation.superseded_by, { view })}>the current conversation</Link>.
          </p>
        ) : null}
        {failure ? (
          <div className={styles.inlineError}>
            <ErrorAlert error={failure} title="Messages could not be loaded" action={<Button size="sm" variant="secondary" onClick={() => void thread.refetch()}>Try again</Button>} />
          </div>
        ) : null}
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
          loading={thread.isPending}
        />
        <Composer ref={composer} address={address} name={name} messages={thread.messages} me={perspective} disabledReason={disabledReason} onSent={onSent} onSelectLast={onSelectLast} onLeave={onLeave} />
      </section>
      <MessageMenu state={menu} onClose={() => setMenu(null)} />
      <MessageDetailsDialog message={detailsFor} address={address} view={view} onClose={() => setDetailsFor(null)} />
      <DetailsPortal title={isGroupAddress(address) ? "Group details" : "Details"}>
        <DetailsPanel conversation={conversation} address={address} presence={presence} />
      </DetailsPortal>
    </ThreadActionsProvider>
  );
}
