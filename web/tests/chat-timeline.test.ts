/** Thread rows (lib/chat/timeline.ts): day dividers, the New messages divider, and runs of one sender. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { buildTimeline, dayLabel, type TimelineEntry } from "../lib/chat/timeline";

const TZ = "Asia/Kolkata";
const now = new Date("2026-10-17T12:00:00+05:30");
const entry = (key: string, sender: string, at: string): TimelineEntry<null> => ({ key, senderKey: sender, at, value: null });

test("day labels: Today, Yesterday, the weekday and date, and the year when it differs", () => {
  assert.equal(dayLabel(new Date("2026-10-17T09:00:00+05:30"), { now, timeZone: TZ }), "Today");
  assert.equal(dayLabel(new Date("2026-10-16T23:59:00+05:30"), { now, timeZone: TZ }), "Yesterday");
  assert.equal(dayLabel(new Date("2026-10-12T10:00:00+05:30"), { now, timeZone: TZ }), "Monday, Oct 12");
  assert.equal(dayLabel(new Date("2025-12-31T10:00:00+05:30"), { now, timeZone: TZ }), "Wednesday, Dec 31, 2025");
  // Midnight in the reader's zone decides the day, not UTC's.
  assert.equal(dayLabel(new Date("2026-10-16T19:00:00Z"), { now, timeZone: TZ }), "Today");
});

test("one sender within five minutes is one group; a pause, another sender or a new day starts a new one", () => {
  const rows = buildTimeline([
    entry("000", "ada", "2026-10-16T10:00:00+05:30"),
    entry("001", "ada", "2026-10-16T10:04:00+05:30"),
    entry("002", "ada", "2026-10-16T10:10:00+05:30"),
    entry("003", "scout", "2026-10-16T10:10:30+05:30"),
    entry("004", "scout", "2026-10-17T09:00:00+05:30"),
  ], { now, timeZone: TZ });
  assert.deepEqual(rows.map(row => (row.type === "entry" ? `${row.key}:${row.first ? "F" : ""}${row.last ? "L" : ""}` : `${row.type}:${row.type === "day" ? row.label : ""}`)), [
    "day:Yesterday",
    "000:F",
    "001:L",
    "002:FL",
    "003:FL",
    "day:Today",
    "004:FL",
  ]);
});

test("the New messages divider sits before the first unread and breaks a run", () => {
  const rows = buildTimeline([
    entry("000", "scout", "2026-10-17T10:00:00+05:30"),
    entry("001", "scout", "2026-10-17T10:01:00+05:30"),
    entry("002", "scout", "2026-10-17T10:02:00+05:30"),
  ], { now, timeZone: TZ, firstUnreadKey: "001", unreadCount: 2 });
  assert.deepEqual(rows.map(row => (row.type === "entry" ? `${row.key}:${row.first ? "F" : ""}${row.last ? "L" : ""}` : row.type === "unread" ? `unread:${row.count}` : row.type)), [
    "day",
    "000:FL",
    "unread:2",
    "001:F",
    "002:L",
  ]);
});

test("no messages, no rows", () => {
  assert.deepEqual(buildTimeline([], { now, timeZone: TZ }), []);
});
