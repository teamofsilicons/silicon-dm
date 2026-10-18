/**
 * How an upload's progress reads in the tray (lib/upload/progress.ts, components/chat/composer/tray-view.tsx): bytes of
 * the whole, speed and time left for big files, a percentage for small ones, "Preparing…" while the file is hashed,
 * "Saving…" after the last byte, and what a lost connection means for a big file. The speed is smoothed and only
 * estimated after two seconds of sending.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { fileProgressWords } from "../components/chat/composer/tray";
import { BIG_FILE_BYTES, bytesWords, leftWords, partWords, progressWords, RateMeter, rateWords } from "../lib/upload/progress";

const MB = 1024 * 1024;
const GB = 1024 * MB;

test("sizes, parts, speeds and times in the tray's words", () => {
  assert.equal(bytesWords(640), "640 bytes");
  assert.equal(bytesWords(640 * 1024), "640 KB");
  assert.equal(bytesWords(12.4 * MB), "12 MB");
  assert.equal(bytesWords(4.8 * GB), "4.8 GB");
  assert.equal(bytesWords(5 * 1024 * GB), "5 TB");
  assert.equal(partWords(0.5 * GB, 4.8 * GB), "0.5 of 4.8 GB", "the part in the whole's unit");
  assert.equal(partWords(1.26 * GB, 4.8 * GB), "1.2 of 4.8 GB", "rounded down: it never claims more than was sent");
  assert.equal(partWords(312 * MB, 940 * MB), "312 of 940 MB");
  assert.equal(partWords(30 * MB, 2.3 * GB), "30 MB of 2.3 GB", "under a tenth of the unit, the part in its own");
  assert.equal(partWords(0, 2.3 * GB), "0 of 2.3 GB");
  assert.equal(rateWords(23.4 * MB), "23 MB/s");
  assert.equal(leftWords(0.2), "1 s left");
  assert.equal(leftWords(44.1), "45 s left");
  assert.equal(leftWords(61), "2 min left", "rounded up: it never promises too soon");
  assert.equal(leftWords(80 * 60), "1 h 20 min left");
  assert.equal(leftWords(3 * 3600), "3 h left");
  assert.equal(leftWords(3 * 86_400), "3 days left");
});

test("progress words by stage: queued, preparing, a small file's percentage, a big file's bytes and speed, saving", () => {
  assert.equal(progressWords({ stage: "queued", done: 0, total: GB }), "Waiting to upload…");
  assert.equal(progressWords({ stage: "preparing", done: 0.4 * GB, total: GB }), "Preparing… 40%");
  assert.equal(progressWords({ stage: "preparing", done: 0.4 * GB, total: 400 * GB, left: 1300 }), "Preparing… 0% · 22 min left", "a file that takes minutes to prepare says how long");
  assert.equal(progressWords({ stage: "preparing", done: 0.4 * GB, total: GB, left: 2 }), "Preparing… 40%", "not for a few seconds");
  assert.equal(progressWords({ stage: "sending", done: 4.2 * MB, total: 10 * MB }), "Uploading 42%");
  assert.equal(progressWords({ stage: "sending", done: 1.2 * GB, total: 4.8 * GB, rate: 23 * MB, left: 170 }), "1.2 of 4.8 GB · 23 MB/s · 3 min left");
  assert.equal(progressWords({ stage: "sending", done: 1.2 * GB, total: 4.8 * GB, rate: null, left: null }), "1.2 of 4.8 GB", "no speed or time before they are known");
  assert.equal(progressWords({ stage: "saving", done: GB, total: GB }), "Saving…");
});

test("the tray's line for a file: offline (a big file starts again from the beginning), restarted, sending", () => {
  const big = { id: "f", kind: "file" as const, name: "film.mov", size: 4.8 * GB, status: "uploading" as const, progress: 0.25, stage: "sending" as const, sent: 1.2 * GB, rate: 23 * MB, left: 170 };
  assert.equal(fileProgressWords(big, true), "1.2 of 4.8 GB · 23 MB/s · 3 min left");
  assert.equal(fileProgressWords(big, false), "Offline: it starts again from the beginning when you are back");
  assert.equal(fileProgressWords({ ...big, size: 2 * MB, sent: MB, progress: 0.5 }, false), "Uploads when online", "a small file just waits");
  assert.equal(fileProgressWords({ ...big, restarted: true, sent: 0, progress: 0, rate: null, left: null }, true), "Starting again from the beginning…");
  assert.equal(fileProgressWords({ ...big, stage: "preparing", sent: 2.4 * GB, progress: 0.5, left: null }, true), "Preparing… 50%");
  assert.equal(fileProgressWords({ ...big, stage: "preparing", sent: 2.4 * GB, progress: 0.5, left: 200 }, true), "Preparing… 50% · 4 min left");
  assert.equal(fileProgressWords({ ...big, progress: 1, sent: big.size }, true), "Saving…");
  assert.equal(fileProgressWords({ ...big, queued: true }, true), "Waiting to upload…");
  // Every byte arrived and Briefcase stores a large file for a while (the website answered 202): said, and offline it
  // still finishes (the website goes on saving it; the page asks again when it is back).
  assert.equal(fileProgressWords({ ...big, stage: "saving", progress: 1, sent: big.size }, true), "Saving… this can take a few minutes");
  assert.equal(fileProgressWords({ ...big, stage: "saving", progress: 1, sent: big.size }, false), "Offline: it finishes when you are back");
  assert.ok(BIG_FILE_BYTES <= 64 * MB && BIG_FILE_BYTES >= 8 * MB, "bytes and speed only where a percentage says too little");
});

test("the rate: nothing for two seconds, then smoothed (a burst does not swing it), and a reset after a restart", () => {
  const meter = new RateMeter();
  let now = 0;
  let sent = 0;
  const tick = (seconds: number, bytesPerSecond: number) => {
    for (let i = 0; i < seconds * 4; i++) {
      now += 250;
      sent += bytesPerSecond / 4;
      meter.note(sent, now);
    }
  };
  meter.note(0, now);
  tick(1.5, 10 * MB);
  assert.equal(meter.rate(), null, "too early to say");
  tick(2, 10 * MB);
  assert.ok(Math.abs(meter.rate()! - 10 * MB) < 0.5 * MB, `steady 10 MB/s reads as ${(meter.rate()! / MB).toFixed(1)}`);
  tick(0.5, 100 * MB);
  assert.ok(meter.rate()! < 30 * MB, `a half-second burst moves it only a little (${(meter.rate()! / MB).toFixed(1)} MB/s)`);
  const total = sent + 100 * MB;
  assert.ok(meter.left(total)! > 3 && meter.left(total)! < 20, `time left from the smoothed rate (${meter.left(total)!.toFixed(1)} s)`);
  meter.note(0, now + 250);
  assert.equal(meter.rate(), null, "back at the first byte (a restart): no estimate until there is one again");
});
