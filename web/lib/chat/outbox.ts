/**
 * The outbox: every send is written here first, shows at once as Waiting, and leaves only when the service has
 * durably accepted it (UNDERSTANDING: "in no possible way will the message ever be lost"). This file is the pure state
 * machine; lib/chat/outbox-store.ts keeps it in localStorage per account and runs the sends.
 *
 *   enqueue ──▶ waiting ──start──▶ sending ──succeed──▶ (gone: the message is in the thread)
 *                  ▲                  │
 *                  └──── fail (retryable, attempts < 5, or offline) ◀┘
 *                                     │
 *                                     └─ fail (refused, or the 5th attempt) ──▶ failed ──retry──▶ waiting
 *                                                                                  └──discard──▶ (gone)
 *
 * Every attempt of one send uses the same Idempotency-Key, so a retry after a lost answer returns the original
 * message instead of sending it twice. Attempts made while the browser is offline do not count: the send waits for
 * `online` instead.
 */
import type { MessageCreate, Reply } from "./types";

export type OutboxStatus = "waiting" | "sending" | "failed";

export interface OutboxError {
  status: number;
  code: string;
  message: string;
  hint?: string;
}

export interface OutboxItem {
  /** The Idempotency-Key of every attempt, and the item's identity. */
  key: string;
  /** The conversation's address. */
  address: string;
  body: MessageCreate;
  /** What the optimistic bubble shows of the message replied to. */
  replyPreview?: Reply | null;
  /** When it was written (epoch ms): the optimistic bubble's time and order. */
  createdAt: number;
  status: OutboxStatus;
  /** Attempts that reached the network and failed. */
  attempts: number;
  /** When the next automatic attempt may start (epoch ms). */
  nextAttemptAt: number;
  error?: OutboxError;
}

/** Automatic attempts while online before a send is Failed (spec §6.5). */
export const MAX_ATTEMPTS = 5;
const BASE_DELAY_MS = 1000;
const MAX_DELAY_MS = 30_000;

/** The wait before attempt `attempts + 1`: 1 s, 2 s, 4 s, 8 s, 16 s (capped), with up to 20 % jitter. */
export function backoff(attempts: number, random: () => number = Math.random): number {
  const base = Math.min(MAX_DELAY_MS, BASE_DELAY_MS * 2 ** Math.max(0, attempts - 1));
  return Math.round(base * (1 + random() * 0.2));
}

/**
 * Whether trying again can help: the network, a timeout, the service having a moment (5xx), or being told to slow
 * down (429). Anything else the service refused (400, 403, 404, 409, 413, 422…) stays refused, so it fails at once.
 */
export function isRetryable(error: OutboxError): boolean {
  if (error.status === 0) return error.code === "network_error" || error.code === "aborted";
  return error.status === 408 || error.status === 429 || error.status >= 500;
}

export function enqueue(items: OutboxItem[], input: { key: string; address: string; body: MessageCreate; replyPreview?: Reply | null; now: number }): OutboxItem[] {
  if (items.some(item => item.key === input.key)) return items;
  return [...items, { key: input.key, address: input.address, body: input.body, replyPreview: input.replyPreview ?? null, createdAt: input.now, status: "waiting", attempts: 0, nextAttemptAt: input.now }];
}

/** The items whose next attempt may start now, oldest first (one at a time per conversation keeps the order). */
export function due(items: OutboxItem[], now: number, online: boolean): OutboxItem[] {
  if (!online) return [];
  const busy = new Set(items.filter(item => item.status === "sending").map(item => item.address));
  const out: OutboxItem[] = [];
  for (const item of [...items].sort((a, b) => a.createdAt - b.createdAt)) {
    if (item.status !== "waiting" || busy.has(item.address)) continue;
    // An earlier send of the same conversation that is still waiting goes first.
    busy.add(item.address);
    if (item.nextAttemptAt <= now) out.push(item);
  }
  return out;
}

const update = (items: OutboxItem[], key: string, change: (item: OutboxItem) => OutboxItem | null): OutboxItem[] => {
  const out: OutboxItem[] = [];
  for (const item of items) {
    if (item.key !== key) {
      out.push(item);
      continue;
    }
    const next = change(item);
    if (next) out.push(next);
  }
  return out;
};

export const start = (items: OutboxItem[], key: string): OutboxItem[] => update(items, key, item => (item.status === "waiting" ? { ...item, status: "sending" } : item));

/** The service accepted it: it leaves the outbox (the caller puts the returned message into the thread). */
export const succeed = (items: OutboxItem[], key: string): OutboxItem[] => update(items, key, () => null);

/** An attempt failed: wait and try again, or stop at Failed with the service's words. */
export function fail(items: OutboxItem[], key: string, error: OutboxError, now: number, online: boolean, random?: () => number): OutboxItem[] {
  return update(items, key, item => {
    if (!online && error.status === 0) return { ...item, status: "waiting", nextAttemptAt: now, error };
    const attempts = item.attempts + 1;
    if (isRetryable(error) && attempts < MAX_ATTEMPTS) return { ...item, status: "waiting", attempts, nextAttemptAt: now + backoff(attempts, random), error };
    return { ...item, status: "failed", attempts, error };
  });
}

/** Retry (the Carbon pressed Retry): back to waiting, with fresh attempts and the same Idempotency-Key. */
export const retry = (items: OutboxItem[], key: string, now: number): OutboxItem[] =>
  update(items, key, item => (item.status === "failed" ? { ...item, status: "waiting", attempts: 0, nextAttemptAt: now, error: undefined } : item));

export const discard = (items: OutboxItem[], key: string): OutboxItem[] => update(items, key, () => null);

/** The browser came back online: waiting sends try again now instead of after their backoff. */
export const resume = (items: OutboxItem[], now: number): OutboxItem[] =>
  items.map(item => (item.status === "waiting" && item.nextAttemptAt > now ? { ...item, nextAttemptAt: now } : item));

/**
 * Items read back from storage: a send that was in flight when the page went away may or may not have arrived, so it
 * waits to be sent again with the same key (the service answers a duplicate with the original message).
 */
export function restore(items: unknown): OutboxItem[] {
  if (!Array.isArray(items)) return [];
  return items
    .filter((item): item is OutboxItem => !!item && typeof item === "object" && typeof (item as OutboxItem).key === "string" && typeof (item as OutboxItem).address === "string" && !!(item as OutboxItem).body)
    .map(item => (item.status === "sending" ? { ...item, status: "waiting" as const } : item));
}

/** When the soonest waiting item may start (for a timer), or null when nothing waits. */
export function nextWake(items: OutboxItem[]): number | null {
  let soonest: number | null = null;
  for (const item of items) if (item.status === "waiting" && (soonest === null || item.nextAttemptAt < soonest)) soonest = item.nextAttemptAt;
  return soonest;
}
