"use client";

/**
 * One message in the thread (spec §6.4, §6.11): an `article` named by its sender and time, carrying its code,
 * conversation, sender and state as data attributes, with the bubble, the line under it (time, Edited, via an app,
 * the ISI it was routed to, your state), and its actions: a toolbar on hover and on keyboard focus, a context menu,
 * and the keys of lib/shortcuts. Deleting asks in place. Sends still in the outbox render here too, as Waiting or
 * Failed (with Retry and Discard).
 */
import { memo, useEffect, useRef, type MouseEvent } from "react";
import { CornerUpLeft, Ellipsis, Pencil, RotateCw, X } from "lucide-react";
import { ConfirmMorph } from "@/components/silicon-ui/confirm-morph/confirm-morph";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/silicon-ui/popover/popover";
import { accountName, isiOf } from "@/lib/chat/address";
import { clockTime, fullTime } from "@/lib/chat/format";
import { plainPreview } from "@/lib/chat/markdown";
import type { OutboxItem } from "@/lib/chat/outbox";
import type { Message } from "@/lib/chat/types";
import { AccountAvatar } from "../common/avatars";
import { IconAction } from "../common/icon-action";
import { StatusTick, STATE_WORDS, stateOf, type MessageState } from "../common/status-tick";
import { MessageContent, ReplyQuote } from "./message-content";
import { RichText } from "./rich-text";
import { useThreadActions } from "./thread-actions";
import styles from "./thread.module.css";

export interface RowLayout {
  first: boolean;
  last: boolean;
  /** A group conversation: names over others' runs, faces beside them. */
  group: boolean;
}

function Edited({ message }: { message: Message }) {
  const history = message.history;
  return (
    <Popover>
      <PopoverTrigger asChild>
        <button type="button" className={styles.metaButton}>Edited</button>
      </PopoverTrigger>
      <PopoverContent className={styles.history} side="top" align="end">
        <p className={styles.historyTitle}>Edited{message.updated_at ? ` ${fullTime(message.updated_at)}` : ""}</p>
        {history.length ? (
          <ol className={styles.historyList}>
            {[...history].reverse().map((entry, index) => (
              <li key={index} className={styles.historyItem}>
                <span className={styles.historyWhen}>Before {index === 0 ? "the last edit" : `edit ${history.length - index}`}, from {fullTime(entry.created_at)}</span>
                <span>{plainPreview(entry.message || entry.voice_transcript || (entry.attachments.length ? "Attachment" : ""), 400) || "No text"}</span>
              </li>
            ))}
          </ol>
        ) : <p className={styles.historyQuiet}>The earlier text is not shown here.</p>}
      </PopoverContent>
    </Popover>
  );
}

function Meta({ message, mine, state }: { message: Message; mine: boolean; state: MessageState | null }) {
  const isi = isiOf(message.recipient_id);
  const edited = !message.deleted_at && (message.history.length > 0 || !!message.updated_at);
  return (
    <div className={styles.meta} data-mine={mine ? "" : undefined}>
      <time dateTime={message.created_at} title={fullTime(message.created_at)} className={styles.time}>{clockTime(message.created_at)}</time>
      {edited ? <Edited message={message} /> : null}
      {message.via_app ? <span className={styles.metaWord}>via {message.via_app}</span> : null}
      {isi ? <span className={styles.metaWord} title={`Routed to the ${isi} intent of ${message.recipient_id.slice(isi.length + 1)}`}>→ {isi}</span> : null}
      {mine && state ? <StatusTick state={state} size={13} /> : null}
    </div>
  );
}

export interface MessageRowProps extends RowLayout {
  message: Message;
  highlighted: boolean;
}

