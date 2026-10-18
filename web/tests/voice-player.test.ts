/**
 * What the player says when audio cannot be had (lib/voice/player.ts `playbackError`, in lib/voice/words.ts's words,
 * as transcription and dictation failures are), and where a voice note is read from (lib/voice/sources.ts
 * `noteSource`): the page reads bytes only from this site, anything else plays as it is. A busy voice is asked again
 * quietly (`whileBusy`) before a failure shows.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { ApiError } from "../lib/errors";
import { playbackError } from "../lib/voice/player";
import { BUSY_RETRIES, busyWait, noteSource, voiceBusy, whileBusy } from "../lib/voice/sources";
import { voiceWords } from "../lib/voice/words";

test("a request that never got an answer is told in plain words, not as the request", () => {
  const network = new ApiError({ status: 0, code: "network_error", message: "Could not reach this site for GET /api/v1/conversations/c%3Aa/messages/01a/speech: Failed to fetch.", hint: "Check your connection, then try again.", method: "GET", path: "/api/v1/conversations/c%3Aa/messages/01a/speech" });
  const said = playbackError(network);
  assert.equal(said.code, "network_error", "still a network error (the offline copy is used for it)");
  assert.ok(said.isNetwork);
  assert.doesNotMatch(said.message, /GET|\/api\//, "no method or path in what the Carbon reads");
  assert.match(said.message, /^The audio could not be loaded/);
  assert.ok(said.hint && /try again/i.test(said.hint));
});

test("the service's own words pass through where they already read well for a Carbon", () => {
  const gone = new ApiError({ status: 410, code: "message_deleted", message: "This message was deleted; its content, speech included, is gone." });
  assert.equal(playbackError(gone), gone);
  const nothing = new ApiError({ status: 404, code: "nothing_to_speak", message: "This message has no speech and no text to read aloud.", hint: "Play its voice note or open its attachments instead." });
  assert.equal(playbackError(nothing), nothing);
});

/* The service's words for these name routes, settings and requests; a Carbon reads what happened, why, what to do. */
const developerWords = /GET |POST |\/api\/|DM_[A-Z_]+|Retry-After|request|Deepgram|operator|details\./;

test("listening failures are told in a Carbon's words, keeping the service's code", () => {
  const rejected = new ApiError({ status: 422, code: "speech_rejected", message: "Deepgram refused the voice; retrying the same script and voice will not help.", hint: "Change the script (plain words, cues in square brackets) or choose another voice from GET /api/v1/voices." });
  const heard = playbackError(rejected);
  assert.equal(heard.code, "speech_rejected");
  assert.match(heard.message, /^The voice could not say these words/);
  assert.match(heard.hint ?? "", /Settings → Voice/);
  assert.match(playbackError(rejected, "sample").message, /^This voice could not say its sample/);

  const unavailable = new ApiError({ status: 503, code: "speech_unavailable", message: "Deepgram did not answer. Nothing was lost; retry in a few seconds.", hint: "Retry the same request after the Retry-After delay.", retryAfter: 3 });
  assert.equal(playbackError(unavailable).hint, "Nothing is lost. Try again in 3 seconds.");

  const off = new ApiError({ status: 503, code: "speech_not_configured", message: "This DM service has no speech provider configured (DM_DEEPGRAM_API_KEY is not set), so it cannot render speech or transcribe audio.", hint: "Send text instead; speech works once the operator of this DM sets DM_DEEPGRAM_API_KEY. GET /api/v1/accounts says whether speech is enabled." });
  assert.equal(playbackError(off).message, "Listening is not set up on this DM service yet.");

  const limited = new ApiError({ status: 429, code: "speech_rate_limited", message: "This account already had 30 speech renders this minute, the most DM allows.", hint: "Retry in 20 seconds.", retryAfter: 20, details: { limit: 30, retry_after_seconds: 20 } });
  assert.equal(playbackError(limited).message, "DM makes at most 30 new recordings a minute for each account, and you reached that.");
  assert.equal(playbackError(limited).hint, "Try again in 20 seconds. Anything you already heard still plays.");
  assert.equal(playbackError(limited).retryAfter, 20, "the wait is kept for whoever retries");

  const budget = new ApiError({ status: 429, code: "speech_budget_exceeded", message: "This would take the account past its 200000 characters of speech for today (UTC).", hint: "Send text instead, or retry after midnight UTC (5000 seconds).", details: { limit: 200000, retry_after_seconds: 5000 } });
  assert.equal(playbackError(budget).message, "DM reads at most 200,000 characters aloud a day for each account, and you reached that.");

  const broken = new ApiError({ status: 502, code: "server_unavailable", message: "GET /v1/conversations/x/messages/0a1/speech failed: the service could not be reached (HTTP 502), and the answer had no error details.", hint: "Wait a moment and try again." });
  assert.match(playbackError(broken).message, /^The audio could not be loaded/);

  for (const error of [rejected, unavailable, off, limited, budget, broken]) {
    for (const kind of ["speech", "note", "sample"] as const) {
      const said = playbackError(error, kind);
      assert.doesNotMatch(`${said.message} ${said.hint ?? ""}`, developerWords, `${error.code} (${kind}) is in a Carbon's words`);
    }
  }
});

