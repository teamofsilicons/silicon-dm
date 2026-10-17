/** The microphone's state machine (lib/voice/recorder.ts): record, cancel, stop and hand over, dictate, fail and say why. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { baseType, IDLE_RECORDER, MAX_RECORDING_MS, microphoneProblem, noteFileName, pickRecordingType, recorderBusy, recorderReducer, timeLeftWarning, WARN_BEFORE_MS, type RecorderEvent, type RecorderState } from "../lib/voice/recorder";

const run = (events: RecorderEvent[], from: RecorderState = IDLE_RECORDER) => events.reduce(recorderReducer, from);

test("a voice note: ask, record, stop, hand over (back to idle)", () => {
  let state = run([{ type: "start", mode: "note" }]);
  assert.deepEqual(state, { phase: "asking", mode: "note" });
  assert.equal(recorderBusy(state), true);
  state = run([{ type: "granted" }, { type: "tick", elapsedMs: 1200, level: 0.4 }], state);
  assert.deepEqual(state, { phase: "recording", mode: "note", elapsedMs: 1200, level: 0.4 });
  state = run([{ type: "tick", elapsedMs: 900, level: 3 }], state);
  assert.equal(state.phase === "recording" && state.elapsedMs, 1200, "time never goes back");
  assert.equal(state.phase === "recording" && state.level, 1, "the level is clamped");
  state = run([{ type: "stop" }], state);
  assert.deepEqual(state, { phase: "stopping", mode: "note", elapsedMs: 1200 });
  state = run([{ type: "stopped" }], state);
  assert.deepEqual(state, IDLE_RECORDER);
  assert.equal(recorderBusy(state), false);
});

test("dictation goes through transcribing before it is done", () => {
  const state = run([{ type: "start", mode: "dictate" }, { type: "granted" }, { type: "tick", elapsedMs: 3000, level: 0.2 }, { type: "stop" }, { type: "stopped" }]);
  assert.deepEqual(state, { phase: "transcribing", mode: "dictate", elapsedMs: 3000 });
  assert.equal(recorderBusy(state), true);
  assert.deepEqual(recorderReducer(state, { type: "transcribed" }), IDLE_RECORDER);
});

test("Esc cancels from any step; a refused microphone is an error with what to do", () => {
  for (const events of [[{ type: "start", mode: "note" }], [{ type: "start", mode: "note" }, { type: "granted" }], [{ type: "start", mode: "dictate" }, { type: "granted" }, { type: "stop" }, { type: "stopped" }]] as RecorderEvent[][]) {
    assert.deepEqual(run([...events, { type: "cancel" }]), IDLE_RECORDER);
  }
  const problem = microphoneProblem("NotAllowedError");
  const denied = run([{ type: "start", mode: "note" }, { type: "denied", problem }]);
  assert.equal(denied.phase, "error");
  assert.equal(denied.phase === "error" && denied.problem.code, "microphone_blocked");
  assert.match(problem.hint ?? "", /site settings/);
  assert.deepEqual(recorderReducer(denied, { type: "dismiss" }), IDLE_RECORDER);
  assert.deepEqual(recorderReducer(denied, { type: "start", mode: "dictate" }), { phase: "asking", mode: "dictate" }, "trying again starts over");
});

test("Try again on a failed dictation sends the same recording again; a voice note's problem starts over", () => {
  const failure = { code: "speech_unavailable", message: "The speech provider did not answer.", hint: "Try again in a moment." };
  const dictation = run([{ type: "start", mode: "dictate" }, { type: "granted" }, { type: "stop" }, { type: "stopped" }, { type: "failed", problem: failure }]);
  assert.equal(dictation.phase, "error");
  assert.deepEqual(recorderReducer(dictation, { type: "retranscribe" }), { phase: "transcribing", mode: "dictate", elapsedMs: 0 });
  const note = run([{ type: "start", mode: "note" }, { type: "denied", problem: microphoneProblem("NotAllowedError") }]);
  assert.equal(recorderReducer(note, { type: "retranscribe" }), note, "a voice note has nothing to transcribe again");
  assert.equal(recorderReducer(IDLE_RECORDER, { type: "retranscribe" }), IDLE_RECORDER);
});

test("events out of turn change nothing", () => {
  assert.equal(recorderReducer(IDLE_RECORDER, { type: "stop" }), IDLE_RECORDER);
  assert.equal(recorderReducer(IDLE_RECORDER, { type: "granted" }), IDLE_RECORDER);
  assert.equal(recorderReducer(IDLE_RECORDER, { type: "failed", problem: microphoneProblem("x") }), IDLE_RECORDER);
  const recording = run([{ type: "start", mode: "note" }, { type: "granted" }]);
  assert.equal(recorderReducer(recording, { type: "start", mode: "dictate" }), recording, "a second start while recording is ignored");
  assert.equal(recorderReducer(recording, { type: "transcribed" }), recording);
  const failed = recorderReducer(recording, { type: "failed", problem: microphoneProblem("NotReadableError") });
  assert.equal(failed.phase === "error" && failed.problem.code, "microphone_busy");
});

test("ten minutes at most, with a warning in the last thirty seconds", () => {
  assert.equal(MAX_RECORDING_MS, 600_000);
  assert.equal(timeLeftWarning(0), null);
  assert.equal(timeLeftWarning(MAX_RECORDING_MS - WARN_BEFORE_MS - 1), null);
  assert.equal(timeLeftWarning(MAX_RECORDING_MS - 12_000), 12_000);
  assert.equal(timeLeftWarning(MAX_RECORDING_MS + 5000), 0);
});

test("Opus in WebM where supported, MP4 on Safari, the browser's choice otherwise", () => {
  assert.equal(pickRecordingType(type => type === "audio/webm;codecs=opus" || type === "audio/mp4"), "audio/webm;codecs=opus");
  assert.equal(pickRecordingType(type => type === "audio/mp4"), "audio/mp4");
  assert.equal(pickRecordingType(() => false), "");
  assert.equal(pickRecordingType(() => { throw new Error("no"); }), "");
  assert.equal(baseType("audio/webm;codecs=opus"), "audio/webm");
  assert.equal(baseType(""), "audio/webm");
  assert.equal(baseType(" Audio/MP4 "), "audio/mp4");
});

test("voice notes are named for when they were made", () => {
  const at = new Date(2026, 9, 17, 15, 2);
  assert.equal(noteFileName("audio/webm;codecs=opus", at), "voice-note-2026-10-17-1502.webm");
  assert.equal(noteFileName("audio/mp4", at), "voice-note-2026-10-17-1502.m4a");
  assert.equal(noteFileName("audio/ogg;codecs=opus", at), "voice-note-2026-10-17-1502.ogg");
});

test("every microphone failure says what happened and what to do", () => {
  for (const name of ["NotAllowedError", "NotFoundError", "NotReadableError", "SecurityError", "unsupported", "Weird"]) {
    const problem = microphoneProblem(name, "boom");
    assert.ok(problem.message.endsWith("."), name);
    assert.ok(problem.hint && problem.hint.length > 10, name);
  }
  assert.match(microphoneProblem("Weird", "boom").message, /boom/);
});
