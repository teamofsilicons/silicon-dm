"use client";

/**
 * Delivered and read receipts from this tab.
 *
 * - Read: messages from others that enter the viewport while the tab is visible and focused are read. The newest one
 *   seen is sent as `receipts.read up_to` (spec §4.5), at most once a second per conversation. A service without the
 *   bulk route answers 404 once; from then on each unread message gets its own receipt (the older route).
 * - Delivered: a message from someone else that this tab fetched (the sync loop) is delivered, batched the same way.
 *   A read covers it (read implies delivered), so a delivered receipt waits a moment for a read of the same message
 *   and is not sent when the read is: a message that arrives in the open conversation costs one call, not two
 *   (receipt-plan.ts flushPlan).
 * - One bulk call marks at most 5,000 messages, the newest first: while an answer says it left older ones, the call is
 *   made again, a bounded number of times (receipt-plan.ts leftOlder).
 * - A failure that passes (offline, the service busy, away or restarting: 0, 408, 429, 5xx) is tried again after the
 *   service's Retry-After, else after 2 s, 4 s … up to a minute, a few times; a refusal is not tried again until a newer
 *   message is seen. Before, a slow failure was retried at once, and a service that was struggling heard from every
 *   tab every second.
 * - A custodian's view never sends receipts: it is read only (the service would answer 403 `read_only_view`).
 *
 * After a read, the conversation's row takes the unread count the service answered, here and in this browser's other
 * tabs (lib/chat/news.ts announceRead).
 */
import type { QueryClient } from "@tanstack/react-query";
import { ApiError } from "@/lib/errors";
import { receiptsUpTo, recordReceipt } from "./api";
import { applyReadToSummaries, messagesOldestFirst, type ConversationPages, type MessagePages } from "./cache";
import { compareCodes, isAfter, newerCode } from "./codes";
import { sameAccount } from "./address";
import { chatKeys } from "./keys";
import { announceRead } from "./news";
import { deviceId } from "./presence";
import { BULK_CALLS_PER_FLUSH, flushPlan, leftOlder, passingFailure, RECEIPT_ATTEMPTS, receiptRetryDelay, type ReceiptKind } from "./receipt-plan";
import { applyMessage } from "./sync";
import type { AccountRef, ConversationSummary, ReceiptsResult } from "./types";

const FLUSH_MS = 1000;
/** How long a delivered receipt waits for a read of the same messages to join it (the row renders, then is seen). */
const DELIVERED_WAIT_MS = 400;
const SINGLE_RECEIPTS_PER_FLUSH = 30;

/** One conversation's receipts from this tab. */
interface Pending {
  /** The newest code waiting to be marked read, and delivered. */
  read: string | null;
  delivered: string | null;
  /** The newest codes already marked, so nothing goes twice (a read marks delivered too). */
  sentRead: string | null;
  sentDelivered: string | null;
  timer: ReturnType<typeof setTimeout> | null;
  lastFlush: number;
  /** After a failure that passes: nothing is sent before this time. */
  retryAt: number;
  failures: number;
  /** The flush on its way, if any. */
  flushing: Promise<void> | null;
}

let context: { queryClient: QueryClient; me: AccountRef } | null = null;
/** False once the service answered 404 to the bulk route (an older DM): single receipts from then on. */
let bulkSupported = true;
const pending = new Map<string, Pending>();

export function attachReceipts(options: { queryClient: QueryClient; me: AccountRef }): () => void {
  context = options;
  const onFocus = () => {
    for (const [address, entry] of pending) {
      if (!entry.read) continue;
      // Back in view: one more try for a conversation that gave up (its retry wait still holds).
      if (entry.failures >= RECEIPT_ATTEMPTS) entry.failures = RECEIPT_ATTEMPTS - 1;
      schedule(address, entry);
    }
  };
  window.addEventListener("focus", onFocus);
  document.addEventListener("visibilitychange", onFocus);
  return () => {
    window.removeEventListener("focus", onFocus);
    document.removeEventListener("visibilitychange", onFocus);
    if (context === options) context = null;
  };
}

