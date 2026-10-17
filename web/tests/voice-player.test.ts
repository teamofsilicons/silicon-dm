/**
 * What the player says when audio cannot be had (lib/voice/player.ts `playbackError`), and where a voice note is read
 * from (lib/voice/sources.ts `noteSource`): the page reads bytes only from this site, anything else plays as it is.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { ApiError } from "../lib/errors";
import { playbackError } from "../lib/voice/player";
import { noteSource } from "../lib/voice/sources";

test("a request that never got an answer is told in plain words, not as the request", () => {
  const network = new ApiError({ status: 0, code: "network_error", message: "Could not reach this site for GET /api/v1/conversations/c%3Aa/messages/01a/speech: Failed to fetch.", hint: "Check your connection, then try again.", method: "GET", path: "/api/v1/conversations/c%3Aa/messages/01a/speech" });
  const said = playbackError(network);
  assert.equal(said.code, "network_error", "still a network error (the offline copy is used for it)");
  assert.ok(said.isNetwork);
  assert.doesNotMatch(said.message, /GET|\/api\//, "no method or path in what the Carbon reads");
  assert.match(said.message, /^The audio could not be loaded/);
  assert.ok(said.hint && /try again/i.test(said.hint));
});

test("the service's own words pass through untouched", () => {
  const limited = new ApiError({ status: 429, code: "speech_rate_limited", message: "You have rendered 30 messages in the last minute.", hint: "Wait a moment.", retryAfter: 20 });
  assert.equal(playbackError(limited), limited);
  const gone = new ApiError({ status: 410, code: "message_deleted", message: "This message was deleted; its content, speech included, is gone." });
  assert.equal(playbackError(gone), gone);
});

test("a voice note is read through this site when it is a Briefcase file; other https links play as they are", () => {
  const briefcase = noteSource("https://briefcase.teamofsilicons.com/api/v1/public/0193f3b2-7c41-7a3e-8f00-000000000000/apps/dm/voice-note.webm?view=inline");
  assert.ok(briefcase);
  assert.match(briefcase.url, /^\/api\/uploads\/file\?url=/);
  assert.equal(briefcase.fetchable, true);
  const elsewhere = noteSource("https://cdn.example.com/note.ogg");
  assert.deepEqual(elsewhere, { url: "https://cdn.example.com/note.ogg", fetchable: false }, "the page may not fetch it (connect-src 'self'); the element plays it");
  assert.equal(noteSource("http://cdn.example.com/note.ogg"), null, "never over plain http");
  assert.equal(noteSource("https://user:secret@cdn.example.com/note.ogg"), null, "never with credentials in the link");
  assert.equal(noteSource("javascript:alert(1)"), null);
  assert.equal(noteSource("not a url"), null);
});
