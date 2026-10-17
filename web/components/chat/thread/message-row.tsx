"use client";

/**
 * One message in the thread (spec §6.4, §6.11): an `article` named by its sender, time, state and words, carrying its
 * code, conversation, sender and state as data attributes; the bubble; the line under it (the time and your state under
 * the last of a run; Edited with its history, via an app and the ISI it was routed to under any message that has them);
 * and its actions: a toolbar while the pointer is over it, it was tapped or it has the keyboard (beside the bubble, or
 * over its top edge where the column has no room), the context menu, and the keys of lib/shortcuts. Deleting asks in
 * place. Sends still in the outbox render here too, as Waiting or Failed (with Retry and Discard). Your own rows are
 * not read out by the log as they are added.
 *
 * Rows are memoized on what they show (thread-model.ts sameMessageVersion), so a new message renders one row, not the
 * thread. The toolbar exists only on the row in use, and in a group the sender's name and face open the account card
 * (account-card.tsx).
 */
import { forwardRef, memo, useEffect, useLayoutEffect, useMemo, useRef, useState, type FocusEvent, type MouseEvent } from "react";
import { CornerUpLeft, Ellipsis, Headphones, Pause, Pencil, RotateCw, X } from "lucide-react";
import { ConfirmMorph } from "@/components/silicon-ui/confirm-morph/confirm-morph";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/silicon-ui/popover/popover";
import { TextShimmer } from "@/components/silicon-ui/text-shimmer/text-shimmer";
import { accountName, isiOf } from "@/lib/chat/address";
import { canEditHere } from "@/lib/chat/compose";
import { clockTime, fullTime } from "@/lib/chat/format";
import { plainPreview } from "@/lib/chat/markdown";
import { messageMedia } from "@/lib/chat/media";
import type { OutboxItem } from "@/lib/chat/outbox";
import type { Message } from "@/lib/chat/types";
import { canListen, useListenState, useVoiceAvailability, warmListen } from "@/lib/voice";
import { AccountAvatar } from "../common/avatars";
import { IconAction } from "../common/icon-action";
import { StatusTick, STATE_WORDS, stateOf, type MessageState } from "../common/status-tick";
import { AccountTrigger } from "./account-card";
import { MessageContent, ReplyQuote } from "./message-content";
import { RichText } from "./rich-text";
import { applyRowTabbing } from "./row-focus";
import { emojiOnly, estimateBubbleHeight, messageLabel, sameMessageVersion, toolbarPlacement } from "./thread-model";
import { useIsCurrent, useThreadActions, type ValueStore } from "./thread-actions";
import { useOnline } from "./use-online";
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
        <button type="button" className={styles.metaButton} aria-label={`Edited${message.updated_at ? ` ${fullTime(message.updated_at)}` : ""}. Show the earlier versions`}>Edited</button>
      </PopoverTrigger>
      <PopoverContent className={styles.history} side="top" align="end">
        <p className={styles.historyTitle}>Edited{message.updated_at ? ` ${fullTime(message.updated_at)}` : ""}</p>
        {history.length ? (
          <ol className={styles.historyList} aria-label="Earlier versions, newest first">
            {[...history].reverse().map((entry, index) => (
              <li key={index} className={styles.historyItem}>
                <span className={styles.historyWhen}>{index === 0 ? "Before the last edit" : `Version ${history.length - index}`} · from {fullTime(entry.created_at)}</span>
                <span className={styles.historyText}>{plainPreview(entry.message || entry.voice_transcript || (entry.attachments.length ? "Attachment" : ""), 600) || "No text"}</span>
              </li>
            ))}
          </ol>
        ) : <p className={styles.historyQuiet}>DM keeps no earlier text for this message.</p>}
      </PopoverContent>
    </Popover>
  );
}

