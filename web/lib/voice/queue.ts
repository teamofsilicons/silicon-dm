/**
 * Listen mode's queue (spec §6.6), pure: which new messages of the open conversation are spoken, in what order, and
 * when each may start.
 *
 * - Only what arrives after Listen mode was turned on: the newest code at that moment is the mark, and nothing at or
 *   before it is ever spoken (old messages, and the backlog that loads with the page, stay quiet).
 * - Each message after the mark is looked at once, whatever order messages land in: the queue keeps the codes it has
 *   seen (queued, spoken, or not for speaking). A newest-code-seen mark lost a message that landed after a newer one
 *   (a slower fetch, a page applied late): it was below the mark before it was ever seen. Past SEEN_LIMIT codes the
 *   oldest are folded into the mark.
 * - Only Silicons' messages with something to say (words, or a spoken message), never the Carbon's own (in a
 *   custodian's view, never the viewed Silicon's), never a deleted one. "Play spoken messages when they arrive"
 *   (Settings → Listening) uses the same queue with the `speech` filter: spoken messages only.
 * - In code order. A spoken message waits at the head until the service has rendered it (status `ready`); a failed
 *   render, or one still pending after PENDING_GRACE_MS, is tried anyway (the speech route renders on demand).
 */
import { sameAccount } from "@/lib/chat/address";
import { compareCodes } from "@/lib/chat/codes";
import type { AccountRef, Message } from "@/lib/chat/types";

export type QueueFilter = "all" | "speech";

export interface QueueItem {
  code: string;
  /** When it joined the queue (ms), for the pending grace. */
  since: number;
}

export interface ListenQueue {
  /** False until the conversation's messages were seen once (the mark is set from them). */
  started: boolean;
  /** The newest code when the queue started (later, the oldest codes folded in): nothing at or before it is new. */
  mark: string | null;
  /** Codes after the mark already looked at, oldest first: each is queued (or passed over) once. */
  seen: string[];
  waiting: QueueItem[];
  /** The code being spoken now, or null. */
  current: string | null;
}

/** How many seen codes the queue keeps before the oldest are folded into the mark. */
export const SEEN_LIMIT = 2000;

/** How long a spoken message may stay `pending` at the head before it is tried anyway. */
export const PENDING_GRACE_MS = 12_000;

export const EMPTY_QUEUE: ListenQueue = { started: false, mark: null, seen: [], waiting: [], current: null };

/** Whether Listen mode (or autoplay, with `speech`) would speak this message for `perspective`. */
export function speakable(message: Message, perspective: Pick<AccountRef, "id" | "uuid">, filter: QueueFilter = "all"): boolean {
  if (message.deleted_at || message.sender.type !== "silicon" || sameAccount(message.sender, perspective)) return false;
  if (message.speech?.script) return true;
  if (filter === "speech") return false;
  return !!message.message?.trim();
}

function newest(messages: readonly Message[]): string | null {
  let mark: string | null = null;
  for (const message of messages) if (!mark || compareCodes(message["message-id"], mark) > 0) mark = message["message-id"];
  return mark;
}

/** The queue as Listen mode starts: the conversation as it is now is the past. */
export function startQueue(messages: readonly Message[]): ListenQueue {
  return { started: true, mark: newest(messages), seen: [], waiting: [], current: null };
}

/**
 * The messages as the cache has them now (any order). Each one after the mark that was not seen before is looked at
 * once: a speakable one joins the queue in code order, whatever order they landed in; deleted ones leave it. Before the
 * queue started, this starts it (nothing is spoken).
 */
export function observeMessages(queue: ListenQueue, messages: readonly Message[], perspective: Pick<AccountRef, "id" | "uuid">, filter: QueueFilter = "all", now = Date.now()): ListenQueue {
  if (!queue.started) return startQueue(messages);
  const byCode = new Map(messages.map(message => [message["message-id"], message]));
  let waiting = queue.waiting.filter(item => {
    const message = byCode.get(item.code);
    return !message || speakable(message, perspective, filter);
  });
  const seen = new Set(queue.seen);
  const unseen = messages.filter(message => (!queue.mark || compareCodes(message["message-id"], queue.mark) > 0) && !seen.has(message["message-id"]));
  const known = new Set(waiting.map(item => item.code));
  if (queue.current) known.add(queue.current);
  const fresh = unseen
    .filter(message => !known.has(message["message-id"]) && speakable(message, perspective, filter))
    .map(message => ({ code: message["message-id"], since: now }));
  if (fresh.length) waiting = [...waiting, ...fresh].sort((a, b) => compareCodes(a.code, b.code));
  if (!unseen.length && waiting.length === queue.waiting.length && waiting.every((item, index) => item === queue.waiting[index])) return queue;
  let mark = queue.mark;
  let codes = queue.seen;
  if (unseen.length) {
    codes = [...queue.seen, ...unseen.map(message => message["message-id"])].sort(compareCodes);
    // Too many to keep: the oldest fold into the mark (what is at or before it is never new again).
    if (codes.length > SEEN_LIMIT) {
      mark = codes[codes.length - SEEN_LIMIT - 1]!;
      codes = codes.slice(codes.length - SEEN_LIMIT);
    }
  }
  return { ...queue, mark, seen: codes, waiting };
}

/** Whether a message is ready to be spoken now (a pending render waits, up to the grace). */
export function readyToSpeak(message: Message | undefined, item: QueueItem, now = Date.now()): boolean {
  if (!message) return false;
  if (message.speech && message.speech.status === "pending") return now - item.since >= PENDING_GRACE_MS;
  return true;
}

/** The message to speak next, or null (nothing waiting, something speaking, or the head still rendering). */
export function nextToSpeak(queue: ListenQueue, messages: ReadonlyMap<string, Message>, now = Date.now()): Message | null {
  if (queue.current) return null;
  const head = queue.waiting[0];
  if (!head) return null;
  const message = messages.get(head.code);
  return readyToSpeak(message, head, now) ? message ?? null : null;
}

/** When the head (still rendering) may be tried anyway, or null. */
export function headDeadline(queue: ListenQueue, messages: ReadonlyMap<string, Message>): number | null {
  const head = queue.waiting[0];
  if (!head || queue.current) return null;
  const message = messages.get(head.code);
  return message?.speech?.status === "pending" ? head.since + PENDING_GRACE_MS : null;
}

export function beginSpeaking(queue: ListenQueue, code: string): ListenQueue {
  return { ...queue, current: code, waiting: queue.waiting.filter(item => item.code !== code) };
}

export function finishSpeaking(queue: ListenQueue, code: string): ListenQueue {
  return queue.current === code ? { ...queue, current: null } : queue;
}

/**
 * The message about to be spoken goes back to the head of the line (something else started playing first); it has
 * waited already, so a spoken message still rendering is tried as soon as it is its turn again.
 */
export function returnToQueue(queue: ListenQueue, code: string): ListenQueue {
  if (queue.current !== code) return queue;
  return { ...queue, current: null, waiting: [{ code, since: 0 }, ...queue.waiting.filter(item => item.code !== code)] };
}

/** Stop: nothing waiting or speaking; what arrives next is still spoken. */
export function clearQueue(queue: ListenQueue): ListenQueue {
  return { ...queue, waiting: [], current: null };
}
