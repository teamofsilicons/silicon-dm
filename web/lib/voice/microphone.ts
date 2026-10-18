"use client";

/**
 * The microphone in the browser: getUserMedia + MediaRecorder (Opus in WebM, AAC in MP4 on Safari; lib/voice/recorder.ts
 * picks) and an AnalyserNode for the live level meter. Nothing is recorded before the Carbon asks, and the microphone is
 * released the moment the recording stops or is cancelled. The recording goes to the Carbon's Briefcase (a voice note)
 * or only to transcription (dictation); while it is made, its bytes go to this browser's disk a few seconds at a time
 * (`keep`, lib/voice/takes.ts), so a recording of hours takes no more memory than one of seconds, and a reload or a
 * crash does not lose it. When the tab is hidden or closed, what was gathered is written at once.
 *
 * A microphone can go away mid-recording (unplugged, a headset switched off, another app taking it, the permission
 * withdrawn). The recorder then stops by itself; what it recorded until then is kept, and `onInterrupted` says so, so
 * the Carbon decides whether it goes (✓) or not (Esc) instead of losing it.
 */
import { microphoneProblem, pickRecordingType, type RecorderProblem } from "./recorder";
import type { Take, TakeInfo } from "./takes";

export interface Recording {
  blob: Blob;
  /** The recording's media type, with its codecs (`audio/webm;codecs=opus`). */
  type: string;
  durationMs: number;
  /** Where its bytes are kept (lib/voice/takes.ts), to delete once it is uploaded; null when only in memory. */
  take: Pick<TakeInfo, "id" | "store"> | null;
}

export interface MicrophoneOptions {
  /** Where the recording's bytes go as they are recorded (lib/voice/takes.ts startTake); without it, memory. */
  keep?: (type: string) => Promise<Take>;
}

export interface MicrophoneSession {
  /** When recording started (performance.now()). */
  startedAt: number;
  /** The loudness right now, from 0 to 1. */
  level: () => number;
  /** How many bytes are recorded so far (the upload and transcription caps are checked against it). */
  bytes: () => number;
  /** When the microphone went away (performance.now()), or null while it records. */
  interruptedAt: () => number | null;
  /** Calls `listener` once if the microphone goes away before the recording is stopped or cancelled. */
  onInterrupted: (listener: () => void) => void;
  /** Stops (if it has not stopped by itself) and hands over the recording. */
  stop: () => Promise<Recording>;
  /** Stops and throws the recording away. */
  cancel: () => void;
}

export class MicrophoneError extends Error {
  constructor(readonly problem: RecorderProblem) {
    super(problem.message);
    this.name = "MicrophoneError";
  }
}

/** Whether this browser can record at all (it may still be refused the microphone). */
export function canRecord(): boolean {
  return typeof window !== "undefined" && !!navigator.mediaDevices?.getUserMedia && typeof MediaRecorder !== "undefined" && window.isSecureContext !== false;
}

