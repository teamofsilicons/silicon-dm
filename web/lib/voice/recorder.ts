/**
 * The microphone's state machine (spec §6.5), pure: recording a voice note or dictating, from asking for the
 * microphone to handing the recording over. The browser side (lib/voice/microphone.ts) feeds it events; the composer's
 * microphone (components/chat/voice/composer-voice.tsx) draws it.
 *
 *   idle ─start─▶ asking ─granted─▶ recording ─stop─▶ stopping ─stopped─▶ idle (a note goes to the tray)
 *                    │                  │  ▲tick              └──────────▶ transcribing ─transcribed─▶ idle (dictation)
 *                    └─denied─▶ error   └─cancel (Esc) ─▶ idle            any step ─failed─▶ error ─dismiss─▶ idle
 *                                                                        error (dictation) ─retranscribe─▶ transcribing
 *
 * A recording stops by itself at MAX_RECORDING_MS (and sends, as ✓ would), with a warning from WARN_BEFORE_MS before.
 */

export type RecordMode = "note" | "dictate";

export interface RecorderProblem {
  code: string;
  /** What happened and why. */
  message: string;
  /** What to do. */
  hint?: string;
}

export type RecorderState =
  | { phase: "idle" }
  | { phase: "asking"; mode: RecordMode }
  | { phase: "recording"; mode: RecordMode; elapsedMs: number; level: number }
  | { phase: "stopping"; mode: RecordMode; elapsedMs: number }
  | { phase: "transcribing"; mode: RecordMode; elapsedMs: number }
  | { phase: "error"; mode: RecordMode; problem: RecorderProblem };

export type RecorderEvent =
  | { type: "start"; mode: RecordMode }
  | { type: "granted" }
  | { type: "denied"; problem: RecorderProblem }
  | { type: "tick"; elapsedMs: number; level: number }
  | { type: "stop" }
  | { type: "cancel" }
  | { type: "stopped" }
  | { type: "transcribed" }
  | { type: "failed"; problem: RecorderProblem }
  /** Try again on a dictation whose transcription failed: the same recording is sent again. */
  | { type: "retranscribe" }
  | { type: "dismiss" };

/** Ten minutes: the longest voice note or dictation. */
export const MAX_RECORDING_MS = 10 * 60_000;
/** The warning shows this long before the end. */
export const WARN_BEFORE_MS = 30_000;

export const IDLE_RECORDER: RecorderState = { phase: "idle" };

export function recorderReducer(state: RecorderState, event: RecorderEvent): RecorderState {
  switch (event.type) {
    case "start":
      return state.phase === "idle" || state.phase === "error" ? { phase: "asking", mode: event.mode } : state;
    case "granted":
      return state.phase === "asking" ? { phase: "recording", mode: state.mode, elapsedMs: 0, level: 0 } : state;
    case "denied":
      return state.phase === "asking" ? { phase: "error", mode: state.mode, problem: event.problem } : state;
    case "tick":
      if (state.phase !== "recording") return state;
      return { ...state, elapsedMs: Math.max(state.elapsedMs, event.elapsedMs), level: Math.max(0, Math.min(1, event.level)) };
    case "stop":
      return state.phase === "recording" ? { phase: "stopping", mode: state.mode, elapsedMs: state.elapsedMs } : state;
    case "cancel":
      return state.phase === "idle" ? state : IDLE_RECORDER;
    case "stopped":
      if (state.phase !== "stopping") return state;
      return state.mode === "dictate" ? { phase: "transcribing", mode: "dictate", elapsedMs: state.elapsedMs } : IDLE_RECORDER;
    case "transcribed":
      return state.phase === "transcribing" ? IDLE_RECORDER : state;
    case "failed":
      return state.phase === "idle" ? state : { phase: "error", mode: "mode" in state ? state.mode : "note", problem: event.problem };
    case "retranscribe":
      return state.phase === "error" && state.mode === "dictate" ? { phase: "transcribing", mode: "dictate", elapsedMs: 0 } : state;
    case "dismiss":
      return state.phase === "error" ? IDLE_RECORDER : state;
  }
}

