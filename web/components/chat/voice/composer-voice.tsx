"use client";

/**
 * The composer's microphone (spec §6.5). Click it or press ⌘⇧M to record a voice message, ⌘⇧D (or /dictate) to dictate.
 * While it records, a bar covers the composer's row: the time, a live level meter, Esc or ✕ to cancel, Enter, ✓ or the
 * same shortcut again to stop and send (a voice message) or insert the words (dictation). The keyboard stays in the bar
 * while it is open (Tab goes between ✕ and ✓), since what it covers cannot be used meanwhile.
 *
 * A voice message goes to the composer's tray at once (uploading), uploads into the Carbon's Briefcase and is
 * transcribed in parallel, then the composer sends it (bridge.sendWhenReady). Dictation is transcribed and typed at
 * the caret; nothing is uploaded. When uploads are off the microphone dictates only, and says why.
 *
 * The others see "recording a voice message…" (or "typing…" while dictating); Listen mode holds its tongue meanwhile,
 * and anything else playing pauses (the microphone would hear it). A recording stops by itself at ten minutes (and
 * sends), with a warning thirty seconds before. If the microphone goes away mid-recording, what was recorded waits in
 * the bar for ✓ or Esc.
 */
import { useCallback, useEffect, useLayoutEffect, useReducer, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useQueryClient } from "@tanstack/react-query";
import { Check, Mic, X } from "lucide-react";
import { TextShimmer } from "@/components/silicon-ui/text-shimmer/text-shimmer";
import { transcribe } from "@/lib/chat/api";
import { clockDuration, fileSize } from "@/lib/chat/format";
import { reportActivity } from "@/lib/chat/presence";
import { ApiError } from "@/lib/errors";
import { notify, notifyError } from "@/lib/notify";
import { chordFromEvent } from "@/lib/shortcuts/keys";
import { useShortcut, useShortcutKeys, useShortcuts } from "@/lib/shortcuts/react";
import { uploadFile } from "@/lib/upload/client";
import { useVoiceAvailability, type VoiceAvailability } from "@/lib/voice";
import { holdListening } from "@/lib/voice/listen-mode";
import { ensureUploadsConfig } from "@/lib/voice/availability";
import { pause as pausePlayback, playerState } from "@/lib/voice/player";
import { rememberNote } from "@/lib/voice/sources";
import { canRecord, MicrophoneError, openMicrophone, type MicrophoneSession, type Recording } from "@/lib/voice/microphone";
import { baseType, IDLE_RECORDER, MAX_RECORDING_MS, microphoneProblem, noteFileName, recorderBusy, recorderReducer, timeLeftWarning, WARN_BEFORE_MS, type RecordMode, type RecorderEvent, type RecorderProblem, type RecorderState } from "@/lib/voice/recorder";
import { IconAction } from "../common/icon-action";
import type { ComposerVoiceBridge } from "../composer/voice-bridge";
import styles from "./voice.module.css";

/** Twelve seconds of level history (as much as the bar is wide shows; older levels slide out on the left). */
const METER_BARS = 120;
const TICK_MS = 100;
/** An open dialog, menu or list keeps its own keys (Esc closes it), even while the microphone records. */
const LAYER = '[role="dialog"]:not([data-state="closed"]), [role="alertdialog"]:not([data-state="closed"]), [role="menu"]:not([data-state="closed"]), [role="listbox"]';

/** The composer's row around the microphone (the overlay covers it): the nearest ancestor holding the message field. */
function composerRow(from: HTMLElement): HTMLElement | null {
  let node = from.parentElement;
  while (node && node !== document.body && !node.querySelector("[data-composer-field]")) node = node.parentElement;
  return node && node !== document.body ? node : null;
}

const describe = (problem: Pick<RecorderProblem, "message" | "hint">) => [problem.message, problem.hint].filter(Boolean).join(" ");

const pauseFor = (milliseconds: number) => new Promise<void>(resolve => setTimeout(resolve, milliseconds));
/** A failure that may pass if asked again in a moment (the network, the service busy or restarting). */
const passing = (failure: ApiError) => failure.isNetwork || failure.status === 429 || failure.status >= 500;

