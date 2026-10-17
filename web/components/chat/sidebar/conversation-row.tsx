"use client";

/**
 * One conversation in the list (spec §6.3): its face (with presence when known), its name, the time of its last
 * message, one line about that message ("You: ", "Voice message · 0:12", "typing…", "Draft: …"), and either the number
 * of unread messages or the state of your own last message. A link: every conversation has its own address.
 */
import Link from "next/link";
import { memo, type ReactNode } from "react";
import { conversationHref, conversationKind, conversationTitle, kindWord, otherParticipants, sameAccount } from "@/lib/chat/address";
import { unreadOf } from "@/lib/chat/cache";
import { rowTime } from "@/lib/chat/format";
import { plainPreview } from "@/lib/chat/markdown";
import { activityWords } from "@/lib/chat/presence";
import { rowPreview } from "@/lib/chat/preview";
import type { OutboxItem } from "@/lib/chat/outbox";
import type { AccountRef, ConversationSummary, Presence } from "@/lib/chat/types";
import { ConversationAvatar } from "../common/avatars";
import { StatusTick, stateOf, type MessageState } from "../common/status-tick";
import styles from "./sidebar.module.css";

export interface ConversationRowProps {
  conversation: ConversationSummary;
  perspective: AccountRef;
  view: string | null;
  current: boolean;
  /** The newest send of this conversation still in the outbox (Waiting or Failed). */
  pending?: OutboxItem;
  /** Presence of the other account, known while the conversation is open. */
  presence?: Presence | null;
  /** Unsent words typed here (local draft), for conversations other than the open one. */
  draft?: string;
  /** The reader's clock; null while hydrating (times are the reader's own, so the server never writes them). */
  now: Date | null;
}

export const ConversationRow = memo(function ConversationRow({ conversation, perspective, view, current, pending, presence, draft, now }: ConversationRowProps) {
  const title = conversationTitle(conversation, perspective);
  const others = otherParticipants(conversation, perspective);
  const kind = conversationKind(conversation, perspective);
  const group = kind === "group";
  const last = conversation.last_message;
  // The open conversation's own count is shown in the thread (the New messages divider, Jump to latest), not here:
  // it would only flicker while what arrives is being read.
  const unread = current ? 0 : unreadOf(conversation, perspective);
  const preview = rowPreview(last, { me: perspective, group });
  const pendingNewer = pending && (!last || pending.createdAt > Date.parse(last.created_at));
  const mineLast = !!last && sameAccount(last.sender, perspective) && !last.deleted_at;
  const state: MessageState | null = pendingNewer ? (pending.status === "failed" ? "failed" : "waiting") : mineLast ? stateOf(last) : null;
  const typing = presence?.activity ? activityWords(presence.activity) : null;
  const time = !now ? "" : pendingNewer ? rowTime(pending.createdAt, now) : rowTime(last?.created_at ?? conversation.updated_at, now);

  let line: ReactNode;
  if (typing) {
    line = <span className={styles.rowPreview} data-typing="">{typing}</span>;
  } else if (draft) {
    line = <span className={styles.rowPreview}><span className={styles.draftWord}>Draft:</span> {plainPreview(draft, 120)}</span>;
  } else if (pendingNewer) {
    line = <span className={styles.rowPreview}><span className={styles.who}>You:</span> {plainPreview(pending.body.message ?? "", 120) || "Attachment"}</span>;
  } else {
    line = (
      <span className={styles.rowPreview} data-quiet={preview.quiet ? "" : undefined} data-unread={unread ? "" : undefined}>
        {preview.who ? <span className={styles.who}>{preview.who}: </span> : null}
        {preview.text}
      </span>
    );
  }

  return (
    <li className={styles.item}>
      <Link
        href={conversationHref(conversation.id, { view })}
        className={styles.row}
        data-sq="surface"
        data-row=""
        data-conversation={conversation.id}
        data-unread={unread ? "" : undefined}
        aria-current={current ? "page" : undefined}
        prefetch={false}
      >
        <ConversationAvatar group={conversation.group} others={others} presence={presence} decorative />
        <span className={styles.rowMain}>
          <span className={styles.rowTop}>
            <span className={styles.rowName}>{title}</span>
            <span className="sr-only">, {kindWord(kind)}</span>
            {time ? <span className={styles.rowTime} data-unread={unread ? "" : undefined}>{time}</span> : null}
          </span>
          <span className={styles.rowBottom}>
            {line}
            {unread > 0 ? (
              <span className={styles.badge} data-sq="surface">
                {unread > 99 ? "99+" : unread}
                <span className="sr-only"> unread</span>
              </span>
            ) : state ? (
              <StatusTick state={state} />
            ) : null}
          </span>
        </span>
      </Link>
    </li>
  );
});
