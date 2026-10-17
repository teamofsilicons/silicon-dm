"use client";

/**
 * The outbox at run time (the state machine is lib/chat/outbox.ts): kept in localStorage per account so a send
 * survives a reload, a crash or going offline, sent one conversation at a time in order, retried with its own
 * Idempotency-Key, and shown in the thread as Waiting or Failed until the service has it.
 *
 * Tabs share the stored outbox. A send takes a Web Lock named after its key first, so two tabs never send the same
 * item at the same moment (and if they did, the service would answer the duplicate with the original message).
 */
import { useMemo, useSyncExternalStore } from "react";
import type { QueryClient } from "@tanstack/react-query";
import { ApiError } from "@/lib/errors";
import { sendMessage } from "./api";
import { discard, due, enqueue, fail, nextWake, restore, resume, retry, start, succeed, type OutboxItem } from "./outbox";
import { applyMessage } from "./sync";
import { canStore, onStorage, readJson, writeJson } from "./storage";
import type { AccountRef, MessageCreate, Reply } from "./types";

const keyFor = (account: string) => `dm:outbox:v1:${account}`;
const EMPTY: OutboxItem[] = [];

let items: OutboxItem[] = EMPTY;
let account: string | null = null;
let context: { queryClient: QueryClient; me: AccountRef } | null = null;
let timer: ReturnType<typeof setTimeout> | null = null;
const listeners = new Set<() => void>();
const inFlight = new Set<string>();
let detach: (() => void) | null = null;

function emit() {
  for (const listener of listeners) listener();
}

/** Whether the last write reached storage: only then does the stored outbox speak for this tab too. */
let stored = false;

function persist() {
  if (account) stored = writeJson(keyFor(account), items.length ? items : null);
}

function set(next: OutboxItem[], save = true) {
  if (next === items) return;
  items = next;
  if (save) persist();
  emit();
  schedule();
}

const online = () => (typeof navigator === "undefined" ? true : navigator.onLine !== false);

function schedule() {
  if (timer) clearTimeout(timer);
  timer = null;
  if (!context) return;
  const now = Date.now();
  for (const item of due(items, now, online())) void send(item);
  const wake = nextWake(items.filter(item => !inFlight.has(item.key)));
  if (wake !== null && wake > now) timer = setTimeout(schedule, Math.min(wake - now, 2 ** 31 - 1));
}

/** Runs `task` while holding this item's lock; skips it when another tab is sending the same item. */
async function exclusively(key: string, task: () => Promise<void>): Promise<void> {
  const locks = typeof navigator !== "undefined" ? (navigator as Navigator & { locks?: LockManager }).locks : undefined;
  if (!locks) return task();
  await locks.request(`dm:outbox:${key}`, { ifAvailable: true }, async lock => {
    if (lock) await task();
  });
}

async function send(item: OutboxItem) {
  if (inFlight.has(item.key) || !context) return;
  inFlight.add(item.key);
  const { queryClient, me } = context;
  try {
    await exclusively(item.key, async () => {
      // Another tab may have sent it meanwhile (the stored outbox no longer has it).
      if (account && stored && canStore() && !restore(readJson(keyFor(account), [])).some(entry => entry.key === item.key)) {
        set(items.filter(entry => entry.key !== item.key), false);
        return;
      }
      set(start(items, item.key));
      try {
        const message = await sendMessage(item.address, item.body, item.key);
        applyMessage(queryClient, me, message);
        set(succeed(items, item.key));
      } catch (error) {
        const failure = ApiError.from(error);
        set(fail(items, item.key, { status: failure.status, code: failure.code, message: failure.message, hint: failure.hint }, Date.now(), online()));
      }
    });
  } finally {
    inFlight.delete(item.key);
    schedule();
  }
}

/** Connects the outbox to the signed-in account and the query cache (the chat shell does this once). */
export function attachOutbox(options: { queryClient: QueryClient; me: AccountRef }): () => void {
  const id = options.me.uuid ?? options.me.id;
  context = options;
  if (account !== id) {
    account = id;
    items = restore(readJson(keyFor(id), []));
    emit();
  }
  detach?.();
  const reload = () => {
    if (!account) return;
    const fromStorage = restore(readJson(keyFor(account), []));
    // Keep what this tab is sending right now; take everything else as the other tab left it.
    const sending = items.filter(item => inFlight.has(item.key));
    set([...fromStorage.filter(item => !sending.some(own => own.key === item.key)), ...sending], false);
  };
  const back = () => set(resume(items, Date.now()));
  const offStorage = onStorage(keyFor(id), reload);
  window.addEventListener("online", back);
  detach = () => {
    offStorage();
    window.removeEventListener("online", back);
  };
  schedule();
  return () => {
    detach?.();
    detach = null;
    context = null;
    if (timer) clearTimeout(timer);
    timer = null;
  };
}

/** Writes a send into the outbox (it shows as Waiting at once) and starts it. Returns its Idempotency-Key. */
export function queueSend(address: string, body: MessageCreate, replyPreview?: Reply | null): string {
  const key = crypto.randomUUID();
  set(enqueue(items, { key, address, body, replyPreview, now: Date.now() }));
  return key;
}

export const retrySend = (key: string) => set(retry(items, key, Date.now()));
export const discardSend = (key: string) => set(discard(items, key));

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

const snapshot = () => items;

/** Every item of the outbox (all conversations), oldest first. */
export function useOutbox(): OutboxItem[] {
  return useSyncExternalStore(subscribe, snapshot, () => EMPTY);
}

/** The outbox items of one conversation, oldest first. */
export function useConversationOutbox(address: string): OutboxItem[] {
  const all = useOutbox();
  return useMemo(() => all.filter(item => item.address === address).sort((a, b) => a.createdAt - b.createdAt), [all, address]);
}

/** The latest outbox item of a conversation (a row's "Waiting" or "Failed" tick), if any. */
export function latestPending(all: OutboxItem[], address: string): OutboxItem | undefined {
  let latest: OutboxItem | undefined;
  for (const item of all) if (item.address === address && (!latest || item.createdAt > latest.createdAt)) latest = item;
  return latest;
}