/** Asks for the microphone and starts recording. Rejects with a MicrophoneError that says what to do. */
export async function openMicrophone(options: MicrophoneOptions = {}): Promise<MicrophoneSession> {
  if (!canRecord()) throw new MicrophoneError(microphoneProblem(window.isSecureContext === false ? "SecurityError" : "unsupported"));
  let stream: MediaStream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true, channelCount: 1 } });
  } catch (error) {
    const name = (error as { name?: string } | null)?.name;
    throw new MicrophoneError(microphoneProblem(name, error instanceof Error ? error.message : undefined));
  }
  const release = () => {
    for (const track of stream.getTracks()) track.stop();
  };
  const type = pickRecordingType(candidate => MediaRecorder.isTypeSupported(candidate));
  let recorder: MediaRecorder;
  try {
    recorder = new MediaRecorder(stream, type ? { mimeType: type, audioBitsPerSecond: 48_000 } : undefined);
  } catch (error) {
    release();
    throw new MicrophoneError(microphoneProblem("recording_failed", error instanceof Error ? error.message : undefined));
  }

  // The meter: the loudness of the last few milliseconds, in decibels mapped to 0..1 (-60 dB is silence).
  const scope = window as unknown as { AudioContext?: typeof AudioContext; webkitAudioContext?: typeof AudioContext };
  const Context = scope.AudioContext ?? scope.webkitAudioContext;
  let context: AudioContext | null = null;
  let analyser: AnalyserNode | null = null;
  let samples: Float32Array<ArrayBuffer> | null = null;
  if (Context) {
    try {
      context = new Context();
      void context.resume().catch(() => undefined);
      analyser = context.createAnalyser();
      analyser.fftSize = 1024;
      context.createMediaStreamSource(stream).connect(analyser);
      samples = new Float32Array(new ArrayBuffer(analyser.fftSize * 4));
    } catch {
      context = null;
      analyser = null;
    }
  }

  // Where the bytes go: this browser's disk (a take), else memory.
  const take = options.keep ? await options.keep(type || "audio/webm").catch(() => null) : null;
  const chunks: Blob[] = [];
  let size = 0;
  let closed = false;
  let settled = false;
  let failure: string | null = null;
  let interruptedAt: number | null = null;
  /** The tab is being hidden or closed: the next chunk is written at once (requestData asks for it). */
  let flushNext = false;
  const interruptions = new Set<() => void>();
  const onHidden = () => {
    if (document.visibilityState !== "hidden" || recorder.state !== "recording" || !take) return;
    flushNext = true;
    try {
      recorder.requestData();
    } catch {
      take.flush();
    }
  };
  const close = () => {
    if (closed) return;
    closed = true;
    document.removeEventListener("visibilitychange", onHidden);
    window.removeEventListener("pagehide", onHidden);
    release();
    void context?.close().catch(() => undefined);
  };
  // The recorder stops when it is told to, or by itself when the microphone goes away; either way its last data comes
  // first, then this.
  let markStopped: () => void = () => undefined;
  const stopped = new Promise<void>(resolve => {
    markStopped = resolve;
  });
  recorder.ondataavailable = event => {
    if (!event.data.size) return;
    size += event.data.size;
    if (take) {
      take.append(event.data, performance.now() - startedAt);
      if (flushNext) {
        flushNext = false;
        take.flush();
      }
    } else chunks.push(event.data);
  };
  recorder.onstop = () => {
    close();
    markStopped();
  };
  recorder.onerror = event => {
    failure = (event as unknown as { error?: Error }).error?.message ?? "the recorder failed";
    close();
    markStopped();
  };
  let startedAt = performance.now();
  document.addEventListener("visibilitychange", onHidden);
  window.addEventListener("pagehide", onHidden);
  for (const track of stream.getAudioTracks()) {
    track.addEventListener("ended", () => {
      if (settled || interruptedAt !== null) return;
      interruptedAt = performance.now();
      for (const listener of interruptions) listener();
      interruptions.clear();
    });
  }
  try {
    startedAt = performance.now();
    recorder.start(1000);
  } catch (error) {
    close();
    void take?.discard();
    throw new MicrophoneError(microphoneProblem("recording_failed", error instanceof Error ? error.message : undefined));
  }

  return {
    startedAt,
    level: () => {
      if (!analyser || !samples || interruptedAt !== null) return 0;
      analyser.getFloatTimeDomainData(samples);
      let energy = 0;
      for (const value of samples) energy += value * value;
      const rms = Math.sqrt(energy / samples.length);
      const decibels = rms > 0 ? 20 * Math.log10(rms) : -100;
      return Math.max(0, Math.min(1, (decibels + 60) / 60));
    },
    bytes: () => size,
    interruptedAt: () => interruptedAt,
    onInterrupted: listener => {
      if (interruptedAt !== null) listener();
      else if (!settled) interruptions.add(listener);
    },
    stop: async () => {
      settled = true;
      interruptions.clear();
      const endedAt = interruptedAt ?? performance.now();
      if (recorder.state !== "inactive") {
        try {
          recorder.stop();
        } catch {
          // It stopped by itself in the meantime: `stopped` settles all the same.
        }
      }
      // A recorder that never says it stopped (it should within a moment) does not hold the recording hostage.
      await Promise.race([stopped, new Promise<void>(resolve => setTimeout(resolve, 4000))]);
      close();
      if (failure && !size) {
        void take?.discard();
        throw new MicrophoneError(microphoneProblem("recording_failed", failure));
      }
      const recorded = recorder.mimeType || type || "audio/webm";
      const durationMs = Math.round(endedAt - startedAt);
      const blob = take ? await take.finish(durationMs, recorded) : new Blob(chunks, { type: recorded });
      return { blob, type: recorded, durationMs, take: take && take.store !== "memory" ? { id: take.id, store: take.store } : null };
    },
    cancel: () => {
      settled = true;
      interruptions.clear();
      recorder.ondataavailable = null;
      try {
        if (recorder.state !== "inactive") recorder.stop();
      } catch {
        // Already stopped.
      }
      chunks.length = 0;
      void take?.discard();
      close();
    },
  };
}
