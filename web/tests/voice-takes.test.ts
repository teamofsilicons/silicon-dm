import assert from "node:assert/strict";
import test from "node:test";
import { limitReached, limitWarning, MAX_RECORDING_MS, WARN_BEFORE_MS } from "../lib/voice/recorder";
import { abandoned, offerable, SEGMENT_MAX_BYTES, SEGMENT_MAX_MS, SEGMENT_MIN_MS, segmentDue, segmentName, type TakeInfo } from "../lib/voice/takes";

const take = (id: string, extra: Partial<TakeInfo> = {}): TakeInfo => ({
  id,
  account: "u-ada",
  address: "c:ada::si:scout",
  mode: "note",
  type: "audio/webm;codecs=opus",
  startedAt: 1_000,
  durationMs: 12_000,
  bytes: 72_000,
  segments: 3,
  store: "opfs",
  state: "stopped",
  updatedAt: 2_000,
  ...extra,
});

test("a recording is written to disk a few seconds at a time, a minute at most, never a huge segment", () => {
  // Nothing gathered: nothing to write.
  assert.equal(segmentDue(30_000, 0, 30_000), false);
  // Early on, every five seconds (a crash loses little of a short message).
  assert.equal(segmentDue(SEGMENT_MIN_MS - 1, 20_000, 10_000), false);
  assert.equal(segmentDue(SEGMENT_MIN_MS, 30_000, 10_000), true);
  // Later, longer segments (a long recording is not thousands of files): a twentieth of the time so far.
  assert.equal(segmentDue(20_000, 120_000, 600_000), false);
  assert.equal(segmentDue(30_000, 180_000, 600_000), true);
  // Never longer than a minute, never larger than a megabyte.
  assert.equal(segmentDue(SEGMENT_MAX_MS, 360_000, 40 * 3600_000), true);
  assert.equal(segmentDue(1_000, SEGMENT_MAX_BYTES, 3_600_000), true);
  // Names sort in recording order.
  assert.deepEqual([segmentName(10), segmentName(2), segmentName(0)].sort(), ["0000000.part", "0000002.part", "0000010.part"]);
});

test("a voice message left behind is offered to its own Carbon only, newest first, never while a tab still makes it", () => {
  const records = [
    take("old", { startedAt: 1_000 }),
    take("new", { startedAt: 5_000, state: "recording" }),
    take("live", { startedAt: 9_000, state: "recording" }),
    take("other", { account: "u-bo" }),
    take("dictation", { mode: "dictate" }),
    take("empty", { bytes: 0, segments: 0 }),
    take("memory", { store: "memory" }),
  ];
  const live = (record: TakeInfo) => record.id === "live";
  assert.deepEqual(offerable(records, "u-ada", live).map(record => record.id), ["new", "old"]);
  // What nobody will be offered is deleted: other accounts' (a voice is personal), dictations, empty ones; never a live one.
  assert.deepEqual(abandoned(records, "u-ada", live).map(record => record.id).sort(), ["dictation", "empty", "memory", "other"]);
  assert.deepEqual(abandoned(records, null, live).map(record => record.id).sort(), ["dictation", "empty", "memory", "new", "old", "other"]);
});

test("a recording stops by itself at 48 hours or at its size limit, with a minute's warning", () => {
  assert.equal(limitReached({ elapsedMs: MAX_RECORDING_MS - 1, bytes: 0 }), false);
  assert.equal(limitReached({ elapsedMs: MAX_RECORDING_MS, bytes: 0 }), true);
  // A dictation at 97 % of what DM transcribes at once (the last chunks still fit).
  assert.equal(limitReached({ elapsedMs: 60_000, bytes: 96 * 1024 * 1024, maxBytes: 100 * 1024 * 1024 }), false);
  assert.equal(limitReached({ elapsedMs: 60_000, bytes: 97 * 1024 * 1024, maxBytes: 100 * 1024 * 1024 }), true);
  // Ten minutes in, nothing is near.
  assert.equal(limitWarning({ elapsedMs: 600_000, bytes: 3_600_000, maxBytes: 100 * 1024 * 1024 }), null);
  // The last minute of 48 hours.
  assert.deepEqual(limitWarning({ elapsedMs: MAX_RECORDING_MS - 30_000, bytes: 1_000 }), { leftMs: 30_000, reason: "duration" });
  // A size limit read from how fast it grows: 6 kB a second, 120 kB left → 20 s.
  const warning = limitWarning({ elapsedMs: 100_000, bytes: 600_000, maxBytes: 720_000 });
  assert.equal(warning?.reason, "size");
  assert.ok(warning && Math.abs(warning.leftMs - 20_000) < 1);
  assert.equal(WARN_BEFORE_MS, 60_000);
});
