/**
 * The sync loop: one per browser tab, long-polling GET /api/v1/sync?wait=25 for event references and applying each
 * one to the cached state (the message is fetched once and put in place; the conversation's row moves up with its new
 * preview, time and unread count; a conversation the list does not have yet brings a refresh of the list, one for the
 * whole burst). The cursor is kept in sessionStorage, so a reload continues where the tab was; a new tab starts at the
 * head, and once the head is known asks again for the list and threads it already fetched (refetchAfterReset). 409
 * `sync_reset_required` starts again from the head and then refreshes everything. Failures back off (1 s … 30 s) and
 * retry at once when the browser comes back online; a 401 stops the loop (the session keeper signs the Carbon in again).
 *
 * Conversation events (DM 0.17): when GET /api/v1/contracts announces them, the loop asks for them too
 * (`include=conversations`), so a conversation created with you, an invitation, a group joined or left on another
 * device, a rename and a change of members or owners reach the list and the invitations within a second
 * (planConversationEvents, then applyConversationPlan). Against an older service it reads messages only, and the
 * invitations' 15 s check and the list's own refresh keep things complete (lib/chat/features.ts).
 *
 * Only your own account syncs: a custodian's view of a Silicon's conversations refreshes by polling instead
 * (lib/chat/hooks.ts).
 */
import type { QueryClient } from "@tanstack/react-query";
import { ApiError } from "@/lib/errors";
import { getContracts, getConversation, getMessage, syncPage } from "./api";
import { applyMessageToSummaries, dropSummary, replaceSummary, summariesOf, upsertMessage, type ConversationPages, type MessagePages } from "./cache";
import { offersConversationEvents, setConversationEventsActive } from "./features";
import { chatKeys } from "./keys";
import { planConversationEvents, planEvents, SeenEvents, syncDelay, type ConversationPlan, type MessageRef } from "./sync-plan";
import { readText, writeText } from "./storage";
import type { AccountRef, ConversationSummary, Message } from "./types";
import { sameAccount } from "./address";

export interface IncomingInfo {
  /** Newer than anything the conversation's row knew: a new message, not an edit or a receipt. */
  fresh: boolean;
}

/** A conversation that went from the list because you are no longer in it (conversation events). */
export interface GoneConversation {
  id: string;
  /** Its name as the list had it (a group's name), or its address. */
  name: string;
  /** Someone else took you out (`removed`), rather than you leaving it on another device or tab. */
  removed: boolean;
}

export interface SyncOptions {
  queryClient: QueryClient;
  me: AccountRef;
  /** A message from someone else arrived or changed (notifications, delivered receipts). */
  onIncoming?: (message: Message, info: IncomingInfo) => void;
  /** A conversation the list had is gone: you left it elsewhere, or were removed (the shell leaves it if it is open). */
  onConversationGone?: (gone: GoneConversation) => void;
}

const WAIT_SECONDS = 25;
const FETCH_CONCURRENCY = 4;
const cursorKey = (account: string) => `dm:sync-cursor:v1:${account}`;

/** How long a burst of news waits before the list is asked for again (one request for all of it). */
const LIST_REFRESH_DELAY_MS = 250;
const listRefreshes = new WeakMap<QueryClient, ReturnType<typeof setTimeout>>();

/**
 * Asks for your conversation list (and your invitations) again, soon and once: everything that asks within a moment
 * shares one request. Used when a sync event names a conversation the list does not have (a group you joined on another
 * device, the first message of a new conversation, a conversation further down than the list has loaded), and when
 * another tab of this browser started, joined or left one (lib/chat/news.ts).
 */
export function refreshConversationsSoon(queryClient: QueryClient): void {
  if (listRefreshes.has(queryClient)) return;
  listRefreshes.set(queryClient, setTimeout(() => {
    listRefreshes.delete(queryClient);
    void queryClient.invalidateQueries({ queryKey: chatKeys.conversations(), exact: true });
    void queryClient.invalidateQueries({ queryKey: chatKeys.invitations(), exact: true });
  }, LIST_REFRESH_DELAY_MS));
}

const invitationRefreshes = new WeakMap<QueryClient, ReturnType<typeof setTimeout>>();

/** Asks for your invitations again, soon and once (a new one, or one answered or withdrawn somewhere else). */
export function refreshInvitationsSoon(queryClient: QueryClient): void {
  if (invitationRefreshes.has(queryClient) || listRefreshes.has(queryClient)) return;
  invitationRefreshes.set(queryClient, setTimeout(() => {
    invitationRefreshes.delete(queryClient);
    void queryClient.invalidateQueries({ queryKey: chatKeys.invitations(), exact: true });
  }, LIST_REFRESH_DELAY_MS));
}

/** Takes a conversation you are no longer in out of the list, and forgets its details. */
function dropConversation(queryClient: QueryClient, id: string): void {
  queryClient.setQueryData<ConversationPages>(chatKeys.conversations(), data => dropSummary(data, id));
  queryClient.removeQueries({ queryKey: chatKeys.group(id), exact: true });
  queryClient.removeQueries({ queryKey: chatKeys.conversation(id), exact: true });
}

/**
 * A conversation that changed (renamed, described, new members or owners) is asked for again and put in its row, which
 * keeps its place, and its group details are refreshed where they show (the details panel, the thread's header). One
 * the list does not have brings a refresh of the list; one that is gone (404, 403) comes out of it. Never throws: what
 * cannot be asked for now is caught up by the list's next refresh.
 */