const attentive = () => typeof document !== "undefined" && document.visibilityState === "visible" && document.hasFocus();

function entryFor(address: string): Pending {
  let entry = pending.get(address);
  if (!entry) {
    entry = { read: null, delivered: null, sentRead: null, sentDelivered: null, timer: null, lastFlush: 0, retryAt: 0, failures: 0, flushing: null };
    pending.set(address, entry);
  }
  return entry;
}

function schedule(address: string, entry: Pending) {
  if (entry.timer || entry.flushing || (!entry.read && !entry.delivered)) return;
  // Too many failures in a row: wait for the next message seen (note) or the next focus to start again.
  if (entry.failures >= RECEIPT_ATTEMPTS) return;
  const now = Date.now();
  let wait = Math.max(0, entry.lastFlush + FLUSH_MS - now, entry.retryAt - now);
  // Only a delivered receipt waiting: give a read of the same messages a moment to join it.
  if (!entry.read) wait = Math.max(wait, DELIVERED_WAIT_MS);
  entry.timer = setTimeout(() => {
    entry.timer = null;
    void flush(address, entry);
  }, wait);
}

/** Marks messages up to `code` (from others) as read: they were on screen in a focused tab. */
export function noteRead(address: string, code: string): void {
  note("read", address, code);
}

/** Marks a message from someone else as delivered to this browser. */
export function noteDelivered(address: string, code: string): void {
  note("delivered", address, code);
}

function note(kind: ReceiptKind, address: string, code: string) {
  if (!context) return;
  const entry = entryFor(address);
  if (kind === "read") {
    if (!isAfter(code, entry.sentRead)) return;
    entry.read = newerCode(entry.read, code);
  } else {
    // A message read already was delivered too.
    if (!isAfter(code, entry.sentDelivered) || !isAfter(code, entry.sentRead)) return;
    entry.delivered = newerCode(entry.delivered, code);
  }
  // A newer message seen starts a conversation that gave up (RECEIPT_ATTEMPTS) over again; its retry wait still holds.
  if (entry.failures >= RECEIPT_ATTEMPTS) entry.failures = RECEIPT_ATTEMPTS - 1;
  schedule(address, entry);
}

/** Sends what is waiting for one conversation. `asked`: the Carbon asked (Mark everything read), so focus does not matter. */
function flush(address: string, entry: Pending, asked = false): Promise<void> {
  if (entry.flushing) return entry.flushing;
  if (!context || (!entry.read && !entry.delivered)) return Promise.resolve();
  const plan = flushPlan(entry, asked || attentive());
  // What a read covers is not sent on its own (read implies delivered).
  if (entry.delivered && !plan.delivered) entry.delivered = null;
  if (!plan.read && !plan.delivered) return Promise.resolve(); // A read waits for focus (attachReceipts).
  entry.lastFlush = Date.now();
  if (plan.read) entry.read = null;
  if (plan.delivered) entry.delivered = null;
  const work = (async () => {
    // A failure that may pass: everything still to go waits for the retry.
    let retry: ApiError | null = null;
    const kinds = (["read", "delivered"] as const).filter(kind => plan[kind]);
    for (const [index, kind] of kinds.entries()) {
      const upTo = plan[kind]!;
      try {
        await mark(kind, address, upTo);
        if (kind === "read") {
          entry.sentRead = newerCode(entry.sentRead, upTo);
          // Read implies delivered: a delivered receipt for these messages has nothing left to say.
          entry.sentDelivered = newerCode(entry.sentDelivered, upTo);
          if (entry.delivered && compareCodes(entry.delivered, upTo) <= 0) entry.delivered = null;
        } else {
          entry.sentDelivered = newerCode(entry.sentDelivered, upTo);
        }
      } catch (error) {
        const reason = ApiError.from(error);
        if (!passingFailure(reason.status)) continue; // A refusal waits for a newer message.
        // It may pass: this one and what was still to go wait for the retry (with whatever newer arrives meanwhile).
        retry = reason;
        for (const later of kinds.slice(index)) {
          if (later === "read") entry.read = newerCode(entry.read, plan.read);
          else entry.delivered = newerCode(entry.delivered, plan.delivered);
        }
        break;
      }
    }
    if (retry) {
      entry.failures += 1;
      entry.retryAt = Date.now() + receiptRetryDelay(entry.failures, retry.retryAfter);
    } else {
      // The service answered (a refusal too): nothing to wait for.
      entry.failures = 0;
      entry.retryAt = 0;
    }
  })().finally(() => {
    entry.flushing = null;
    schedule(address, entry);
  });
  entry.flushing = work;
  return work;
}

