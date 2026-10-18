/**
 * The pure part of lib/chat/receipts.ts: what one flush of a conversation's receipts sends, whether a bulk answer left
 * older messages to mark, and how long a receipt that failed waits before it is tried again. Tested in
 * tests/chat-receipts.test.ts.
 */
import { compareCodes } from "./codes";
import type { ReceiptsResult } from "./types";

export type ReceiptKind = "read" | "delivered";

/**
 * The most messages one bulk receipt marks (docs/reference/limits.md), the newest first. An answer that marked fewer
 * marked everything up to its code; one that marked this many may have left older ones.
 */
export const BULK_RECEIPTS_PER_CALL = 5000;

/** The most bulk calls one flush makes for one kind (so at most 100,000 messages at once; the next flush goes on). */
export const BULK_CALLS_PER_FLUSH = 20;

/** Failures in a row before a conversation's receipts stop trying by themselves (the next message seen, or focus, tries again). */
export const RECEIPT_ATTEMPTS = 6;

/**
 * What a flush sends: read up to the newest message seen when it may (a focused tab, or the Carbon asked), and delivered
 * only when that read does not already cover it: a read receipt marks the messages delivered too, so a message that
 * arrived and was read in the same second costs one call, not two.
 */
export function flushPlan(pending: { read: string | null; delivered: string | null }, mayRead: boolean): { read: string | null; delivered: string | null } {
  const read = mayRead ? pending.read : null;
  const delivered = pending.delivered && !(read && compareCodes(pending.delivered, read) <= 0) ? pending.delivered : null;
  return { read, delivered };
}

/**
 * Whether a bulk receipt's answer left older messages unmarked, so the same call should be made again: it marked as
 * many as one call marks (BULK_RECEIPTS_PER_CALL), and for a read, the service still counts some unread. The count
 * alone does not say it: `unread_count` also counts what arrived after the code (not on screen yet), and asking again
 * for those marks nothing.
 */
export function leftOlder(kind: ReceiptKind, result: Pick<ReceiptsResult, "updated" | "unread_count">): boolean {
  if (!(result.updated >= BULK_RECEIPTS_PER_CALL)) return false;
  return kind === "delivered" || result.unread_count > 0;
}

/** A failure that passes with time (no connection, the service busy, away or restarting), not one about the receipt. */
export function passingFailure(status: number): boolean {
  return status === 0 || status === 408 || status === 429 || status >= 500;
}

/**
 * How long a receipt that failed for a reason that passes waits before it is tried again: the service's Retry-After
 * when it gave one (at least a second, at most five minutes), else 2 s, 4 s, 8 s … up to a minute, with up to 25 %
 * jitter so the tabs of a browser and the browsers of a group do not all come back at once.
 */
export function receiptRetryDelay(failures: number, retryAfterSeconds: number | null | undefined, random: () => number = Math.random): number {
  if (typeof retryAfterSeconds === "number" && Number.isFinite(retryAfterSeconds) && retryAfterSeconds >= 0) {
    return Math.min(300_000, Math.max(1000, Math.round(retryAfterSeconds * 1000)));
  }
  const base = Math.min(60_000, 2000 * 2 ** Math.max(0, failures - 1));
  return Math.round(base * (1 + random() * 0.25));
}
