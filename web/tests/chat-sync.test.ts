/**
 * Sync events applied to the cached state (lib/chat/sync-plan.ts, lib/chat/cache.ts): each message fetched once, put
 * where it belongs, the conversation's row updated in place (preview, time, unread, order), never moving backwards; a
 * conversation the list does not have refreshes the list once per burst; conversation events (DM 0.17) refresh, drop or
 * add rows and the invitations, the last word on each conversation winning.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { applyMessageToSummaries, applyReadToSummaries, dropSummary, findMessage, mergeMessage, messagesOldestFirst, replaceSummary, statusOf, summariesOf, totalUnread, unreadOf, upsertMessage, type ConversationPages, type MessagePages } from "../lib/chat/cache";
import { compareCodes } from "../lib/chat/codes";
import { QueryClient } from "@tanstack/react-query";
import { chatKeys } from "../lib/chat/keys";
import { offersConversationEvents } from "../lib/chat/features";
import { applyConversationPlan, applyMessage, refetchAfterReset, refreshConversationsSoon } from "../lib/chat/sync";
import { planConversationEvents, planEvents, SeenEvents, syncDelay } from "../lib/chat/sync-plan";
import type { AccountRef, ConversationSummary, Message, SyncEvent } from "../lib/chat/types";

const ada: AccountRef = { type: "carbon", id: "c:ada", uuid: "u-ada", display_name: "Ada" };
const scout: AccountRef = { type: "silicon", id: "si:scout", uuid: "u-scout", display_name: "Scout" };
const CONV = "c:ada::si:scout";

function message(code: string, sender: AccountRef, overrides: Partial<Message> = {}): Message {
  return {
    "message-id": code, conversation_id: CONV, recipient_id: sender === ada ? "si:scout" : "c:ada", sender, message: `text ${code}`, attachments: [], voice_transcript: null,
    reply: null, bundle: null, history: [], created_at: `2026-10-17T10:00:${code.slice(-2).replace(/[a-z]/g, "0")}Z`, updated_at: null, deleted_at: null, delivered_at: null, read_at: null, ...overrides,
  };
}

const pages = (...lists: Message[][]): MessagePages => ({ pages: lists.map((items, index) => ({ items, next_cursor: index < lists.length - 1 ? `c${index}` : null })), pageParams: lists.map((_, index) => (index ? `c${index - 1}` : null)) });

function summary(id: string, last: Message | null, overrides: Partial<ConversationSummary> = {}): ConversationSummary {
  return { id, participants: [ada, scout], last_message: last, last_message_status: last ? statusOf(last) : null, created_at: "2026-10-01T00:00:00Z", updated_at: last?.created_at ?? "2026-10-01T00:00:00Z", ...overrides };
}

const list = (...items: ConversationSummary[]): ConversationPages => ({ pages: [{ items, next_cursor: null }], pageParams: [null] });

test("message codes order by length, then as text", () => {
  const codes = ["00a", "1000", "zzz", "000", "009", "00z", "1001"];
  assert.deepEqual([...codes].sort(compareCodes), ["000", "009", "00a", "00z", "zzz", "1000", "1001"]);
});

test("a page of events asks for each message once, and applied events are skipped", () => {
  const events: SyncEvent[] = [
    { event_id: "e1", sequence: 1, type: "message", conversation_id: CONV, message_id: "00a" },
    { event_id: "e2", sequence: 2, type: "message_status", conversation_id: CONV, message_id: "00a" },
    { event_id: "e3", sequence: 3, type: "message", conversation_id: "g:product", message_id: "001" },
    { event_id: "e1", sequence: 1, type: "message", conversation_id: CONV, message_id: "00a" },
  ];
  const seen = new SeenEvents();
  const refs = planEvents(events, seen);
  assert.deepEqual(refs.map(ref => [ref.conversation, ref.code, ref.kinds, ref.eventIds]), [
    [CONV, "00a", ["message", "message_status"], ["e1", "e2", "e1"]],
    ["g:product", "001", ["message"], ["e3"]],
  ]);
  for (const ref of refs) for (const id of ref.eventIds) seen.add(id);
  assert.deepEqual(planEvents(events, seen), [], "nothing again once applied");
});

test("the seen-events memory keeps the most recent ids only", () => {
  const seen = new SeenEvents(3);
  for (const id of ["a", "b", "c", "d"]) seen.add(id);
  assert.equal(seen.size, 3);
  assert.equal(seen.has("a"), false);
  assert.equal(seen.has("d"), true);
});

test("a new message goes on top of the newest page; an edit replaces in place; old ones wait for their page", () => {
  const data = pages([message("002", scout), message("001", ada)], [message("000", scout)]);
  const added = upsertMessage(data, message("003", scout))!;
  assert.deepEqual(added.pages[0]!.items.map(item => item["message-id"]), ["003", "002", "001"]);
  const edited = upsertMessage(added, message("001", ada, { message: "edited", updated_at: "2026-10-17T11:00:00Z" }))!;
  assert.equal(findMessage(edited, "001")?.message, "edited");
  assert.equal(upsertMessage(edited, findMessage(edited, "001")!), edited, "nothing changed: same object");
  const older = pages([message("005", scout)]);
  older.pages[0]!.next_cursor = "more";
  assert.equal(upsertMessage(older, message("002", scout)), older, "older than everything loaded while more pages remain");
  assert.deepEqual(messagesOldestFirst(added).map(item => item["message-id"]), ["000", "001", "002", "003"]);
  assert.equal(upsertMessage(undefined, message("000", ada)), undefined, "nothing loaded: nothing to update");
});

test("merging never undoes a deletion, keeps the later edit, and never moves receipts backwards", () => {
  const deleted = message("001", ada, { message: null, deleted_at: "2026-10-17T12:00:00Z" });
  const stale = message("001", ada, { message: "old text", read_at: "2026-10-17T12:01:00Z" });
  const merged = mergeMessage(deleted, stale);
  assert.equal(merged.deleted_at, deleted.deleted_at);
  assert.equal(merged.message, null);
  assert.equal(merged.read_at, stale.read_at, "a receipt still arrives");
  const newer = message("001", ada, { message: "v2", updated_at: "2026-10-17T12:05:00Z", delivered_at: "2026-10-17T12:00:00Z" });
  const delayed = message("001", ada, { message: "v1", updated_at: "2026-10-17T12:01:00Z", delivered_at: null, read_at: "2026-10-17T12:06:00Z" });
  const both = mergeMessage(newer, delayed);
  assert.equal(both.message, "v2", "the later edit stays");
  assert.equal(both.delivered_at, newer.delivered_at, "delivered does not go back to null");
  assert.equal(both.read_at, delayed.read_at);
});

test("a new message from someone else moves its row to the top with its preview, status and one more unread", () => {
  const other = summary("g:product", message("004", ada, { conversation_id: "g:product", created_at: "2026-10-17T10:30:00Z" }), { updated_at: "2026-10-17T10:30:00Z", unread_count: 0 });
  const mine = summary(CONV, message("001", ada), { updated_at: "2026-10-17T10:00:01Z", unread_count: 0 });
  const update = applyMessageToSummaries(list(other, mine), message("002", scout, { created_at: "2026-10-17T10:45:00Z" }), ada);
  assert.equal(update.found, true);
  assert.equal(update.fresh, true);
  const rows = update.data!.pages[0]!.items;
  assert.deepEqual(rows.map(row => row.id), [CONV, "g:product"]);
  assert.equal(rows[0]!.last_message?.["message-id"], "002");
  assert.equal(rows[0]!.unread_count, 1);
  assert.equal(rows[0]!.last_message_status, "sent");
  assert.equal(rows[0]!.updated_at, "2026-10-17T10:45:00.000Z");
});

test("your own new message, an edit of the last one, or a receipt never counts as unread", () => {
  const start = list(summary(CONV, message("001", scout), { unread_count: 0 }));
  const own = applyMessageToSummaries(start, message("002", ada), ada);
  assert.equal(own.data!.pages[0]!.items[0]!.unread_count, 0);
  const read = applyMessageToSummaries(own.data, message("002", ada, { read_at: "2026-10-17T10:50:00Z" }), ada);
  assert.equal(read.fresh, false);
  assert.equal(read.data!.pages[0]!.items[0]!.last_message_status, "read");
  assert.equal(read.data!.pages[0]!.items[0]!.unread_count, 0);
  const older = applyMessageToSummaries(read.data, message("000", scout, { message: "edited" }), ada);
  assert.equal(older.data, read.data, "an edit of an older message leaves the row alone");
  assert.equal(applyMessageToSummaries(start, message("009", scout, { conversation_id: "g:elsewhere" }), ada).found, false, "an unknown conversation asks for a refetch");
});

test("reading sets the row's count from the service; without counts, an unread last message counts as one", () => {
  const data = list(summary(CONV, message("003", scout), { unread_count: 3, last_read_message_id: "000" }));
  const read = applyReadToSummaries(data, CONV, { unread_count: 0, last_read_message_id: "003", updated: 3 })!;
  assert.equal(read.pages[0]!.items[0]!.unread_count, 0);
  assert.equal(read.pages[0]!.items[0]!.last_read_message_id, "003");
  assert.equal(applyReadToSummaries(read, CONV, { unread_count: 0, last_read_message_id: "003" }), read, "no change, same object");
  const older = summary(CONV, message("003", scout));
  assert.equal(unreadOf(older, ada), 1);
  assert.equal(unreadOf({ ...older, last_message: message("003", scout, { read_at: "2026-10-17T11:00:00Z" }) }, ada), 0);
  assert.equal(unreadOf({ ...older, last_message: message("003", ada) }, ada), 0);
  assert.equal(totalUnread([older, summary("g:x", null, { unread_count: 4 })], ada), 5);
});

test("the sync loop backs off from one second up to thirty", () => {
  assert.deepEqual([1, 2, 3, 4, 5, 6, 7].map(failures => syncDelay(failures, () => 0)), [1000, 2000, 4000, 8000, 16000, 30000, 30000]);
});

test("a message in a conversation the list does not have refreshes the list and the invitations once for the burst", async () => {
  const queryClient = new QueryClient();
  queryClient.setQueryData(chatKeys.conversations(), list(summary(CONV, message("001", scout))));
  const asked: string[] = [];
  queryClient.invalidateQueries = (async (filters?: { queryKey?: readonly unknown[] }) => {
    asked.push(JSON.stringify(filters?.queryKey));
  }) as QueryClient["invalidateQueries"];
  // A group joined on another device, then three messages in it: one refresh, of the list and the invitations.
  for (const code of ["000", "001", "002"]) applyMessage(queryClient, ada, message(code, scout, { conversation_id: "g:joined-elsewhere" }));
  // A conversation the list has: nothing to ask for.
  applyMessage(queryClient, ada, message("002", scout));
  assert.deepEqual(asked, [], "it waits for the burst to end");
  await new Promise(resolve => setTimeout(resolve, 400));
  assert.deepEqual(asked, [JSON.stringify(chatKeys.conversations()), JSON.stringify(chatKeys.invitations())]);
  // Another tab's news (lib/chat/news.ts) goes the same way: a moment later, once.
  refreshConversationsSoon(queryClient);
  refreshConversationsSoon(queryClient);
  await new Promise(resolve => setTimeout(resolve, 400));
  assert.equal(asked.length, 4);
  queryClient.clear();
});

test("after a reset the list and your own threads are fetched again; after an expired history, everything is", () => {
  const queryClient = new QueryClient();
  queryClient.setQueryData(chatKeys.conversations(), list(summary(CONV, message("001", scout))));
  queryClient.setQueryData(chatKeys.messages(CONV), pages([message("001", scout)]));
  queryClient.setQueryData(chatKeys.messages(CONV, "si:scout"), pages([message("001", scout)]));
  queryClient.setQueryData(chatKeys.me, { account: ada });
  const asked: string[] = [];
  queryClient.invalidateQueries = (async (filters?: Parameters<QueryClient["invalidateQueries"]>[0]) => {
    for (const query of queryClient.getQueryCache().findAll(filters)) asked.push(JSON.stringify(query.queryKey));
  }) as QueryClient["invalidateQueries"];
  queryClient.setQueryData(chatKeys.invitations(), { items: [] });
  // A new tab's first reset: what it fetched before the head may lack messages that came in meanwhile.
  refetchAfterReset(queryClient, false);
  assert.deepEqual(asked.sort(), [JSON.stringify(chatKeys.conversations()), JSON.stringify(chatKeys.invitations()), JSON.stringify(chatKeys.messages(CONV))].sort(), "your list, invitations and threads, not a custodian's view (it is polled)");
  asked.length = 0;
  refetchAfterReset(queryClient, true);
  assert.equal(asked.length, 5, "an expired history refreshes everything");
  queryClient.clear();
});

const change = (event_id: string, sequence: number, conversation_id: string, what: string): SyncEvent => ({ event_id, sequence, type: "conversation", conversation_id, change: what });

test("conversation events: the list for what joined it, the invitations for theirs, a fresh copy for what changed, out for what you left", () => {
  const events: SyncEvent[] = [
    change("c1", 1, "g:new", "created"),
    change("c2", 2, "g:asked", "invited"),
    { event_id: "m1", sequence: 3, type: "message", conversation_id: "g:new", message_id: "000" },
    change("c3", 4, "g:renamed", "renamed"),
    change("c4", 5, "g:renamed", "members_changed"),
    // Removed, then added back: the last word wins, it stays (and the list is asked for again).
    change("c5", 6, "g:back", "removed"),
    change("c6", 7, "g:back", "added"),
    // Left on another device: out of the list, and a later change to it is not asked for.
    change("c8", 9, "g:left", "renamed"),
    change("c7", 8, "g:left", "left"),
    // A change this website does not know yet: ask for the conversation again.
    change("c9", 10, "g:pinned", "pinned"),
  ];
  const seen = new SeenEvents();
  const plan = planConversationEvents(events, seen);
  assert.deepEqual(plan.drop, ["g:left"]);
  assert.deepEqual(plan.removed, [], "g:back was removed, then added back; g:left was left");
  assert.deepEqual(plan.refresh, ["g:renamed", "g:pinned"]);
  assert.equal(plan.list, true);
  assert.equal(plan.invitations, true);
  assert.deepEqual(plan.eventIds, ["c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9"], "in the order they happened");
  // Messages are planned as before, and conversation events are never fetched as messages.
  assert.deepEqual(planEvents(events, seen).map(ref => ref.eventIds), [["m1"]]);
  for (const id of plan.eventIds) seen.add(id);
  assert.deepEqual(planConversationEvents(events, seen), { drop: [], removed: [], refresh: [], list: false, invitations: false, eventIds: [] }, "nothing again once applied");
  // Taken out by someone else: dropped, and said so (the shell tells the Carbon why the conversation went).
  const out = planConversationEvents([change("r1", 1, "g:out", "members_changed"), change("r2", 2, "g:out", "removed")], new SeenEvents());
  assert.deepEqual([out.drop, out.removed, out.refresh], [["g:out"], ["g:out"], []]);

  // An invitation answered or withdrawn touches the invitations only; one accepted brings the group into the list.
  assert.deepEqual(planConversationEvents([change("d1", 1, "g:x", "invitation_declined"), change("d2", 2, "g:y", "invitation_cancelled")], new SeenEvents()), { drop: [], removed: [], refresh: [], list: false, invitations: true, eventIds: ["d1", "d2"] });
  const accepted = planConversationEvents([change("e1", 1, "g:x", "invitation_accepted")], new SeenEvents());
  assert.equal(accepted.list && accepted.invitations, true);
});

test("the contracts announce conversation events in the shape the website reads", () => {
  assert.equal(offersConversationEvents({ features: { conversation_events: { minimum_client: "0.17.0", sync_query: "include=conversations", event_type: "conversation", changes: ["created"] } } }), true);
  assert.equal(offersConversationEvents({ features: {} }), false, "an older service");
  assert.equal(offersConversationEvents({ features: { conversation_events: { sync_query: "include=everything" } } }), false, "a query this website does not know");
  assert.equal(offersConversationEvents(null), false);
});

test("a fresh copy of a conversation keeps its row's place, and the row's newer activity; a left one comes out", () => {
  const older = summary("g:a", message("001", scout, { conversation_id: "g:a" }));
  const data = list(summary(CONV, message("005", scout)), older, summary("g:b", null));
  const renamed = { ...older, group: { name: "Renamed", description: "", owners: [] } };
  const replaced = replaceSummary(data, renamed)!;
  assert.deepEqual(summariesOf(replaced).map(item => item.id), [CONV, "g:a", "g:b"], "nothing moves");
  assert.equal(summariesOf(replaced)[1]!.group?.name, "Renamed");
  // The row learnt of message 002 from the feed while the copy (still at 001) was on its way: 002 stays.
  const ahead = list(summary("g:a", message("002", scout, { conversation_id: "g:a" }), { unread_count: 2 }));
  const kept = summariesOf(replaceSummary(ahead, { ...renamed, unread_count: 1 }))[0]!;
  assert.equal(kept.last_message?.["message-id"], "002");
  assert.equal(kept.unread_count, 2);
  assert.equal(kept.group?.name, "Renamed");
  assert.equal(replaceSummary(data, summary("g:unknown", null)), data, "a conversation the list lacks changes nothing");
  assert.deepEqual(summariesOf(dropSummary(data, "g:a")).map(item => item.id), [CONV, "g:b"]);
  assert.equal(dropSummary(data, "g:unknown"), data);
});

test("a plan drops what you left at once, asks for the list or the invitations once, and an unlisted change asks for the list", async () => {
  const queryClient = new QueryClient();
  queryClient.setQueryData(chatKeys.conversations(), list(summary(CONV, message("001", scout)), summary("g:left", null)));
  queryClient.setQueryData(chatKeys.group("g:left"), { id: "g:left" });
  const asked: string[] = [];
  queryClient.invalidateQueries = (async (filters?: { queryKey?: readonly unknown[] }) => {
    asked.push(JSON.stringify(filters?.queryKey));
  }) as QueryClient["invalidateQueries"];
  const signal = new AbortController().signal;
  const gone: unknown[] = [];
  await applyConversationPlan(queryClient, { drop: ["g:left", "g:never-listed"], removed: ["g:left"], refresh: [], list: false, invitations: true, eventIds: ["x"] }, signal, item => gone.push(item));
  assert.deepEqual(summariesOf(queryClient.getQueryData<ConversationPages>(chatKeys.conversations())).map(item => item.id), [CONV], "out at once");
  assert.equal(queryClient.getQueryData(chatKeys.group("g:left")), undefined, "its details are forgotten");
  assert.deepEqual(gone, [{ id: "g:left", name: "g:left", removed: true }], "heard of once, and only what the list had");
  await new Promise(resolve => setTimeout(resolve, 400));
  assert.deepEqual(asked, [JSON.stringify(chatKeys.invitations())], "the invitations only");
  asked.length = 0;
  // A rename of a group the list does not have yet (joined a moment ago): the list is asked for again, with the invitations.
  await applyConversationPlan(queryClient, { drop: [], removed: [], refresh: ["g:not-yet"], list: false, invitations: false, eventIds: ["y"] }, signal);
  await new Promise(resolve => setTimeout(resolve, 400));
  assert.deepEqual(asked, [JSON.stringify(chatKeys.conversations()), JSON.stringify(chatKeys.invitations())]);
  queryClient.clear();
});