/** Marks one kind up to a code: the bulk route (again while it leaves older messages), or single receipts on an older service. */
async function mark(kind: ReceiptKind, address: string, upTo: string): Promise<void> {
  if (bulkSupported) {
    try {
      let result: ReceiptsResult = await receiptsUpTo(address, upTo, kind);
      for (let calls = 1; calls < BULK_CALLS_PER_FLUSH && leftOlder(kind, result); calls++) result = await receiptsUpTo(address, upTo, kind);
      if (kind === "read" && context) {
        context.queryClient.setQueryData<ConversationPages>(chatKeys.conversations(), data => applyReadToSummaries(data, address, result));
        // This browser's other tabs take the same count at once (lib/chat/news.ts).
        announceRead(context.me.uuid ?? context.me.id, address, result);
      }
      return;
    } catch (error) {
      const failure = ApiError.from(error);
      if (failure.status !== 404 || failure.code === "conversation_not_found" || failure.code === "message_not_found") throw error;
      bulkSupported = false;
    }
  }
  await singleReceipts(kind, address, upTo);
}

/**
 * Marks every given conversation read up to its last message (⇧Esc, "Mark everything read"). Asked for by the Carbon,
 * so it does not wait for focus or a retry's wait. Resolves with how many conversations were marked.
 */
export async function markAllRead(conversations: ConversationSummary[]): Promise<number> {
  if (!context) return 0;
  let marked = 0;
  for (const conversation of conversations) {
    const last = conversation.last_message;
    if (!last) continue;
    const entry = entryFor(conversation.id);
    // One on its way first, so nothing goes twice.
    if (entry.flushing) await entry.flushing;
    entry.read = newerCode(entry.read, last["message-id"]);
    if (entry.timer) clearTimeout(entry.timer);
    entry.timer = null;
    entry.lastFlush = 0;
    entry.retryAt = 0;
    entry.failures = 0;
    await flush(conversation.id, entry, true);
    if (!entry.read && !isAfter(last["message-id"], entry.sentRead)) marked += 1;
  }
  return marked;
}

/** The older service: one receipt per message from others, up to `upTo`, that is not yet read (or delivered). */
async function singleReceipts(kind: ReceiptKind, address: string, upTo: string) {
  if (!context) return;
  const { queryClient, me } = context;
  // Delivered: each arriving message is noted as it comes, so the newest one is all that is left to say.
  const codes = kind === "delivered"
    ? [upTo]
    : messagesOldestFirst(queryClient.getQueryData<MessagePages>(chatKeys.messages(address)))
        .filter(message => !sameAccount(message.sender, me) && !message.deleted_at && !message.read_at && compareCodes(message["message-id"], upTo) <= 0)
        .slice(-SINGLE_RECEIPTS_PER_FLUSH)
        .map(message => message["message-id"]);
  for (const code of codes) {
    const updated = await recordReceipt(address, code, kind, deviceId());
    applyMessage(queryClient, me, updated);
  }
  if (kind === "read") queryClient.setQueryData<ConversationPages>(chatKeys.conversations(), data => {
    const row = data?.pages.flatMap(page => page.items).find(item => item.id === address);
    return row && typeof row.unread_count === "number" ? applyReadToSummaries(data, address, { unread_count: 0 }) : data;
  });
}