/** Uploads the recording, asking again twice (after 1 s, then 3 s) when the failure may pass. */
async function uploadRecording(blob: Blob, options: Parameters<typeof uploadFile>[1]): Promise<Awaited<ReturnType<typeof uploadFile>>> {
  for (let attempt = 1; ; attempt += 1) {
    try {
      return await uploadFile(blob, options);
    } catch (error) {
      const failure = ApiError.from(error);
      if (attempt >= 3 || !passing(failure)) throw failure;
      await pauseFor(attempt === 1 ? 1000 : 3000);
    }
  }
}

/**
 * After ✓ on a voice message: a tray item at once, then the upload and the transcription side by side, then ready
 * (the composer sends it) or failed (with the reason). A recording is never lost to a failed upload: the toast that says
 * so offers Try again, which uploads the same recording again.
 */
async function handOver(bridge: ComposerVoiceBridge, recording: Recording, voice: VoiceAvailability, replaces?: string): Promise<void> {
  const id = `voice-${typeof crypto.randomUUID === "function" ? crypto.randomUUID() : Date.now().toString(36)}`;
  const type = baseType(recording.type);
  if (replaces) bridge.dispatchTray({ type: "remove", id: replaces });
  bridge.dispatchTray({ type: "add", item: { id, kind: "voice", status: "uploading", progress: 0 } });
  bridge.sendWhenReady();
  if (!recording.blob.size) {
    bridge.dispatchTray({ type: "fail", id, error: "Nothing was recorded: the microphone gave no sound. Check the microphone, then record again." });
    return;
  }
  if (voice.maxUploadBytes && recording.blob.size > voice.maxUploadBytes) {
    const problem = { message: `The voice message is ${fileSize(recording.blob.size)}; uploads here take at most ${fileSize(voice.maxUploadBytes)}.`, hint: "Record a shorter one." };
    bridge.dispatchTray({ type: "fail", id, error: describe(problem) });
    return;
  }
  reportActivity("transcribing_voice");
  const name = noteFileName(recording.type);
  const words = voice.transcripts
    ? transcribe(recording.blob, { contentType: type })
        .then(result => result.transcript.trim() || null)
        .catch((error: unknown) => {
          const failure = ApiError.from(error);
          // Speech switched off is said elsewhere; anything else, quietly: the message goes without its words.
          if (failure.code !== "speech_not_configured") notify.warning("Sent without a transcript", describe(failure));
          return null;
        })
    : Promise.resolve(null);
  try {
    const file = await uploadRecording(recording.blob, {
      name,
      contentType: type,
      onProgress: (sent, total) => bridge.dispatchTray({ type: "progress", id, progress: total ? sent / total : 0 }),
    });
    // Its own bubble draws and plays from the recording at once; nothing is downloaded again.
    rememberNote(file.url, recording.blob);
    const transcript = await words;
    bridge.dispatchTray({
      type: "ready",
      id,
      voice: { permanent_url: file.url, name: file.name, content_type: file.content_type || type, size: file.size, duration_milliseconds: recording.durationMs },
      ...(transcript ? { transcript } : {}),
    });
  } catch (error) {
    const failure = ApiError.from(error);
    bridge.dispatchTray({ type: "fail", id, error: describe(failure) });
    const toast = notifyError(failure, "The voice message was not uploaded");
    // The recording is still here: Try again uploads it once more (the failed item makes way for the new one).
    notify.update(toast, { duration: Infinity, action: { label: "Try again", onClick: () => void handOver(bridge, recording, voice, id) } });
  } finally {
    reportActivity(null);
  }
}

/** Writes a style only when it changes (the bar follows the composer's row every frame). */
function setStyle(node: HTMLElement, name: "width" | "minHeight" | "transform", value: string): void {
  if (node.style[name] !== value) node.style[name] = value;
}

