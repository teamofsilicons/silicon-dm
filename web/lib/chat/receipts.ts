"use client";

/**
 * Delivered and read receipts from this tab.
 *
 * - Read: messages from others that enter the viewport while the tab is visible and focused are read. The newest one
 *   seen is sent as `receipts.read up_to` (spec §4.5), at most once a second per conversation. A service without the
 *   bulk route answers 404 once; from then on each unread message gets its own receipt (the older route).
 * - Delivered: a message from someone else that this tab fetched (the sync loop) is delivered, batched the same way.
 * - A custodian's view never sends receipts: it is read only (the service would answer 403 `read_only_view`).
 *
 * After a read, the conversation's row takes the unread count the service answered.
 */
import type { QueryClient } from "@tanstack/react-query";
import { ApiError } from "@/lib/errors";
import { receiptsUpTo, recordReceipt } from "./api";
import { applyReadToSummaries, messagesOldestFirst, type ConversationPages, type MessagePages } from "./cache";
import { compareCodes, isAfter, newerCode } from "./codes";
import { sameAccount } from "./address";
import { chatKeys } from "./keys";
import { deviceId } from "./presence";
import { applyMessage } from "./sync";
import type { AccountRef, ConversationSummary } from "./types";

const FLUSH_MS = 1000;
const SINGLE_RECEIPTS_PER_FLUSH = 30;

type Kind = "read" | "delivered";

interface Pending {
  upTo: string | null;
  timer: ReturnType<typeof setTimeout> | null;
  lastFlush: number;
  /** The newest code already sent for this kind, so nothing goes twice. */
  sent: string | null;
  flushing: boolean;
}

let context: { queryClient: QueryClient; me: AccountRef } | null = null;
/** False once the service answered 404 to the bulk route (an older DM): single receipts from then on. */
let bulkSupported = true;
const pending = new Map<string, Pending>();

export function attachReceipts(options: { queryClient: QueryClient; me: AccountRef }): () => void {
  context = options;
  const onFocus = () => {
    for (const [key, entry] of pending) if (key.startsWith("read\u0000") && entry.upTo) schedule(key, entry);
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

function entryFor(key: string): Pending {
  let entry = pending.get(key);
  if (!entry) {
    entry = { upTo: null, timer: null, lastFlush: 0, sent: null, flushing: false };
    pending.set(key, entry);
  }
  return entry;
}

function schedule(key: string, entry: Pending) {
  if (entry.timer || entry.flushing) return;
  const wait = Math.max(0, entry.lastFlush + FLUSH_MS - Date.now());
  entry.timer = setTimeout(() => {
    entry.timer = null;
    void flush(key, entry);
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

function note(kind: Kind, address: string, code: string) {
  if (!context) return;
  const key = `${kind}\u0000${address}`;
  const entry = entryFor(key);
  if (!isAfter(code, entry.sent)) return;
  entry.upTo = newerCode(entry.upTo, code);
  schedule(key, entry);
}

async function flush(key: string, entry: Pending, asked = false): Promise<void> {
  const [kind, address] = key.split("\u0000") as [Kind, string];
  const upTo = entry.upTo;
  if (!context || !upTo) return;
  if (kind === "read" && !asked && !attentive()) return; // Waits for focus (attachReceipts).
  entry.flushing = true;
  entry.lastFlush = Date.now();
  entry.upTo = null;
  try {
    if (bulkSupported) {
      try {
        const result = await receiptsUpTo(address, upTo, kind);
        entry.sent = newerCode(entry.sent, upTo);
        if (kind === "read") context.queryClient.setQueryData<ConversationPages>(chatKeys.conversations(), data => applyReadToSummaries(data, address, result));
        return;
      } catch (error) {
        const failure = ApiError.from(error);
        if (failure.status !== 404 || failure.code === "conversation_not_found" || failure.code === "message_not_found") throw error;
        bulkSupported = false;
      }
    }
    await singleReceipts(kind, address, upTo);
    entry.sent = newerCode(entry.sent, upTo);
  } catch {
    // Try again with the next message seen (or the next focus); receipts never move backwards, so nothing is lost.
    entry.upTo = newerCode(entry.upTo, upTo);
  } finally {
    entry.flushing = false;
    if (entry.upTo) schedule(key, entry);
  }
}

/**
 * Marks every given conversation read up to its last message (⇧Esc, "Mark everything read"). Asked for by the Carbon,
 * so it does not wait for focus. Resolves with how many conversations were marked.
 */
export async function markAllRead(conversations: ConversationSummary[]): Promise<number> {
  if (!context) return 0;
  let marked = 0;
  for (const conversation of conversations) {
    const last = conversation.last_message;
    if (!last) continue;
    const key = `read\u0000${conversation.id}`;
    const entry = entryFor(key);
    entry.upTo = newerCode(entry.upTo, last["message-id"]);
    if (entry.timer) clearTimeout(entry.timer);
    entry.timer = null;
    entry.lastFlush = 0;
    await flush(key, entry, true);
    if (!entry.upTo) marked += 1;
  }
  return marked;
}

/** The older service: one receipt per message from others, up to `upTo`, that is not yet read (or delivered). */
async function singleReceipts(kind: Kind, address: string, upTo: string) {
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
