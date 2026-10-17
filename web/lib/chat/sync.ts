/**
 * The sync loop: one per browser tab, long-polling GET /api/v1/sync?wait=25 for event references and applying each
 * one to the cached state (the message is fetched once and put in place; the conversation's row moves up with its new
 * preview, time and unread count). The cursor is kept in sessionStorage, so a reload continues where the tab was;
 * a new tab starts at the head (its lists load fresh). 409 `sync_reset_required` starts again from the head and
 * refreshes everything. Failures back off (1 s … 30 s) and retry at once when the browser comes back online; a 401
 * stops the loop (the session keeper signs the Carbon in again).
 *
 * Only your own account syncs: a custodian's view of a Silicon's conversations refreshes by polling instead
 * (lib/chat/hooks.ts).
 */
import type { QueryClient } from "@tanstack/react-query";
import { ApiError } from "@/lib/errors";
import { getMessage, syncPage } from "./api";
import { applyMessageToSummaries, upsertMessage, type ConversationPages, type MessagePages } from "./cache";
import { chatKeys } from "./keys";
import { planEvents, SeenEvents, syncDelay, type MessageRef } from "./sync-plan";
import { readText, writeText } from "./storage";
import type { AccountRef, Message } from "./types";
import { sameAccount } from "./address";

export interface IncomingInfo {
  /** Newer than anything the conversation's row knew: a new message, not an edit or a receipt. */
  fresh: boolean;
}

export interface SyncOptions {
  queryClient: QueryClient;
  me: AccountRef;
  /** A message from someone else arrived or changed (notifications, delivered receipts). */
  onIncoming?: (message: Message, info: IncomingInfo) => void;
}

const WAIT_SECONDS = 25;
const FETCH_CONCURRENCY = 4;
const cursorKey = (account: string) => `dm:sync-cursor:v1:${account}`;

/** Puts one fetched message where it belongs in the caches. Returns whether it is fresh for its conversation. */
export function applyMessage(queryClient: QueryClient, me: AccountRef, message: Message): IncomingInfo {
  queryClient.setQueryData<MessagePages>(chatKeys.messages(message.conversation_id), data => upsertMessage(data, message));
  let found = true;
  let fresh = false;
  queryClient.setQueryData<ConversationPages>(chatKeys.conversations(), data => {
    const result = applyMessageToSummaries(data, message, me);
    found = result.found || !data;
    fresh = result.fresh;
    return result.data;
  });
  if (!found) void queryClient.invalidateQueries({ queryKey: chatKeys.conversations() });
  return { fresh: fresh || !found };
}

async function inPool<T>(items: T[], limit: number, run: (item: T) => Promise<void>): Promise<void> {
  let next = 0;
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) await run(items[next++]!);
  });
  await Promise.all(workers);
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise(resolve => {
    const done = () => {
      clearTimeout(timer);
      window.removeEventListener("online", done);
      signal.removeEventListener("abort", done);
      resolve();
    };
    const timer = setTimeout(done, ms);
    window.addEventListener("online", done);
    signal.addEventListener("abort", done);
  });
}

/** Runs the loop until `signal` aborts. Never throws. */
export async function runSync(options: SyncOptions, signal: AbortSignal): Promise<void> {
  const { queryClient, me } = options;
  const account = me.uuid ?? me.id;
  const seen = new SeenEvents();
  let cursor = readText(cursorKey(account), "session");
  let more = false;
  let failures = 0;

  const applyRef = async (ref: MessageRef) => {
    try {
      const message = await getMessage(ref.conversation, ref.code, { signal });
      const info = applyMessage(queryClient, me, message);
      if (!sameAccount(message.sender, me)) options.onIncoming?.(message, info);
    } catch (error) {
      const failure = ApiError.from(error);
      // Gone or no longer visible to this account: nothing to show.
      if (failure.status !== 404 && failure.status !== 403 && failure.status !== 410) throw error;
    }
    for (const id of ref.eventIds) seen.add(id);
  };

  while (!signal.aborted) {
    try {
      if (!cursor) {
        const head = await syncPage({ reset: true, signal });
        cursor = head.cursor;
        writeText(cursorKey(account), cursor, "session");
      }
      const page = await syncPage({ cursor, wait: more ? 0 : WAIT_SECONDS, signal });
      await inPool(planEvents(page.events, seen), FETCH_CONCURRENCY, applyRef);
      cursor = page.cursor;
      writeText(cursorKey(account), cursor, "session");
      more = page.has_more;
      failures = 0;
    } catch (error) {
      if (signal.aborted) break;
      const failure = ApiError.from(error);
      if (failure.status === 401) break;
      if (failure.status === 409 && failure.code === "sync_reset_required") {
        cursor = null;
        more = false;
        writeText(cursorKey(account), null, "session");
        await queryClient.invalidateQueries({ queryKey: chatKeys.all });
        continue;
      }
      failures += 1;
      await sleep(syncDelay(failures), signal);
    }
  }
}

let running: { account: string; controller: AbortController; users: number } | null = null;

/**
 * Starts the tab's sync loop (or joins the one already running for this account) and returns a release function;
 * the loop stops when the last user releases it.
 */
export function startSync(options: SyncOptions): () => void {
  const account = options.me.uuid ?? options.me.id;
  if (running && running.account !== account) {
    running.controller.abort();
    running = null;
  }
  if (!running) {
    const controller = new AbortController();
    running = { account, controller, users: 0 };
    void runSync(options, controller.signal);
  }
  const current = running;
  current.users += 1;
  return () => {
    current.users -= 1;
    if (current.users <= 0) {
      current.controller.abort();
      if (running === current) running = null;
    }
  };
}