/** What a message says about itself besides its content (time, Edited, via, ISI, its state), and whether it needs a line. */
function metaOf(message: Message) {
  const isi = isiOf(message.recipient_id);
  const edited = !message.deleted_at && (message.history.length > 0 || !!message.updated_at);
  return { isi, edited, marked: edited || !!message.via_app || !!isi };
}

/**
 * The line under a message: the time and your state under the last of a run; under any other message only what marks
 * it (Edited, via an app, the ISI it went to), so an edit in the middle of a run is never hidden.
 */
function Meta({ message, mine, state, full }: { message: Message; mine: boolean; state: MessageState | null; full: boolean }) {
  const { isi, edited } = metaOf(message);
  return (
    <div className={styles.meta} data-mine={mine ? "" : undefined}>
      {full ? <time dateTime={message.created_at} title={fullTime(message.created_at)} className={styles.time}>{clockTime(message.created_at)}</time> : null}
      {edited ? <Edited message={message} /> : null}
      {message.via_app ? <span className={styles.metaWord}>via {message.via_app}</span> : null}
      {isi ? (
        <span className={styles.metaWord} title={`Routed to the ${isi} intent of ${message.recipient_id.slice(isi.length + 1)}`}>
          → {isi}<span className="sr-only">: routed to the {isi} intent of {message.recipient_id.slice(isi.length + 1)}</span>
        </span>
      ) : null}
      {full && mine && state ? <StatusTick state={state} size={14} /> : null}
    </div>
  );
}

/**
 * While a message's words play (Listen, Space): a quiet line under it; a shimmer while its audio is prepared. A spoken
 * message or a voice note shows this in its own player instead.
 */
function ListenLine({ address, code }: { address: string; code: string }) {
  const listen = useListenState(address, code);
  if (listen.state === "idle") return null;
  return (
    <p className={styles.listenLine} role="status">
      {listen.state === "loading" ? <TextShimmer>Preparing audio…</TextShimmer> : (
        <>
          <span className={styles.listenBars} data-paused={listen.state === "paused" ? "" : undefined} aria-hidden="true"><i /><i /><i /></span>
          {listen.state === "playing" ? "Playing" : "Paused"}
          <span className={styles.listenTrack} aria-hidden="true"><span style={{ transform: `scaleX(${Math.max(0, Math.min(1, listen.progress))})` }} /></span>
        </>
      )}
    </p>
  );
}

function Toolbar({ message, mine, name, showTime }: { message: Message; mine: boolean; name: string; showTime: boolean }) {
  const actions = useThreadActions();
  const voice = useVoiceAvailability();
  const listenable = voice.listen && canListen(message);
  const listen = useListenState(actions.address, message["message-id"]);
  const playing = listen.state === "playing" || listen.state === "loading";
  const self = useRef<HTMLDivElement>(null);
  // Beside the bubble when the column has room on its outer side, else over its top edge at that end (never over the
  // sender's name). Measured before the toolbar is first painted, each time it shows.
  useLayoutEffect(() => {
    const node = self.current;
    const bubble = node?.previousElementSibling;
    const log = node?.closest("[role='log']");
    if (!node || !bubble || !log) return;
    const at = bubble.getBoundingClientRect();
    const within = log.getBoundingClientRect();
    node.dataset.place = toolbarPlacement({ mine, bubble: { left: at.left, right: at.right }, column: { left: within.left, right: within.right }, width: node.offsetWidth });
  });
  const openMenuFromButton = (event: MouseEvent<HTMLButtonElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    actions.openMenu(message, { x: rect.left, y: rect.bottom + 4 });
  };
  return (
    <div ref={self} className={styles.toolbar} data-sq="surface" role="toolbar" aria-label={`Actions for ${name}'s message`}>
      {showTime ? <time className={styles.toolbarTime} dateTime={message.created_at} title={fullTime(message.created_at)}>{clockTime(message.created_at)}</time> : null}
      {actions.readOnly ? null : <IconAction size="sm" label="Reply" shortcut="message.reply" icon={<CornerUpLeft aria-hidden="true" />} onClick={() => actions.reply(message)} tooltipSide="top" />}
      {mine && !actions.readOnly && canEditHere(message) ? <IconAction size="sm" label="Edit" shortcut="message.edit" icon={<Pencil aria-hidden="true" />} onClick={() => actions.edit(message)} tooltipSide="top" /> : null}
      {listenable ? (
        <IconAction size="sm" label={playing ? "Pause" : "Listen"} shortcut="message.listen" icon={playing ? <Pause aria-hidden="true" /> : <Headphones aria-hidden="true" />} onClick={() => actions.listen(message)} tooltipSide="top"
          // Hovering or focusing Listen starts preparing the audio, so pressing it plays at once.
          onPointerEnter={() => warmListen(actions.address, message)} onFocus={() => warmListen(actions.address, message)} />
      ) : null}
      <IconAction size="sm" label="More actions" icon={<Ellipsis aria-hidden="true" />} onClick={openMenuFromButton} tooltipSide="top" aria-haspopup="menu" />
    </div>
  );
}