/** The microphone is in use (the overlay shows, Listen mode holds its tongue, presence says so). */
export const recorderBusy = (state: RecorderState) => state.phase === "asking" || state.phase === "recording" || state.phase === "stopping" || state.phase === "transcribing";

/** Time left before the recording stops by itself, when the warning should show; else null. */
export function timeLeftWarning(elapsedMs: number, maxMs = MAX_RECORDING_MS, warnMs = WARN_BEFORE_MS): number | null {
  const left = maxMs - elapsedMs;
  return left <= warnMs ? Math.max(0, left) : null;
}

/**
 * The recording format: Opus in WebM where the browser records it (Chrome, Edge, Firefox), AAC in MP4 on Safari, else
 * whatever the browser chooses. Both are formats the service transcribes and Briefcase stores.
 */
export function pickRecordingType(isTypeSupported: (type: string) => boolean): string {
  for (const type of ["audio/webm;codecs=opus", "audio/webm", "audio/mp4;codecs=mp4a.40.2", "audio/mp4", "audio/ogg;codecs=opus"]) {
    try {
      if (isTypeSupported(type)) return type;
    } catch {
      // A browser that throws on the question does not record that type.
    }
  }
  return "";
}

/** The media type without parameters (`audio/webm;codecs=opus` → `audio/webm`), as uploads and transcription take it. */
export function baseType(type: string | null | undefined, fallback = "audio/webm"): string {
  const base = (type ?? "").split(";")[0]?.trim().toLowerCase();
  return base || fallback;
}

/** A voice note's file name: `voice-note-2026-10-17-1532.webm`. */
export function noteFileName(type: string, at: Date = new Date()): string {
  const base = baseType(type);
  const extension = base === "audio/mp4" || base === "audio/x-m4a" || base === "audio/aac" ? "m4a" : base === "audio/ogg" ? "ogg" : base === "audio/wav" ? "wav" : base === "audio/mpeg" ? "mp3" : "webm";
  const two = (value: number) => String(value).padStart(2, "0");
  return `voice-note-${at.getFullYear()}-${two(at.getMonth() + 1)}-${two(at.getDate())}-${two(at.getHours())}${two(at.getMinutes())}.${extension}`;
}

/**
 * What a failed `getUserMedia` or MediaRecorder means for the Carbon: what happened, why, and what to do.
 * `name` is the DOMException's name.
 */
export function microphoneProblem(name: string | null | undefined, detail?: string): RecorderProblem {
  switch (name) {
    case "NotAllowedError":
    case "PermissionDeniedError":
      return {
        code: "microphone_blocked",
        message: "The microphone is blocked for this site, so nothing could be recorded.",
        hint: "Allow the microphone in the browser's site settings (the icon at the start of the address bar), then try again.",
      };
    case "NotFoundError":
    case "DevicesNotFoundError":
    case "OverconstrainedError":
      return { code: "microphone_missing", message: "No microphone was found.", hint: "Connect a microphone (or choose one in the system's sound settings), then try again." };
    case "NotReadableError":
    case "TrackStartError":
    case "AbortError":
      return { code: "microphone_busy", message: "The microphone could not be started: another app may be using it.", hint: "Close the other app's recording, then try again." };
    case "SecurityError":
      return { code: "microphone_insecure", message: "This page may not use the microphone.", hint: "Open DM over https (or on this computer's own address), then try again." };
    case "unsupported":
    case "NotSupportedError":
      return { code: "recording_unsupported", message: "This browser cannot record audio here.", hint: "Use a recent Chrome, Edge, Firefox or Safari, with a microphone connected." };
    default:
      return { code: "recording_failed", message: `The recording failed${detail ? `: ${detail}` : ""}.`, hint: "Try again. If it keeps failing, reload the page." };
  }
}