test("a voice message's words and dictation fail in a Carbon's words too", () => {
  const unavailable = new ApiError({ status: 503, code: "speech_unavailable", message: "Deepgram could not transcribe the audio right now. Nothing was stored; retry in a few seconds.", hint: "Retry the same request after the Retry-After delay." });
  assert.match(voiceWords(unavailable, "transcribe").message, /^The recording could not be turned into words/);
  const limited = new ApiError({ status: 429, code: "transcription_rate_limited", message: "This account already asked for 20 transcriptions this minute, the most DM allows.", hint: "Retry in 9 seconds.", retryAfter: 9, details: { limit: 20 } });
  assert.equal(voiceWords(limited, "dictate").message, "DM turns at most 20 recordings into words a minute for each account, and you reached that.");
  assert.equal(voiceWords(limited, "dictate").hint, "Try again in 9 seconds.");
  const off = new ApiError({ status: 503, code: "speech_not_configured", message: "… DM_DEEPGRAM_API_KEY is not set …", hint: "… the operator …" });
  assert.equal(voiceWords(off, "dictate").hint, "Type instead.");
  // A voice message's words that failed: it went without them, so nothing asks to try again.
  assert.equal(voiceWords(off, "transcribe").hint, "The voice message went without its words; it plays as recorded.");
  assert.equal(voiceWords(unavailable, "transcribe").hint, "The voice message went without its words; it plays as recorded.");
  const large = new ApiError({ status: 413, code: "audio_too_large", message: "The audio is 120 MiB; DM transcribes at most 100 MiB.", hint: "Set DM_TRANSCRIBE_MAX_BYTES." });
  assert.equal(voiceWords(large, "dictate").hint, "Dictate a shorter part, then the rest.");
  assert.equal(voiceWords(large, "transcribe").message, "The recording is too long to be turned into words.");
  for (const error of [unavailable, limited, off, large]) {
    for (const task of ["transcribe", "dictate"] as const) {
      const said = voiceWords(error, task);
      assert.doesNotMatch(`${said.message} ${said.hint ?? ""}`, developerWords, `${error.code} (${task}) is in a Carbon's words`);
    }
  }
  // Looking at and choosing voices: a request that never got an answer, and a voice no longer offered.
  const lost = new ApiError({ status: 0, code: "network_error", message: "Could not reach this site for GET /api/v1/voices/si:scout: Failed to fetch.", hint: "Check your connection, then try again." });
  assert.equal(voiceWords(lost, "voices").message, "DM could not be reached.");
  const gone = new ApiError({ status: 422, code: "voice_not_found", message: "There is no voice `george`. The voices are: mark, brittney.", hint: "Use one of the keys in details.voices (GET /api/v1/voices describes each)." });
  assert.equal(voiceWords(gone, "voices").message, "That voice is not one DM offers any more.");
  const forbidden = new ApiError({ status: 403, code: "voice_change_forbidden", message: "You cannot change the voice of si:atlas.", hint: "Only si:atlas itself or its custodian c:bo can choose its voice; ask them." });
  assert.equal(voiceWords(forbidden, "voices"), forbidden, "the service's own words, which name who may");
  // Codes it does not know keep the service's words.
  const other = new ApiError({ status: 403, code: "read_only_view", message: "A custodian's view is read only.", hint: "Sign in as the Silicon to do that." });
  assert.equal(voiceWords(other, "transcribe"), other);
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

test("a busy voice (503 speech_unavailable) is asked again after the wait it asks for, a few times, before it fails", async () => {
  const busy = new ApiError({ status: 503, code: "speech_unavailable", message: "Speech is busy.", retryAfter: 0 });
  assert.equal(voiceBusy(busy), true);
  assert.equal(voiceBusy(new ApiError({ status: 503, code: "server_unavailable", message: "Down." })), false, "only the voice being busy");
  assert.equal(voiceBusy(new ApiError({ status: 429, code: "speech_rate_limited", message: "Limit." })), false, "a limit is said at once");
  assert.equal(busyWait(new ApiError({ status: 503, code: "speech_unavailable", message: "", retryAfter: 5 })), 5000);
  assert.equal(busyWait(new ApiError({ status: 503, code: "speech_unavailable", message: "" })), 5000, "5 s when it said nothing");
  assert.equal(busyWait(new ApiError({ status: 503, code: "speech_unavailable", message: "", retryAfter: 120 })), 10_000, "never more than 10 s at a time");
  assert.equal(busyWait(busy), 1000, "never at once");

  let calls = 0;
  const started = Date.now();
  const audio = await whileBusy(async () => {
    calls += 1;
    if (calls < 3) throw busy;
    return "audio";
  });
  assert.equal(audio, "audio");
  assert.equal(calls, 3, "two busy answers, then the audio");
  assert.ok(Date.now() - started >= 1900, "it waited between the tries");

  calls = 0;
  await assert.rejects(whileBusy(async () => {
    calls += 1;
    throw busy;
  }), (error: unknown) => error === busy);
  assert.equal(calls, BUSY_RETRIES + 1, "bounded: then the failure shows");

  calls = 0;
  const refused = new ApiError({ status: 422, code: "speech_rejected", message: "No." });
  await assert.rejects(whileBusy(async () => {
    calls += 1;
    throw refused;
  }), (error: unknown) => error === refused);
  assert.equal(calls, 1, "a refusal is not asked again");

  const controller = new AbortController();
  const slow = new ApiError({ status: 503, code: "speech_unavailable", message: "Speech is busy.", retryAfter: 5 });
  const waiting = whileBusy(async () => {
    throw slow;
  }, controller.signal);
  setTimeout(() => controller.abort(), 50);
  const before = Date.now();
  await assert.rejects(waiting, (error: unknown) => (error as { name?: string }).name === "AbortError", "a press elsewhere stops the wait");
  assert.ok(Date.now() - before < 2000, "at once, not after the 5 s");
});