/**
 * "Delete for everyone?", asked in place under your message. Once answered it morphs into Deleting and Deleted over the
 * tombstone; a message deleted elsewhere (another tab, its custodian) before you answered takes the question away.
 */
const DeleteQuestion = forwardRef<HTMLDivElement, { message: Message; mine: boolean; deleted: boolean; onCancel: () => void }>(function DeleteQuestion({ message, mine, deleted, onCancel }, ref) {
  const actions = useThreadActions();
  const [answered, setAnswered] = useState(false);
  const gone = deleted && !answered;
  useEffect(() => {
    if (gone) actions.cancelDelete();
  }, [gone, actions]);
  if (gone) return null;
  return (
    <div className={styles.confirm} data-mine={mine ? "" : undefined}>
      <ConfirmMorph
        ref={ref}
        label="Delete"
        prompt="Delete for everyone?"
        confirmLabel="Delete"
        pendingLabel="Deleting"
        doneLabel="Deleted"
        defaultState="confirming"
        confirmTimeout={0}
        resultTimeout={1200}
        onConfirm={() => actions.confirmDelete(message)}
        onCancel={() => {
          actions.cancelDelete();
          onCancel();
        }}
        onStateChange={next => {
          if (next === "pending") setAnswered(true);
          if (next === "idle") actions.cancelDelete();
        }}
      />
    </div>
  );
});

/** Focus within a row (the row itself, or a control in it): the row is in use, and its controls join the Tab order. */
function useFocusWithin() {
  const [focused, setFocused] = useState(false);
  const onFocus = () => setFocused(true);
  const onBlur = (event: FocusEvent<HTMLElement>) => {
    if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setFocused(false);
  };
  return { focused, onFocus, onBlur };
}

export interface MessageRowProps extends RowLayout {
  message: Message;
  /** The day it was sent, as its divider names it (said in its accessible name). */
  day: string | null;
  /** A send still in the outbox follows it in the run: it shows its own time and state. */
  stateShown: boolean;
  highlighted: boolean;
  /** The delete question shows on this row. */
  confirming: boolean;
  /** It arrived while the thread was open (it rises in). */
  fresh: boolean;
  /** The row the pointer is over (one store for the thread). */
  hover: ValueStore<string | null>;
}

function sameRow(a: MessageRowProps, b: MessageRowProps): boolean {
  return a.first === b.first && a.last === b.last && a.group === b.group && a.day === b.day && a.stateShown === b.stateShown && a.highlighted === b.highlighted && a.confirming === b.confirming && a.fresh === b.fresh && a.hover === b.hover && sameMessageVersion(a.message, b.message);
}

