/**
 * Receipts (lib/chat/receipt-plan.ts): a read covers a delivered receipt, a bulk answer is asked again only when it
 * left older messages, and a failure that passes waits (the service's Retry-After first).
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { BULK_RECEIPTS_PER_CALL, flushPlan, leftOlder, passingFailure, receiptRetryDelay } from "../lib/chat/receipt-plan";

test("a read in the same flush covers the delivered receipt; a delivered one goes alone when the read waits or is older", () => {
  assert.deepEqual(flushPlan({ read: "00c", delivered: "00c" }, true), { read: "00c", delivered: null }, "read implies delivered: one call");
  assert.deepEqual(flushPlan({ read: "00c", delivered: "00a" }, true), { read: "00c", delivered: null });
  assert.deepEqual(flushPlan({ read: "00a", delivered: "00c" }, true), { read: "00a", delivered: "00c" }, "fetched further than seen: both go");
  assert.deepEqual(flushPlan({ read: "00c", delivered: "00c" }, false), { read: null, delivered: "00c" }, "an unfocused tab says delivered and keeps the read for focus");
  assert.deepEqual(flushPlan({ read: null, delivered: "00b" }, true), { read: null, delivered: "00b" });
  assert.deepEqual(flushPlan({ read: "0zz", delivered: "1000" }, true), { read: "0zz", delivered: "1000" }, "codes compare by length first");
});

test("a bulk receipt is made again only when it marked as many as one call marks and something is left", () => {
  assert.equal(leftOlder("read", { updated: 12, unread_count: 3 }), false, "unread after the code (not on screen yet): asking again marks nothing");
  assert.equal(leftOlder("read", { updated: BULK_RECEIPTS_PER_CALL, unread_count: 1000 }), true, "capped: older ones are left");
  assert.equal(leftOlder("read", { updated: BULK_RECEIPTS_PER_CALL, unread_count: 0 }), false);
  assert.equal(leftOlder("delivered", { updated: BULK_RECEIPTS_PER_CALL, unread_count: 0 }), true, "delivered counts nothing unread: the cap says it");
  assert.equal(leftOlder("delivered", { updated: 4, unread_count: 4 }), false);
});

test("a failure that passes waits: the service's Retry-After, else 2 s doubling up to a minute; refusals are not retried", () => {
  for (const status of [0, 408, 429, 500, 502, 503, 504]) assert.equal(passingFailure(status), true, `${status} passes`);
  for (const status of [400, 401, 403, 404, 409, 410, 422]) assert.equal(passingFailure(status), false, `${status} is a refusal`);
  const none = () => 0;
  assert.equal(receiptRetryDelay(1, 5, none), 5000, "Retry-After: 5");
  assert.equal(receiptRetryDelay(1, 0, none), 1000, "never at once");
  assert.equal(receiptRetryDelay(1, 3600, none), 300_000, "at most five minutes");
  assert.equal(receiptRetryDelay(1, null, none), 2000);
  assert.equal(receiptRetryDelay(2, null, none), 4000);
  assert.equal(receiptRetryDelay(9, null, none), 60_000);
  assert.ok(receiptRetryDelay(1, null, () => 1) <= 2500, "jitter of at most a quarter");
});
