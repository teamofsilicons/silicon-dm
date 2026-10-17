/** The audio cache's LRU (lib/voice/lru.ts): replay is instant, memory stays bounded, playing audio is never evicted. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { Lru } from "../lib/voice/lru";

function cache(maxBytes = 100, maxEntries = 10) {
  const evicted: string[] = [];
  const lru = new Lru<string>({ maxBytes, maxEntries, onEvict: key => evicted.push(key) });
  return { lru, evicted };
}

test("the least recently used goes first when the bytes run over", () => {
  const { lru, evicted } = cache(100);
  lru.set("a", "A", 40);
  lru.set("b", "B", 40);
  assert.equal(lru.get("a"), "A", "reading a makes b the oldest");
  lru.set("c", "C", 40);
  assert.deepEqual(evicted, ["b"]);
  assert.deepEqual(lru.keys(), ["a", "c"]);
  assert.equal(lru.bytes, 80);
});

test("the count is bounded too", () => {
  const { lru, evicted } = cache(1_000_000, 2);
  lru.set("a", "A", 1);
  lru.set("b", "B", 1);
  lru.set("c", "C", 1);
  assert.deepEqual(evicted, ["a"]);
  assert.equal(lru.size, 2);
});

test("a pinned entry (playing now) survives eviction until it is unpinned", () => {
  const { lru, evicted } = cache(100);
  lru.set("playing", "P", 60);
  lru.pin("playing");
  lru.set("next", "N", 60);
  assert.deepEqual(evicted, [], "over the limit, but the only other entry is the newest one");
  assert.equal(lru.peek("playing"), "P");
  lru.set("third", "T", 10);
  assert.deepEqual(evicted, ["next"], "the oldest unpinned entry goes");
  lru.unpin("playing");
  assert.equal(lru.bytes, 70);
  lru.set("big", "B", 50);
  assert.deepEqual(evicted, ["next", "playing"], "once unpinned it can go");
});

test("replacing a key releases the old value; delete and deleteWhere release theirs", () => {
  const { lru, evicted } = cache(100);
  lru.set("a", "A1", 10);
  lru.set("a", "A2", 20);
  assert.deepEqual(evicted, ["a"]);
  assert.equal(lru.get("a"), "A2");
  assert.equal(lru.bytes, 20);
  lru.set("listen|c:ada::si:scout|00a", "x", 5);
  lru.set("listen|c:ada::si:scout|00b", "y", 5);
  lru.set("note|https://x", "z", 5);
  lru.pin("listen|c:ada::si:scout|00b");
  assert.equal(lru.deleteWhere(key => key.startsWith("listen|")), 1, "a pinned entry is kept");
  assert.equal(lru.has("listen|c:ada::si:scout|00b"), true);
  assert.equal(lru.delete("missing"), false);
  lru.unpin("listen|c:ada::si:scout|00b");
  lru.clear();
  assert.equal(lru.size, 0);
  assert.equal(lru.bytes, 0);
});