export const MessageRow = memo(function MessageRow({ message, first, last, group, day, stateShown, highlighted, confirming, fresh, hover }: MessageRowProps) {
  const actions = useThreadActions();
  const row = useRef<HTMLElement>(null);
  const confirmRef = useRef<HTMLDivElement>(null);
  const code = message["message-id"];
  const mine = actions.isMine(message);
  const deleted = !!message.deleted_at;
  const state: MessageState | null = mine && !deleted ? stateOf(message) : null;
  const name = mine ? "You" : accountName(message.sender);
  const hovered = useIsCurrent(hover, code);
  const { focused, onFocus, onBlur } = useFocusWithin();
  const active = (hovered || focused) && !deleted && !confirming;
  const { edited, marked } = metaOf(message);

  const shape = useMemo(() => {
    const media = messageMedia(message).attachments;
    const plain = !message.reply && !message.speech && !message.voice && message.bundle?.role !== "display";
    const emoji = plain && !media.length && !message.gif ? emojiOnly(message.message) : 0;
    const visual = media.length > 0 && media.every(item => item.kind === "image" || item.kind === "video");
    const mediaOnly = plain && !message.message && ((visual && !message.gif) || (!media.length && !!message.gif));
    return { emoji, kind: deleted ? "deleted" : emoji ? "emoji" : mediaOnly ? "media" : "text", height: estimateBubbleHeight(message) };
  }, [message, deleted]);

  // Controls inside join the Tab order only while the row has the keyboard (row-focus.ts).
  useLayoutEffect(() => applyRowTabbing(row.current, focused));

  // The question takes focus when it appears, so Enter or Escape answers it right away (its buttons may only be there
  // a frame later).
  useLayoutEffect(() => {
    if (!confirming) return;
    const button = () => confirmRef.current?.querySelector<HTMLButtonElement>("button:not([disabled])");
    const now = button();
    if (now) {
      now.focus();
      return;
    }
    const frame = requestAnimationFrame(() => button()?.focus());
    return () => cancelAnimationFrame(frame);
  }, [confirming]);

  const openMenu = (event: MouseEvent) => {
    // Links, photos, players, code and selected words keep the browser's own menu (open in a new tab, save the image,
    // copy the selection); the rest of the message opens the message's menu.
    const target = event.target instanceof Element ? event.target : null;
    if (target?.closest("a[href], img, video, audio, canvas, pre") || window.getSelection()?.toString().trim()) return;
    event.preventDefault();
    actions.openMenu(message, { x: event.clientX, y: event.clientY });
  };

  return (
    <article
      ref={row}
      className={styles.message}
      data-mine={mine ? "" : undefined}
      data-first={first ? "" : undefined}
      data-last={last ? "" : undefined}
      data-highlight={highlighted ? "" : undefined}
      data-deleted={deleted ? "" : undefined}
      data-fresh={fresh ? "" : undefined}
      data-active={active ? "" : undefined}
      data-message-row=""
      data-row-key={code}
      data-message-code={code}
      data-conversation={message.conversation_id}
      data-sender-id={message.sender.id}
      data-status={state ?? undefined}
      aria-label={messageLabel({ sender: message.sender, mine, time: clockTime(message.created_at), day, state: state ? STATE_WORDS[state] : null, edited, message })}
      // The log reads others' new messages out; what you sent yourself is never read back to you.
      aria-live={mine && !actions.readOnly ? "off" : undefined}
      tabIndex={-1}
      onContextMenu={openMenu}
      onFocus={onFocus}
      onBlur={onBlur}
    >
      {group && !mine ? (
        <div className={styles.gutter}>
          {last ? (
            <AccountTrigger account={message.sender} className={styles.faceButton} untabbable label={`About ${accountName(message.sender)}`}>
              <AccountAvatar account={message.sender} size="bubble" decorative />
            </AccountTrigger>
          ) : null}
        </div>
      ) : null}
      <div className={styles.stack}>
        {group && !mine && first ? (
          <AccountTrigger account={message.sender} className={styles.sender}>{accountName(message.sender)}</AccountTrigger>
        ) : null}
        <div className={styles.bubbleRow}>
          <div className={styles.bubble} data-sq="surface" data-kind={shape.kind} style={{ containIntrinsicBlockSize: `auto ${shape.height}px` }}>
            <MessageContent message={message} emoji={shape.emoji} />
          </div>
          {active ? <Toolbar message={message} mine={mine} name={name} showTime={!(last || stateShown)} /> : null}
        </div>
        {/* A spoken message and a voice note show their playback in their own player; words played with Listen here. */}
        {canListen(message) && !message.speech && !message.voice ? <ListenLine address={actions.address} code={code} /> : null}
        {last || confirming || marked || stateShown ? <Meta message={message} mine={mine} state={state} full={last || confirming || stateShown} /> : null}
        {confirming ? <DeleteQuestion ref={confirmRef} message={message} mine={mine} deleted={deleted} onCancel={() => row.current?.focus({ preventScroll: true })} /> : null}
      </div>
    </article>
  );
}, sameRow);

