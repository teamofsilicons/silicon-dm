/** The outbox state machine (lib/chat/outbox.ts): a send is never lost, never doubled, and fails only for a reason. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { backoff, discard, due, enqueue, fail, isRetryable, MAX_ATTEMPTS, nextWake, restore, resume, retry, start, succeed, type OutboxItem } from "../lib/chat/outbox";

const body = { message: "hello" };
const noJitter = () => 0;
const network = { status: 0, code: "network_error", message: "Could not reach this site." };
const refused = { status: 422, code: "validation_failed", message: "The message is empty.", hint: "Write something first." };

function queue(...keys: string[]): OutboxItem[] {
  return keys.reduce<OutboxItem[]>((items, key, index) => enqueue(items, { key, address: "c:ada::si:scout", body, now: 1000 + index }), []);
}

test("a new send waits at once, keeps its key, and is due immediately", () => {
  const items = queue("k1");
  assert.equal(items.length, 1);
  assert.equal(items[0]!.status, "waiting");
  assert.equal(items[0]!.attempts, 0);
  assert.deepEqual(due(items, 1000, true).map(item => item.key), ["k1"]);
  assert.equal(enqueue(items, { key: "k1", address: "c:ada::si:scout", body, now: 2000 }), items, "the same key is not queued twice");
});

test("sends of one conversation go one at a time, in the order they were written", () => {
  let items = queue("k1", "k2");
  assert.deepEqual(due(items, 5000, true).map(item => item.key), ["k1"]);
  items = start(items, "k1");
  assert.deepEqual(due(items, 5000, true), [], "nothing else starts while k1 is in flight");
  items = succeed(items, "k1");
  assert.deepEqual(due(items, 5000, true).map(item => item.key), ["k2"]);
  const other = enqueue(items, { key: "g1", address: "g:product", body, now: 1 });
  assert.deepEqual(due(other, 5000, true).map(item => item.key).sort(), ["g1", "k2"], "other conversations do not wait");
});

test("network failures back off and retry with the same key, up to five attempts, then fail", () => {
  let items = start(queue("k1"), "k1");
  let now = 10_000;
  for (let attempt = 1; attempt < MAX_ATTEMPTS; attempt++) {
    items = fail(items, "k1", network, now, true, noJitter);
    const item = items[0]!;
    assert.equal(item.status, "waiting", `attempt ${attempt} waits`);
    assert.equal(item.key, "k1");
    assert.equal(item.attempts, attempt);
    assert.equal(item.nextAttemptAt, now + backoff(attempt, noJitter));
    assert.deepEqual(due(items, now, true), [], "not before its backoff");
    now = item.nextAttemptAt;
    assert.deepEqual(due(items, now, true).map(entry => entry.key), ["k1"]);
    items = start(items, "k1");
  }
  items = fail(items, "k1", network, now, true, noJitter);
  assert.equal(items[0]!.status, "failed");
  assert.equal(items[0]!.attempts, MAX_ATTEMPTS);
  assert.deepEqual(items[0]!.error, network);
});

test("the backoff doubles from one second and is capped", () => {
  assert.deepEqual([1, 2, 3, 4, 5].map(attempt => backoff(attempt, noJitter)), [1000, 2000, 4000, 8000, 16000]);
  assert.equal(backoff(20, noJitter), 30_000);
  assert.ok(backoff(1, () => 1) <= 1200, "jitter stays within 20 %");
});

test("a refusal from the service fails at once with its words; retry starts again with the same key", () => {
  let items = start(queue("k1"), "k1");
  items = fail(items, "k1", refused, 2000, true);
  assert.equal(items[0]!.status, "failed");
  assert.equal(items[0]!.error?.hint, "Write something first.");
  items = retry(items, "k1", 3000);
  assert.equal(items[0]!.status, "waiting");
  assert.equal(items[0]!.attempts, 0);
  assert.equal(items[0]!.error, undefined);
  assert.equal(items[0]!.key, "k1");
  assert.deepEqual(due(items, 3000, true).map(item => item.key), ["k1"]);
  assert.deepEqual(discard(items, "k1"), []);
});

test("which failures are worth retrying", () => {
  assert.equal(isRetryable(network), true);
  assert.equal(isRetryable({ status: 503, code: "server_unavailable", message: "" }), true);
  assert.equal(isRetryable({ status: 429, code: "rate_limited", message: "" }), true);
  assert.equal(isRetryable({ status: 408, code: "timeout", message: "" }), true);
  for (const status of [400, 401, 403, 404, 409, 413, 422]) assert.equal(isRetryable({ status, code: "x", message: "" }), false, String(status));
  assert.equal(isRetryable({ status: 0, code: "client_error", message: "" }), false);
});

test("offline attempts do not count, and coming back online makes waiting sends due now", () => {
  let items = start(queue("k1"), "k1");
  items = fail(items, "k1", network, 1000, false);
  assert.equal(items[0]!.status, "waiting");
  assert.equal(items[0]!.attempts, 0, "an offline failure is not an attempt");
  assert.deepEqual(due(items, 1000, false), [], "nothing is sent while offline");
  items = fail(start(items, "k1"), "k1", network, 1000, true, noJitter);
  assert.equal(items[0]!.nextAttemptAt, 2000);
  assert.equal(resume(items, 1200)[0]!.nextAttemptAt, 1200);
  assert.equal(nextWake(items), 2000);
  assert.equal(nextWake([]), null);
});

test("a send in flight when the page went away waits to be sent again; junk in storage is dropped", () => {
  const stored = [{ ...queue("k1")[0]!, status: "sending" }, { nope: true }, null, { ...queue("k2")[0]!, status: "failed" }];
  const restored = restore(stored);
  assert.deepEqual(restored.map(item => [item.key, item.status]), [["k1", "waiting"], ["k2", "failed"]]);
  assert.deepEqual(restore("not a list"), []);
});