export const MessageRow = memo(function MessageRow({ message, first, last, group, highlighted }: MessageRowProps) {
  const actions = useThreadActions();
  const mine = actions.isMine(message);
  const code = message["message-id"];
  const deleted = !!message.deleted_at;
  const state: MessageState | null = mine && !deleted ? stateOf(message) : null;
  const name = mine ? "You" : accountName(message.sender);
  const confirming = actions.deleting === code;
  const confirmRef = useRef<HTMLDivElement>(null);

  // The question takes focus when it appears, so Enter or Escape answers it right away.
  useEffect(() => {
    if (!confirming) return;
    const frame = requestAnimationFrame(() => confirmRef.current?.querySelector<HTMLButtonElement>("button:not([disabled])")?.focus());
    return () => cancelAnimationFrame(frame);
  }, [confirming]);

  const openMenu = (event: MouseEvent) => {
    event.preventDefault();
    actions.openMenu(message, { x: event.clientX, y: event.clientY });
  };
  const openMenuFromButton = (event: MouseEvent<HTMLButtonElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    actions.openMenu(message, { x: rect.left, y: rect.bottom + 4 });
  };

  return (
    <article
      className={styles.message}
      data-mine={mine ? "" : undefined}
      data-first={first ? "" : undefined}
      data-last={last ? "" : undefined}
      data-highlight={highlighted ? "" : undefined}
      data-deleted={deleted ? "" : undefined}
      data-message-row=""
      data-message-code={code}
      data-conversation={message.conversation_id}
      data-sender-id={message.sender.id}
      data-status={state ?? undefined}
      aria-label={`${name}, ${clockTime(message.created_at)}${state ? `, ${STATE_WORDS[state]}` : ""}${deleted ? ", deleted" : ""}`}
      tabIndex={-1}
      onContextMenu={openMenu}
    >
      {group && !mine ? <div className={styles.gutter}>{last ? <AccountAvatar account={message.sender} size="bubble" decorative /> : null}</div> : null}
      <div className={styles.stack}>
        {group && !mine && first ? <p className={styles.sender}>{accountName(message.sender)}</p> : null}
        <div className={styles.bubbleRow}>
          <div className={styles.bubble} data-sq="surface">
            <MessageContent message={message} />
          </div>
          {deleted ? null : (
            <div className={styles.toolbar} data-sq="surface" role="toolbar" aria-label={`Actions for ${name}'s message`}>
              {actions.readOnly ? null : <IconAction size="sm" label="Reply" shortcut="message.reply" icon={<CornerUpLeft aria-hidden="true" />} onClick={() => actions.reply(message)} tooltipSide="top" />}
              {mine && !actions.readOnly ? <IconAction size="sm" label="Edit" shortcut="message.edit" icon={<Pencil aria-hidden="true" />} onClick={() => actions.edit(message)} tooltipSide="top" /> : null}
              <IconAction size="sm" label="More actions" icon={<Ellipsis aria-hidden="true" />} onClick={openMenuFromButton} tooltipSide="top" aria-haspopup="menu" />
            </div>
          )}
        </div>
        {last || confirming ? <Meta message={message} mine={mine} state={state} /> : null}
        {confirming ? (
          <div className={styles.confirm} data-mine={mine ? "" : undefined}>
            <ConfirmMorph
              ref={confirmRef}
              label="Delete"
              prompt="Delete for everyone?"
              confirmLabel="Delete"
              pendingLabel="Deleting"
              doneLabel="Deleted"
              defaultState="confirming"
              confirmTimeout={0}
              resultTimeout={1200}
              onConfirm={() => actions.confirmDelete(message)}
              onCancel={actions.cancelDelete}
              onStateChange={next => { if (next === "idle") actions.cancelDelete(); }}
            />
          </div>
        ) : null}
      </div>
    </article>
  );
});

/** A send still in the outbox: Waiting (the service does not have it yet) or Failed (with the service's words). */
export const PendingRow = memo(function PendingRow({ item, first, last }: { item: OutboxItem } & RowLayout) {
  const actions = useThreadActions();
  const state: MessageState = item.status === "failed" ? "failed" : "waiting";
  const at = new Date(item.createdAt).toISOString();
  return (
    <article
      className={styles.message}
      data-mine=""
      data-first={first ? "" : undefined}
      data-last={last ? "" : undefined}
      data-pending={state}
      data-pending-key={item.key}
      data-message-row=""
      data-status={state}
      data-conversation={item.address}
      aria-label={`You, ${clockTime(at)}, ${STATE_WORDS[state]}`}
      tabIndex={-1}
    >
      <div className={styles.stack}>
        <div className={styles.bubbleRow}>
          <div className={styles.bubble} data-sq="surface">
            {item.replyPreview ? <ReplyQuote reply={item.replyPreview} interactive={false} /> : null}
            {item.body.message ? <RichText text={item.body.message} /> : null}
            {!item.body.message && item.body.attachments?.length ? <p className={styles.tombstone}>{item.body.attachments.length === 1 ? "1 attachment" : `${item.body.attachments.length} attachments`}</p> : null}
          </div>
        </div>
        <div className={styles.meta} data-mine="">
          <time dateTime={at} className={styles.time}>{clockTime(at)}</time>
          <StatusTick state={state} size={13} />
        </div>
        {state === "failed" ? (
          <div className={styles.failed} role="alert">
            <p className={styles.failedText}>
              <strong>Not sent.</strong> {[item.error?.message, item.error?.hint].filter(Boolean).join(" ") || "The service did not accept it."}
            </p>
            <span className={styles.failedActions}>
              <button type="button" className={styles.failedButton} data-sq="surface" onClick={() => actions.retrySend(item.key)}><RotateCw size={14} strokeWidth={1.9} aria-hidden="true" /> Retry</button>
              <button type="button" className={styles.failedButton} data-sq="surface" onClick={() => actions.discardSend(item.key)}><X size={14} strokeWidth={1.9} aria-hidden="true" /> Discard</button>
            </span>
          </div>
        ) : null}
      </div>
    </article>
  );
});