/** A send still in the outbox: Waiting (the service does not have it yet) or Failed (with the service's words). */
export const PendingRow = memo(function PendingRow({ item, first, last, fresh }: { item: OutboxItem; fresh: boolean } & RowLayout) {
  const actions = useThreadActions();
  const online = useOnline();
  const row = useRef<HTMLElement>(null);
  const { focused, onFocus, onBlur } = useFocusWithin();
  const state: MessageState = item.status === "failed" ? "failed" : "waiting";
  const at = new Date(item.createdAt).toISOString();
  const text = item.body.message ?? "";
  const attachments = item.body.attachments?.length ?? 0;
  const emoji = !item.replyPreview && !attachments && !item.body.gif && !item.body.voice ? emojiOnly(text) : 0;
  useLayoutEffect(() => applyRowTabbing(row.current, focused || state === "failed"));
  const words = [text ? plainPreview(text, 140) : null, attachments ? (attachments === 1 ? "1 attachment" : `${attachments} attachments`) : null, item.body.gif ? "GIF" : null, item.body.voice ? "Voice message" : null].filter(Boolean).join(", ");
  return (
    <article
      ref={row}
      className={styles.message}
      data-mine=""
      data-first={first ? "" : undefined}
      data-last={last ? "" : undefined}
      data-fresh={fresh ? "" : undefined}
      data-pending={state}
      data-pending-key={item.key}
      data-message-row=""
      data-row-key={item.key}
      data-status={state}
      data-conversation={item.address}
      aria-label={`Your message at ${clockTime(at)}, ${STATE_WORDS[state]}${words ? `: ${words}` : ""}`}
      // Not read out as it is added (you just wrote it); a failed send speaks through its alert.
      aria-live="off"
      tabIndex={-1}
      onFocus={onFocus}
      onBlur={onBlur}
    >
      <div className={styles.stack}>
        <div className={styles.bubbleRow}>
          <div className={styles.bubble} data-sq="surface" data-kind={emoji ? "emoji" : "text"}>
            {item.replyPreview ? <ReplyQuote reply={item.replyPreview} interactive={false} /> : null}
            {text ? (emoji ? <p className={styles.emoji} data-count={emoji}>{text.trim()}</p> : <RichText text={text} />) : null}
            {!text && words ? <p className={styles.tombstone}>{words}</p> : null}
          </div>
        </div>
        {/* Waiting sends in a row say it once, under the last (each bubble is faded, and its name says Waiting). */}
        {last || state === "failed" ? (
          <div className={styles.meta} data-mine="">
            <time dateTime={at} className={styles.time}>{clockTime(at)}</time>
            <StatusTick state={state} size={14} />
            {state === "waiting" && (!online || item.attempts) ? <span className={styles.metaWord}>{online ? "Trying again" : "Waiting for the network"}</span> : null}
          </div>
        ) : null}
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
