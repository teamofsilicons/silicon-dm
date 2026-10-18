/**
 * Listen mode's queue (lib/voice/queue.ts): only new Silicon messages are spoken, in order, never the Carbon's own or
 * old ones, and a spoken message waits for its render.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import type { AccountRef, Message } from "../lib/chat/types";
import { beginSpeaking, clearQueue, EMPTY_QUEUE, finishSpeaking, headDeadline, nextToSpeak, observeMessages, PENDING_GRACE_MS, returnToQueue, SEEN_LIMIT, speakable, startQueue } from "../lib/voice/queue";

const ada: AccountRef = { type: "carbon", id: "c:ada", uuid: "u-ada" };
const scout: AccountRef = { type: "silicon", id: "si:scout", uuid: "u-scout" };
const bo: AccountRef = { type: "carbon", id: "c:bo", uuid: "u-bo" };

function message(code: string, sender: AccountRef, text: string, extra: Partial<Message> = {}): Message {
  return {
    "message-id": code,
    conversation_id: "c:ada::si:scout",
    recipient_id: "c:ada",
    sender,
    message: text,
    attachments: [],
    voice_transcript: null,
    reply: null,
    bundle: null,
    history: [],
    created_at: "2026-10-17T10:00:00Z",
    updated_at: null,
    deleted_at: null,
    delivered_at: null,
    read_at: null,
    ...extra,
  };
}

const spoken = (code: string, status: "pending" | "ready" | "failed") => message(code, scout, "", { speech: { voice: "mark", script: "[warmly] Hi Ada.", status } });
const map = (list: Message[]) => new Map(list.map(item => [item["message-id"], item]));

test("what is speakable: a Silicon's words or speech, never mine, never deleted, never a Carbon's", () => {
  assert.equal(speakable(message("001", scout, "Hello"), ada), true);
  assert.equal(speakable(message("001", scout, "   "), ada), false, "nothing to say");
  assert.equal(speakable(spoken("002", "ready"), ada), true);
  assert.equal(speakable(message("003", bo, "Hi"), ada), false, "a Carbon's message is not read aloud");
  assert.equal(speakable(message("004", scout, "Hi", { deleted_at: "2026-10-17T10:01:00Z" }), ada), false);
  assert.equal(speakable(message("005", scout, "Hi"), scout), false, "in a custodian's view the viewed Silicon's own are mine");
  assert.equal(speakable(message("006", scout, "Words only"), ada, "speech"), false, "autoplay plays spoken messages only");
  assert.equal(speakable(spoken("007", "ready"), ada, "speech"), true);
});

test("turning it on makes everything already there the past", () => {
  const history = [message("00a", scout, "old one"), message("00b", ada, "mine")];
  let queue = startQueue(history);
  assert.equal(queue.mark, "00b");
  queue = observeMessages(queue, history, ada);
  assert.deepEqual(queue.waiting, []);
  // An older page loading later never counts as new.
  queue = observeMessages(queue, [message("009", scout, "even older"), ...history], ada);
  assert.deepEqual(queue.waiting, []);
});

test("before the messages were seen, the first look starts the queue without speaking anything", () => {
  const first = observeMessages(EMPTY_QUEUE, [message("00a", scout, "backlog")], ada);
  assert.equal(first.started, true);
  assert.equal(first.mark, "00a");
  assert.deepEqual(first.waiting, []);
  const empty = observeMessages(EMPTY_QUEUE, [], ada);
  assert.equal(empty.mark, null);
  const next = observeMessages(empty, [message("000", scout, "the very first message")], ada, "all", 5);
  assert.deepEqual(next.waiting, [{ code: "000", since: 5 }], "in an empty conversation every arrival is new");
});

test("new Silicon messages queue in code order; mine are seen but never spoken", () => {
  let queue = startQueue([message("00a", scout, "old")]);
  const later = [message("00a", scout, "old"), message("00c", scout, "second"), message("00b", scout, "first"), message("00d", ada, "my reply")];
  queue = observeMessages(queue, later, ada, "all", 100);
  assert.deepEqual(queue.waiting.map(item => item.code), ["00b", "00c"]);
  assert.equal(queue.mark, "00a", "the mark is where Listen mode started");
  assert.deepEqual(queue.seen, ["00b", "00c", "00d"]);
  const again = observeMessages(queue, later, ada, "all", 200);
  assert.equal(again, queue, "nothing changed: the same queue");
});

test("a message that lands after a newer one is still spoken, once", () => {
  const old = [message("00a", scout, "old")];
  let queue = startQueue(old);
  // 00c arrives first (a slower fetch held 00b back), and is spoken.
  queue = observeMessages(queue, [...old, message("00c", scout, "third")], ada, "all", 100);
  assert.deepEqual(queue.waiting.map(item => item.code), ["00c"]);
  queue = finishSpeaking(beginSpeaking(queue, "00c"), "00c");
  // Then 00b lands: below the newest code seen, but never seen itself.
  const all = [...old, message("00b", scout, "second"), message("00c", scout, "third")];
  queue = observeMessages(queue, all, ada, "all", 200);
  assert.deepEqual(queue.waiting.map(item => item.code), ["00b"], "a newest-code mark lost it");
  queue = finishSpeaking(beginSpeaking(queue, "00b"), "00b");
  assert.deepEqual(observeMessages(queue, all, ada, "all", 300).waiting, [], "and it is not spoken twice");
});

test("the seen codes stay bounded: the oldest fold into the mark", () => {
  const code = (index: number) => index.toString(36).padStart(3, "0");
  let queue = startQueue([]);
  const many = Array.from({ length: SEEN_LIMIT + 5 }, (_, index) => message(code(index), ada, `mine ${index}`));
  queue = observeMessages(queue, many, ada);
  assert.equal(queue.seen.length, SEEN_LIMIT);
  assert.equal(queue.mark, code(4), "the five oldest are now the past");
  assert.deepEqual(queue.waiting, [], "my own messages are never spoken");
  assert.deepEqual(observeMessages(queue, many, ada).waiting, [], "nothing comes back as new");
});

test("one at a time, in order; finishing lets the next one go", () => {
  let queue = startQueue([]);
  const list = [message("001", scout, "one"), message("002", scout, "two")];
  queue = observeMessages(queue, list, ada);
  const first = nextToSpeak(queue, map(list));
  assert.equal(first?.["message-id"], "001");
  queue = beginSpeaking(queue, "001");
  assert.equal(nextToSpeak(queue, map(list)), null, "nothing starts while one is speaking");
  queue = observeMessages(queue, list, ada);
  assert.deepEqual(queue.waiting.map(item => item.code), ["002"], "the one speaking does not queue again");
  queue = finishSpeaking(queue, "001");
  assert.equal(nextToSpeak(queue, map(list))?.["message-id"], "002");
});

test("a spoken message waits at the head until it is rendered, then for the grace at most", () => {
  let queue = startQueue([]);
  let list = [spoken("001", "pending"), message("002", scout, "after it")];
  queue = observeMessages(queue, list, ada, "all", 1000);
  assert.equal(nextToSpeak(queue, map(list), 1000), null, "pending: the head waits and keeps the order");
  assert.equal(headDeadline(queue, map(list)), 1000 + PENDING_GRACE_MS);
  list = [spoken("001", "ready"), message("002", scout, "after it")];
  queue = observeMessages(queue, list, ada, "all", 2000);
  assert.equal(nextToSpeak(queue, map(list), 2000)?.["message-id"], "001", "ready: it goes");
  const stuck = observeMessages(startQueue([]), [spoken("003", "pending")], ada, "all", 0);
  assert.equal(nextToSpeak(stuck, map([spoken("003", "pending")]), PENDING_GRACE_MS)?.["message-id"], "003", "still pending after the grace: tried anyway");
  const failed = observeMessages(startQueue([]), [spoken("004", "failed")], ada, "all", 0);
  assert.equal(nextToSpeak(failed, map([spoken("004", "failed")]), 0)?.["message-id"], "004", "a failed render is tried on demand");
});

test("a deleted message leaves the queue; Stop empties it but new ones still come", () => {
  let queue = startQueue([]);
  queue = observeMessages(queue, [message("001", scout, "one"), message("002", scout, "two")], ada);
  queue = observeMessages(queue, [message("001", scout, "one", { deleted_at: "2026-10-17T10:02:00Z" }), message("002", scout, "two")], ada);
  assert.deepEqual(queue.waiting.map(item => item.code), ["002"]);
  queue = clearQueue(beginSpeaking(queue, "002"));
  assert.equal(queue.current, null);
  assert.deepEqual(queue.waiting, []);
  queue = observeMessages(queue, [message("002", scout, "two"), message("003", scout, "three")], ada);
  assert.deepEqual(queue.waiting.map(item => item.code), ["003"]);
});

test("a message that had to give way to something else goes back to the head, its wait already done", () => {
  const old = [message("001", scout, "Old")];
  let queue = startQueue(old);
  const now = Date.now();
  queue = observeMessages(queue, [...old, spoken("002", "pending"), message("003", scout, "Later")], ada, "all", now);
  queue = beginSpeaking(queue, "002");
  queue = returnToQueue(queue, "002");
  assert.equal(queue.current, null);
  assert.deepEqual(queue.waiting.map(item => item.code), ["002", "003"]);
  // Still rendering, but it has waited its grace already: it is tried as soon as it is its turn.
  assert.equal(nextToSpeak(queue, map([...old, spoken("002", "pending"), message("003", scout, "Later")]), now)?.["message-id"], "002");
  assert.equal(returnToQueue(queue, "009"), queue, "only the message being spoken goes back");
});