async function refreshConversation(queryClient: QueryClient, id: string, signal: AbortSignal): Promise<void> {
  const listed = summariesOf(queryClient.getQueryData<ConversationPages>(chatKeys.conversations())).some(item => item.id === id || item.supersedes?.includes(id));
  if (!listed) {
    refreshConversationsSoon(queryClient);
    return;
  }
  try {
    // pages: 0, so a conversation that is gone is a 404 at once (no walk through the list looking for it).
    const summary = await getConversation(id, { signal, pages: 0 });
    queryClient.setQueryData<ConversationPages>(chatKeys.conversations(), data => replaceSummary(data, summary));
    queryClient.setQueryData<ConversationSummary>(chatKeys.conversation(id), summary);
    void queryClient.invalidateQueries({ queryKey: chatKeys.group(id), exact: true });
  } catch (error) {
    if (signal.aborted) return;
    const failure = ApiError.from(error);
    if (failure.status === 404 || failure.status === 403) dropConversation(queryClient, id);
    else refreshConversationsSoon(queryClient);
  }
}

/**
 * Carries out what a page's conversation events ask for (lib/chat/sync-plan.ts planConversationEvents). Never throws.
 * `onGone` hears of each conversation that leaves the list here (one this tab already dropped, by leaving it itself,
 * is not heard of again).
 */
export async function applyConversationPlan(queryClient: QueryClient, plan: ConversationPlan, signal: AbortSignal, onGone?: (gone: GoneConversation) => void): Promise<void> {
  for (const id of plan.drop) {
    const summary = summariesOf(queryClient.getQueryData<ConversationPages>(chatKeys.conversations())).find(item => item.id === id || item.supersedes?.includes(id));
    dropConversation(queryClient, id);
    if (summary) onGone?.({ id: summary.id, name: summary.group?.name || summary.id, removed: plan.removed.includes(id) });
  }
  // The list's refresh asks for the invitations too.
  if (plan.list) refreshConversationsSoon(queryClient);
  else if (plan.invitations) refreshInvitationsSoon(queryClient);
  await Promise.all(plan.refresh.map(id => refreshConversation(queryClient, id, signal)));
}

/** Whether the service offers conversation events in the sync feed; null when it could not be asked (ask again later). */
async function conversationEventsOffered(signal: AbortSignal): Promise<boolean | null> {
  try {
    return offersConversationEvents(await getContracts(signal));
  } catch (error) {
    const failure = ApiError.from(error);
    // An older service without the route, or one that refuses it: messages only. Anything else may pass: ask again.
    return failure.status === 404 || failure.status === 405 || failure.status === 403 ? false : null;
  }
}

/**
 * After a reset the feed starts at the head: whatever was fetched before the head was taken can miss messages that came
 * in between (they are neither in that answer nor after the cursor). So once a reset answers, your conversation list
 * and your own threads are asked for again (the ones on screen at once, the others when they next show); after an
 * expired history (409) everything is. Seen when a page loads while the service restarts: the thread loads, a message
 * is sent, and only then does the reset succeed.
 */
export function refetchAfterReset(queryClient: QueryClient, expired: boolean): void {
  if (expired) {
    void queryClient.invalidateQueries({ queryKey: chatKeys.all });
    return;
  }
  void queryClient.invalidateQueries({ queryKey: chatKeys.conversations(), exact: true });
  // Invitations too: an invitation sent before the head is in neither the answer nor the feed.
  void queryClient.invalidateQueries({ queryKey: chatKeys.invitations(), exact: true });
  // Your own threads only (a custodian's view of a Silicon is polled, not synced).
  void queryClient.invalidateQueries({ predicate: query => query.queryKey[0] === "dm" && query.queryKey[1] === "messages" && query.queryKey[3] === "" });
}

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
  // A conversation the list does not have: it is new to this tab, so the list is asked for again (once per burst).
  if (!found) refreshConversationsSoon(queryClient);
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
  // The history ran out (409): after the next reset everything is fetched again.
  let expired = false;
  // Conversation events: asked for once the contracts announce them (null until they have answered).
  let events: boolean | null = null;

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
      if (events === null) {
        events = await conversationEventsOffered(signal);
        // On as soon as the service offers them (a long poll may take 25 s to answer the first time), so the slower
        // checks step back at once; off again if the feed refuses them.
        setConversationEventsActive(events === true);
      }
      const include = events ? "conversations" as const : undefined;
      if (!cursor) {
        const head = await syncPage({ reset: true, include, signal });
        cursor = head.cursor;
        writeText(cursorKey(account), cursor, "session");
        refetchAfterReset(queryClient, expired);
        expired = false;
      }
      const page = await syncPage({ cursor, wait: more ? 0 : WAIT_SECONDS, include, signal });
      const plan = planConversationEvents(page.events, seen);
      await applyConversationPlan(queryClient, plan, signal, options.onConversationGone);
      for (const id of plan.eventIds) seen.add(id);
      await inPool(planEvents(page.events, seen), FETCH_CONCURRENCY, applyRef);
      cursor = page.cursor;
      writeText(cursorKey(account), cursor, "session");
      more = page.has_more;
      failures = 0;
    } catch (error) {
      if (signal.aborted) break;
      const failure = ApiError.from(error);
      if (failure.status === 401) break;
      if (failure.status === 422 && events && failure.details.field === "include") {
        // A service that does not take `include` after all: read messages only, and let the slower checks keep up.
        events = false;
        setConversationEventsActive(false);
        continue;
      }
      if (failure.status === 409 && failure.code === "sync_reset_required") {
        // Start again from the head; everything is fetched again once the reset has answered (refetchAfterReset).
        cursor = null;
        more = false;
        expired = true;
        writeText(cursorKey(account), null, "session");
        continue;
      }
      failures += 1;
      await sleep(syncDelay(failures), signal);
    }
  }
  setConversationEventsActive(false);
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