export function ComposerVoice({ bridge }: { bridge?: ComposerVoiceBridge }) {
  const voice = useVoiceAvailability();
  const queryClient = useQueryClient();
  const [state, dispatchState] = useReducer(recorderReducer, IDLE_RECORDER);
  // The same state machine, mirrored synchronously, so a double press or a key repeat cannot start twice.
  const machine = useRef<RecorderState>(IDLE_RECORDER);
  const dispatch = useCallback((event: RecorderEvent) => {
    machine.current = recorderReducer(machine.current, event);
    dispatchState(event);
  }, []);
  const [levels, setLevels] = useState<number[]>([]);
  const [reduced, setReduced] = useState(false);
  /** The microphone went away mid-recording: what it recorded waits for ✓ or Esc. */
  const [interrupted, setInterrupted] = useState(false);
  const session = useRef<MicrophoneSession | null>(null);
  /** A dictation whose transcription failed for a reason that may pass: Try again sends this recording again. */
  const kept = useRef<Recording | null>(null);
  const attempt = useRef(0);
  const mode = useRef<RecordMode>("note");
  const root = useRef<HTMLSpanElement>(null);
  const overlay = useRef<HTMLDivElement>(null);
  const primary = useRef<HTMLButtonElement>(null);
  const latest = useRef({ bridge, voice });
  const dictateKeys = useShortcutKeys("composer.dictate");
  const { shortcuts, platform } = useShortcuts();
  const busy = recorderBusy(state);
  const open = busy || state.phase === "error";

  useEffect(() => {
    latest.current = { bridge, voice };
  });

  const release = useCallback(() => {
    attempt.current += 1;
    session.current?.cancel();
    session.current = null;
    holdListening("recording", false);
  }, []);

  const start = useCallback(async (wanted: RecordMode) => {
    const { bridge: current } = latest.current;
    let features = latest.current.voice;
    if (!current) return;
    if (current.disabledReason) {
      notify.info("Nothing can be sent here", current.disabledReason);
      return;
    }
    if (recorderBusy(machine.current)) return;
    if (wanted === "note" && features.checking && canRecord()) {
      // Pressed before the page knew whether uploads work: ask, then go on.
      const config = await ensureUploadsConfig(queryClient).catch(() => null);
      if (config?.enabled) features = { ...features, notes: true, notesReason: null, maxUploadBytes: config.max_bytes };
      if (recorderBusy(machine.current)) return;
    }
    if (wanted === "note" && !features.notes) {
      if (features.dictate) notify.info("Voice messages are off here", `${features.uploadsReason ?? features.notesReason ?? ""} Dictation still works${dictateKeys.text ? ` (${dictateKeys.text})` : ""}.`.trim());
      else notify.error({ status: 0, code: "voice_off", message: features.notesReason ?? features.reason, hint: "" }, "The microphone is off here");
      return;
    }
    if (wanted === "dictate" && !features.dictate) {
      notify.error({ status: 0, code: "dictation_off", message: features.dictateReason ?? features.reason, hint: "" }, "Dictation is off here");
      return;
    }
    const mine = ++attempt.current;
    kept.current = null;
    mode.current = wanted;
    setLevels([]);
    setInterrupted(false);
    setReduced(window.matchMedia("(prefers-reduced-motion: reduce)").matches);
    dispatch({ type: "start", mode: wanted });
    holdListening("recording", true);
    // Whatever else plays would be recorded too: it pauses (Listen mode's own message resumes after the hold).
    const playing = playerState();
    if ((playing.status === "playing" || playing.status === "loading") && playing.origin !== "queue") pausePlayback();
    try {
      const opened = await openMicrophone();
      if (attempt.current !== mine) {
        opened.cancel();
        return;
      }
      session.current = opened;
      opened.onInterrupted(() => {
        if (attempt.current === mine && session.current === opened) setInterrupted(true);
      });
      dispatch({ type: "granted" });
    } catch (error) {
      if (attempt.current !== mine) return;
      holdListening("recording", false);
      dispatch({ type: "denied", problem: error instanceof MicrophoneError ? error.problem : microphoneProblem("recording_failed", error instanceof Error ? error.message : undefined) });
    }
  }, [dictateKeys.text, dispatch, queryClient]);

  const cancel = useCallback(() => {
    release();
    kept.current = null;
    setInterrupted(false);
    dispatch({ type: "cancel" });
    reportActivity(null);
    latest.current.bridge?.focusField();
  }, [release, dispatch]);

  /** Dictation's second half: the recording's words, typed at the caret. */
  const transcribeDictation = useCallback(async (recording: Recording, mine: number) => {
    const current = latest.current.bridge;
    reportActivity("typing");
    try {
      if (!recording.blob.size) throw new ApiError({ status: 0, code: "nothing_recorded", message: "Nothing was recorded: the microphone gave no sound.", hint: "Check the microphone (and that the right one is chosen in the system's sound settings), then try again." });
      const result = await transcribe(recording.blob, { contentType: baseType(recording.type) });
      if (attempt.current !== mine) return;
      const words = result.transcript.trim();
      if (!words) {
        dispatch({ type: "failed", problem: { code: "nothing_heard", message: "No words were heard in the recording.", hint: "Speak a little closer to the microphone, then try again." } });
        return;
      }
      current?.insertText(words);
      dispatch({ type: "transcribed" });
      current?.focusField();
    } catch (error) {
      if (attempt.current !== mine) return;
      const failure = ApiError.from(error);
      // What was said is not lost to a failure that may pass: Try again sends the same recording.
      if (passing(failure)) kept.current = recording;
      dispatch({ type: "failed", problem: { code: failure.code, message: failure.message, hint: failure.hint } });
    } finally {
      reportActivity(null);
    }
  }, [dispatch]);

  /** ✓ / Enter: stop, then send the voice message or insert the dictated words. */
  const finish = useCallback(async () => {
    const opened = session.current;
    if (!opened) return;
    const { bridge: current, voice: features } = latest.current;
    const mine = attempt.current;
    session.current = null;
    setInterrupted(false);
    dispatch({ type: "stop" });
    let recording: Recording;
    try {
      recording = await opened.stop();
    } catch (error) {
      holdListening("recording", false);
      dispatch({ type: "failed", problem: error instanceof MicrophoneError ? error.problem : microphoneProblem("recording_failed") });
      return;
    }
    if (attempt.current !== mine) return;
    dispatch({ type: "stopped" });
    if (mode.current === "note") {
      holdListening("recording", false);
      current?.focusField();
      if (current) void handOver(current, recording, features);
      return;
    }
    holdListening("recording", false);
    await transcribeDictation(recording, mine);
  }, [dispatch, transcribeDictation]);

  /** Try again on a problem: a dictation whose transcription failed goes again as it was; anything else records anew. */
  const retry = useCallback(() => {
    const recording = kept.current;
    if (recording && machine.current.phase === "error" && machine.current.mode === "dictate") {
      kept.current = null;
      dispatch({ type: "retranscribe" });
      void transcribeDictation(recording, attempt.current);
      return;
    }
    kept.current = null;
    if (machine.current.phase === "error") void start(machine.current.mode);
  }, [dispatch, start, transcribeDictation]);

  const dismiss = useCallback(() => {
    kept.current = null;
    dispatch({ type: "dismiss" });
    latest.current.bridge?.focusField();
  }, [dispatch]);

  // The composer's /voice and /dictate start the same flows as ⌘⇧M and ⌘⇧D.
  useEffect(() => {
    if (!bridge) return;
    bridge.registerControls({ record: () => void start("note"), dictate: () => void start("dictate") });
    return () => bridge.registerControls(null);
  }, [bridge, start]);

  useShortcut("composer.record", () => {
    void start("note");
  }, !!bridge && voice.record && !busy);
  useShortcut("composer.dictate", () => {
    void start("dictate");
  }, !!bridge && voice.dictate && !busy);

  // While recording: the time, the meter, presence, and the limits (ten minutes, the upload cap).
  const phase = state.phase;
  useEffect(() => {
    if (phase !== "recording") return;
    const timer = setInterval(() => {
      const opened = session.current;
      if (!opened || opened.interruptedAt() !== null) return;
      const elapsedMs = performance.now() - opened.startedAt;
      const level = opened.level();
      dispatch({ type: "tick", elapsedMs, level });
      setLevels(previous => [...previous.slice(1 - METER_BARS), level]);
      reportActivity(mode.current === "note" ? "recording_voice" : "typing");
      const cap = latest.current.voice.maxUploadBytes;
      if (elapsedMs >= MAX_RECORDING_MS || (mode.current === "note" && cap && opened.bytes() >= cap * 0.97)) void finish();
    }, TICK_MS);
    return () => clearInterval(timer);
  }, [phase, finish, dispatch]);

  // Keys while the bar is open: Esc cancels (or closes a problem); Enter, or the shortcut that started it, stops and
  // sends; Tab stays among the bar's buttons. A dialog or menu opened meanwhile keeps its own keys.
  useEffect(() => {
    if (!open) return;
    const sameShortcut = (event: KeyboardEvent) => {
      const chord = chordFromEvent(event, platform);
      const id = mode.current === "note" ? "composer.record" : "composer.dictate";
      return !!chord && !!shortcuts.find(entry => entry.id === id)?.bindings.includes(chord);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      const target = event.target instanceof Element ? event.target : null;
      const bar = overlay.current;
      if (target?.closest(LAYER) && !bar?.contains(target)) return;
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        if (phase === "error") dismiss();
        else cancel();
      } else if (phase === "recording" && ((event.key === "Enter" && !(target instanceof HTMLButtonElement)) || sameShortcut(event))) {
        event.preventDefault();
        event.stopPropagation();
        void finish();
      } else if (event.key === "Tab" && bar && (!target || bar.contains(target) || target === document.body)) {
        const buttons = [...bar.querySelectorAll<HTMLButtonElement>("button:not([disabled])")];
        if (!buttons.length) return;
        event.preventDefault();
        const at = target ? buttons.indexOf(target as HTMLButtonElement) : -1;
        const next = at < 0 ? (event.shiftKey ? buttons.length - 1 : 0) : (at + (event.shiftKey ? -1 : 1) + buttons.length) % buttons.length;
        buttons[next]!.focus();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [open, phase, cancel, dismiss, finish, platform, shortcuts]);

  // The keyboard goes to the bar's main button (✓ while recording, Try again on a problem).
  useEffect(() => {
    if (phase === "recording" || phase === "error" || phase === "asking") primary.current?.focus({ preventScroll: true });
  }, [phase]);

  // The bar sits exactly over the composer's row, its bottom on the row's bottom so a taller bar (a problem with its
  // hint) grows upward, never off screen. Followed every frame: the keyboard on a phone moves the row.
  useLayoutEffect(() => {
    if (!open) return;
    const host = root.current;
    const node = overlay.current;
    if (!host || !node) return;
    const row = composerRow(host) ?? host;
    let frame = 0;
    const place = () => {
      const rect = row.getBoundingClientRect();
      setStyle(node, "width", `${Math.round(rect.width)}px`);
      setStyle(node, "minHeight", `${Math.max(44, Math.round(rect.height))}px`);
      setStyle(node, "transform", `translate(${Math.round(rect.left)}px, ${Math.round(rect.bottom - node.offsetHeight)}px)`);
      frame = requestAnimationFrame(place);
    };
    place();
    return () => cancelAnimationFrame(frame);
  }, [open]);

  // Leaving the conversation mid-recording throws the recording away and frees the microphone.
  useEffect(() => release, [release]);

  const dictating = state.phase !== "idle" && state.mode === "dictate";
  const what = dictating ? "Dictating" : "Recording a voice message";
  const elapsed = state.phase === "recording" || state.phase === "stopping" || state.phase === "transcribing" ? state.elapsedMs : 0;
  const left = state.phase === "recording" && !interrupted ? timeLeftWarning(elapsed) : null;
  const sendWords = dictating ? "inserts what it heard" : "sends what it recorded";

  let button;
  if (!bridge) {
    button = <IconAction label="Record a voice message" icon={<Mic strokeWidth={1.75} aria-hidden="true" />} unavailable="the microphone is not connected to this message field" tooltipSide="top" />;
  } else if (voice.notes || (voice.checking && voice.microphone)) {
    // While the uploads answer is on its way the microphone records (start() waits for it before deciding).
    button = <IconAction label="Record a voice message" shortcut="composer.record" icon={<Mic strokeWidth={1.75} aria-hidden="true" />} tooltipSide="top" onClick={() => void start("note")} />;
  } else if (voice.dictate) {
    // The reason in full is in the toast ⌘⇧M shows; the name stays short.
    button = <IconAction label="Dictate (voice messages need uploads, which are off here)" shortcut="composer.dictate" icon={<Mic strokeWidth={1.75} aria-hidden="true" />} tooltipSide="top" onClick={() => void start("dictate")} />;
  } else {
    button = <IconAction label="Record a voice message" icon={<Mic strokeWidth={1.75} aria-hidden="true" />} unavailable={voice.notesReason ?? voice.reason} tooltipSide="top" />;
  }

  const bar = open ? (
    <div ref={overlay} className={styles.recorder} data-sq="surface" data-voice-ui="" data-phase={state.phase} data-interrupted={interrupted ? "" : undefined} role="group" aria-label={state.phase === "error" ? "The microphone" : what}>
      {state.phase === "error" ? (
        <>
          <div className={styles.problem} role="alert">
            <span className={styles.problemTitle}>{state.problem.message}</span>
            {state.problem.hint ? <span className={styles.problemHint}>{state.problem.hint}</span> : null}
          </div>
          <div className={styles.problemActions}>
            <button type="button" ref={primary} className={styles.textButton} data-sq="surface" onClick={retry}>Try again</button>
            <button type="button" className={styles.recorderButton} data-sq="surface" aria-label="Close" onClick={dismiss}>
              <X size={16} strokeWidth={1.9} aria-hidden="true" />
            </button>
          </div>
        </>
      ) : (
        <>
          <button type="button" ref={state.phase === "asking" || state.phase === "transcribing" ? primary : undefined} className={styles.recorderButton} data-sq="surface" aria-label={dictating ? "Cancel dictation" : "Cancel the voice message"} aria-keyshortcuts="Escape" onClick={cancel}>
            <X size={17} strokeWidth={1.9} aria-hidden="true" />
          </button>
          <div className={styles.recorderBody}>
            {state.phase === "asking" ? (
              <span className={styles.recorderNote} role="status">Waiting for the microphone… If the browser asks, allow it.</span>
            ) : state.phase === "transcribing" ? (
              <span className={styles.recorderNote} role="status"><TextShimmer>Transcribing…</TextShimmer></span>
            ) : interrupted ? (
              <>
                <span className={styles.recTime}>{clockDuration(elapsed)}</span>
                <span className={styles.recorderNote} role="status">{`The microphone stopped. ✓ ${sendWords}; ✕ throws it away.`}</span>
              </>
            ) : (
              <>
                <svg className={styles.recDot} viewBox="0 0 8 8" aria-hidden="true"><circle cx="4" cy="4" r="4" fill="currentColor" /></svg>
                <span className={styles.recLabel}>{dictating ? "Dictating" : "Recording"}</span>
                <span className={styles.recTime} role="timer" aria-label={`${what}, ${clockDuration(elapsed)}`}>{clockDuration(elapsed)}</span>
                {reduced ? (
                  <span className={styles.level} aria-hidden="true"><span style={{ width: `${Math.round((state.phase === "recording" ? state.level : 0) * 100)}%` }} /></span>
                ) : (
                  <span className={styles.meter} aria-hidden="true">
                    {levels.map((level, index) => <span key={index} style={{ height: `${3 + Math.round(level * 21)}px` }} />)}
                  </span>
                )}
                {left !== null ? <span className={styles.warning} aria-hidden="true">{`${Math.ceil(left / 1000)} s left`}</span> : null}
                {/* Said once when the warning starts (the countdown itself is for the eyes). */}
                <span className="sr-only" role="status">{left !== null ? `${Math.round(WARN_BEFORE_MS / 1000)} seconds left: at ${clockDuration(MAX_RECORDING_MS)} the recording stops and ${dictating ? "its words are inserted" : "it is sent"}.` : ""}</span>
              </>
            )}
          </div>
          <button
            type="button"
            ref={state.phase === "recording" || state.phase === "stopping" ? primary : undefined}
            className={`${styles.recorderButton} ${styles.recorderSend}`}
            data-sq="surface"
            aria-label={dictating ? "Stop and insert the words" : "Stop and send the voice message"}
            aria-keyshortcuts="Enter"
            aria-disabled={state.phase === "recording" ? undefined : true}
            title={`${dictating ? "Insert the words" : "Send"} (Enter)`}
            onClick={() => {
              if (state.phase === "recording") void finish();
            }}
          >
            <Check size={17} strokeWidth={2.25} aria-hidden="true" />
          </button>
        </>
      )}
    </div>
  ) : null;

  return (
    <span ref={root} className={styles.mic} data-recording={busy ? "" : undefined}>
      {button}
      {bar && typeof document !== "undefined" ? createPortal(bar, document.body) : null}
    </span>
  );
}
