"use client";

/**
 * The top of a thread, once its first message is loaded, and the whole of a conversation with no messages yet: who it
 * is with (the face, the name, Carbon or Silicon and the id, a Silicon's custodian), or the group and how many are in
 * it, and one sentence: "This is the beginning of your conversation with Scout."
 */
import { accountName, kindWord, type ConversationKind } from "@/lib/chat/address";
import type { AccountRef, ConversationSummary } from "@/lib/chat/types";
import { AccountAvatar, GroupAvatar } from "../common/avatars";
import { useChat } from "../shell/chat-context";
import { useThreadActions, useThreadPeople } from "./thread-actions";
import styles from "./thread.module.css";

export function ThreadBeginning({ kind, others, title, conversation, empty }: { kind: ConversationKind; others: AccountRef[]; title: string; conversation: ConversationSummary | undefined; empty: boolean }) {
  const { view, viewed } = useChat();
  const actions = useThreadActions();
  const people = useThreadPeople();
  const single = kind !== "group" && others.length === 1 ? others[0]! : null;
  const custodian = single ? people.custodianOf(single) : null;
  const viewedName = viewed ? accountName(viewed) : view;
  const members = conversation?.participants.length ?? others.length + 1;
  const sentence = single
    ? view ? `This is the beginning of ${viewedName}'s conversation with ${accountName(single)}.` : `This is the beginning of your conversation with ${accountName(single)}.`
    : `This is the beginning of ${title}.`;
  const next = !empty ? null : actions.readOnly ? "Nothing has been said here yet." : "Say hello. What you send is kept even when you are offline, and goes out as soon as it can.";
  return (
    // In an empty conversation this sits inside the log, which reads new messages out; this is not one.
    <div className={styles.beginning} data-empty={empty ? "" : undefined} aria-live="off">
      {single ? <AccountAvatar account={single} size="row" decorative /> : <GroupAvatar name={title} size="row" decorative />}
      <p className={styles.beginningName}>{single ? accountName(single) : title}</p>
      <p className={styles.beginningMeta}>
        {single ? (
          <>
            <span>{kindWord(single.type)}</span>
            <span aria-hidden="true">·</span>
            <span className={styles.beginningId}>{single.id}</span>
            {custodian ? (
              <>
                <span aria-hidden="true">·</span>
                <span>looked after by {actions.isSelf(custodian) ? "you" : accountName(custodian)}</span>
              </>
            ) : null}
          </>
        ) : (
          <>
            <span>Group</span>
            <span aria-hidden="true">·</span>
            <span>{members} {members === 1 ? "member" : "members"}</span>
          </>
        )}
      </p>
      <p className={styles.beginningText}>
        {sentence}
        {next ? <> {next}</> : null}
      </p>
    </div>
  );
}
