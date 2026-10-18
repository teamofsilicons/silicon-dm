"use client";

/**
 * The playback engine (spec §6.6): one <audio> element for the whole tab. A new play stops the previous one, and the
 * element a click once unlocked stays unlocked (Safari ties that permission to the element, so Listen mode can speak
 * later without a click). Everything the website plays goes through here: spoken messages, Listen on a Silicon's
 * words, voice notes, voice samples.
 *
 *   play(track)      loads the track (lib/voice/sources.ts: memory → Cache Storage → the service) and plays it
 *   toggle(track)    play or pause (pressing it while it loads cancels)
 *   seek / seekBy    in seconds; before the audio is loaded the position is kept for when it is
 *   setRate          1×, 1.5×, 2× with the pitch kept; the choice is the Carbon's default from then on
 *   useTrack(key)    one track's state for a bubble (a bubble that is not playing never re-renders on progress)
 *
 * Hardware keys and the system's media controls work through the Media Session (title: the sender, artist "Silicon
 * DM"). Progress is read every animation frame while playing, so a waveform fills smoothly.
 */
import { useSyncExternalStore } from "react";
import { chatSettings, updateChatSettings, type PlaybackSpeed } from "@/lib/chat/settings";
import { ApiError } from "@/lib/errors";
import { silentWav } from "./wav";
import { voiceWords } from "./words";

export type PlayerStatus = "idle" | "loading" | "playing" | "paused" | "error";
export type TrackKind = "speech" | "note" | "sample";
/** Who started it: Listen (a message action), the Listen mode queue, a bubble's own button, a voice sample. */
export type TrackOrigin = "listen" | "queue" | "bubble" | "sample";

export interface LoadedAudio {
  /** What the <audio> element plays (an object URL from the cache, or a direct link as a last resort). */
  url: string;
  /** The exact length when known (a WAV's header, decoded audio, the service's x-dm-duration-ms). */
  durationMs?: number | null;
  /** Called when the player lets go of it. */
  release?: () => void;
}

export interface Track {
  /** Stable identity: the same message (or sample) is the same key wherever it is played from. */
  key: string;
  /** What the audio is made of (a spoken message's script and voice, a voice note's link): an edit is other audio. */
  version?: string;
  kind: TrackKind;
  origin: TrackOrigin;
  /** The Media Session title: who is speaking. */
  title: string;
  artwork?: string | null;
  /** A length to show before the audio is loaded. */
  durationMs?: number | null;
  load: (signal: AbortSignal) => Promise<LoadedAudio>;
  /** Seconds to start from (a press on the waveform before anything was loaded). */
  startAt?: number;
}

export interface PlayerState {
  key: string | null;
  kind: TrackKind | null;
  origin: TrackOrigin | null;
  title: string;
  status: PlayerStatus;
  /** Seconds. */
  position: number;
  /** Seconds; 0 while unknown. */
  duration: number;
  rate: PlaybackSpeed;
  error: ApiError | null;
}

export type PlayerEvent =
  | { type: "ended" | "stopped" | "replaced" | "failed"; key: string; origin: TrackOrigin }
  | { type: "next"; key: string | null; origin: TrackOrigin | null };

const RATES: PlaybackSpeed[] = [1, 1.5, 2];
export const SPEEDS = RATES;

const IDLE: PlayerState = { key: null, kind: null, origin: null, title: "", status: "idle", position: 0, duration: 0, rate: 1, error: null };

interface Active {
  track: Track;
  loaded: LoadedAudio | null;
  controller: AbortController;
  generation: number;
}

let state: PlayerState = IDLE;
let audio: HTMLAudioElement | null = null;
let active: Active | null = null;
let generation = 0;
let frame = 0;
let unlocked = false;
let silence: string | null = null;
let lastPositionReport = 0;
const listeners = new Set<() => void>();
const eventListeners = new Set<(event: PlayerEvent) => void>();

function set(patch: Partial<PlayerState>): void {
  state = { ...state, ...patch };
  for (const listener of listeners) listener();
}

function emit(event: PlayerEvent): void {
  for (const listener of eventListeners) {
    try {
      listener(event);
    } catch {
      // A listener's failure never stops playback.
    }
  }
}

const currentRate = (): PlaybackSpeed => {
  const rate = typeof window === "undefined" ? 1 : chatSettings().playbackSpeed;
  return RATES.includes(rate) ? rate : 1;
};

function keepPitch(node: HTMLAudioElement): void {
  const media = node as HTMLAudioElement & { webkitPreservesPitch?: boolean; mozPreservesPitch?: boolean };
  media.preservesPitch = true;
  media.webkitPreservesPitch = true;
  media.mozPreservesPitch = true;
}

/** The tab's one audio element (created on first use, in the browser only). */
function element(): HTMLAudioElement | null {
  if (typeof window === "undefined" || typeof Audio === "undefined") return null;
  if (audio) return audio;
  const node = new Audio();
  node.preload = "auto";
  keepPitch(node);
  // In the document (without controls it draws nothing), so a Silicon driving the browser, and the tests, can see
  // what plays: `audio[data-dm-player]`, its `data-track` and its currentTime.
  node.dataset.dmPlayer = "";
  node.setAttribute("aria-hidden", "true");
  document.body.appendChild(node);
  node.addEventListener("playing", () => {
    if (!isActive(node)) return;
    set({ status: "playing", error: null, position: node.currentTime });
    startFrames(node);
    sessionState("playing");
  });
  node.addEventListener("pause", () => {
    if (!isActive(node) || node.ended || state.status === "loading") return;
    stopFrames();
    set({ status: "paused", position: node.currentTime });
    sessionState("paused");
  });
  node.addEventListener("ended", () => {
    if (!isActive(node) || !active) return;
    const done = active;
    finish();
    emit({ type: "ended", key: done.track.key, origin: done.track.origin });
  });
  node.addEventListener("timeupdate", () => {
    if (!isActive(node) || frame) return;
    set({ position: node.currentTime });
  });
  node.addEventListener("durationchange", () => {
    if (!isActive(node) || !active || active.loaded?.durationMs) return;
    if (Number.isFinite(node.duration) && node.duration > 0) set({ duration: node.duration });
  });
  node.addEventListener("error", () => {
    if (!isActive(node)) return;
    fail(new ApiError({ status: 0, code: "audio_unplayable", message: "This browser could not play the audio.", hint: "Try again. If it keeps failing, try another browser." }));
  });
  audio = node;
  return node;
}

function isActive(node: HTMLAudioElement): boolean {
  return !!active?.loaded && node.src === active.loaded.url;
}

function startFrames(node: HTMLAudioElement): void {
  if (frame || typeof requestAnimationFrame === "undefined") return;
  const step = () => {
    frame = 0;
    if (!isActive(node) || node.paused) return;
    if (Math.abs(node.currentTime - state.position) >= 0.03) set({ position: node.currentTime });
    const now = performance.now();
    if (now - lastPositionReport > 1000) {
      lastPositionReport = now;
      sessionPosition();
    }
    frame = requestAnimationFrame(step);
  };
  frame = requestAnimationFrame(step);
}

function stopFrames(): void {
  if (frame && typeof cancelAnimationFrame !== "undefined") cancelAnimationFrame(frame);
  frame = 0;
}

function release(previous: Active): void {
  previous.loaded?.release?.();
}

/* Media Session ----------------------------------------------------------------------------------------------------- */

let handlersBound = false;

function session(): MediaSession | null {
  return typeof navigator !== "undefined" && "mediaSession" in navigator ? navigator.mediaSession : null;
}

function bindHandlers(media: MediaSession): void {
  if (handlersBound) return;
  handlersBound = true;
  const on = (action: MediaSessionAction, handler: MediaSessionActionHandler | null) => {
    try {
      media.setActionHandler(action, handler);
    } catch {
      // This browser does not know the action.
    }
  };
  on("play", () => resume());
  on("pause", () => pause());
  on("stop", () => stop());
  on("seekbackward", details => seekBy(-(details.seekOffset || 5)));
  on("seekforward", details => seekBy(details.seekOffset || 5));
  on("seekto", details => {
    if (typeof details.seekTime === "number") seek(details.seekTime);
  });
  on("nexttrack", () => emit({ type: "next", key: state.key, origin: state.origin }));
}

function sessionMetadata(track: Track): void {
  const media = session();
  if (!media || typeof MediaMetadata === "undefined") return;
  bindHandlers(media);
  try {
    media.metadata = new MediaMetadata({ title: track.title, artist: "Silicon DM", artwork: track.artwork ? [{ src: track.artwork }] : [] });
  } catch {
    // Artwork the browser cannot read: the title and artist are enough.
  }
}

function sessionState(value: MediaSessionPlaybackState): void {
  const media = session();
  if (!media) return;
  try {
    media.playbackState = value;
    if (value === "none") media.metadata = null;
    else sessionPosition();
  } catch {
    // Older browsers: no state.
  }
}

function sessionPosition(): void {
  const media = session();
  if (!media?.setPositionState || !state.duration || !Number.isFinite(state.duration)) return;
  try {
    media.setPositionState({ duration: state.duration, playbackRate: state.rate, position: Math.min(state.position, state.duration) });
  } catch {
    // An inconsistent moment (position past a duration that was a guess): the next report fixes it.
  }
}

/* Playing ------------------------------------------------------------------------------------------------------------ */

/**
 * Lets this tab play sound later without a click: call it inside a click or key press (turning Listen mode on, the
 * first play). It plays a tenth of a second of silence on the shared element.
 */
export function unlockAudio(): void {
  const node = element();
  if (!node || unlocked || active?.loaded) return;
  silence ??= URL.createObjectURL(new Blob([silentWav()], { type: "audio/wav" }));
  try {
    node.src = silence;
    unlocked = true;
    void node.play().then(
      () => {
        if (node.src === silence) node.pause();
      },
      (error: unknown) => {
        if ((error as { name?: string } | null)?.name === "NotAllowedError") unlocked = false;
      },
    );
  } catch {
    unlocked = false;
  }
}

/** Plays a track (stopping whatever played). `gesture`: called from a click or key press, so it unlocks audio first. */
export function play(track: Track, options: { gesture?: boolean } = {}): void {
  const node = element();
  if (!node) return;
  if (active && active.track.key === track.key && active.track.version === track.version) {
    if (active.loaded && (state.status === "paused" || state.status === "playing")) {
      if (typeof track.startAt === "number") seek(track.startAt);
      if (state.status === "paused") resume();
      return;
    }
    if (state.status === "loading") {
      if (typeof track.startAt === "number") seek(track.startAt);
      return;
    }
  }
  if (options.gesture) unlockAudio();
  replace(node, track);
}

function replace(node: HTMLAudioElement, track: Track): void {
  const previous = active;
  if (previous) {
    previous.controller.abort();
    release(previous);
    emit({ type: "replaced", key: previous.track.key, origin: previous.track.origin });
  }
  stopFrames();
  const current: Active = { track, loaded: null, controller: new AbortController(), generation: ++generation };
  active = current;
  node.dataset.track = track.key;
  if (!node.paused) node.pause();
  const rate = currentRate();
  set({ key: track.key, kind: track.kind, origin: track.origin, title: track.title, status: "loading", position: track.startAt ?? 0, duration: (track.durationMs ?? 0) / 1000, rate, error: null });
  sessionMetadata(track);
  track.load(current.controller.signal).then(
    loaded => {
      if (active !== current) {
        loaded.release?.();
        return;
      }
      current.loaded = loaded;
      if (loaded.durationMs) set({ duration: loaded.durationMs / 1000 });
      node.src = loaded.url;
      node.defaultPlaybackRate = rate;
      node.playbackRate = rate;
      keepPitch(node);
      const startAt = current.track.startAt;
      if (typeof startAt === "number" && startAt > 0) {
        try {
          node.currentTime = startAt;
        } catch {
          // Not seekable yet: it starts from the beginning.
        }
      }
      node.play().catch((error: unknown) => {
        if (active !== current) return;
        const name = (error as { name?: string } | null)?.name;
        if (name === "AbortError") return;
        if (name === "NotAllowedError") {
          set({ status: "paused", error: new ApiError({ status: 0, code: "autoplay_blocked", message: "This browser kept the audio from playing on its own.", hint: "Press play to hear it." }) });
          return;
        }
        fail(ApiError.from(error));
      });
    },
    (error: unknown) => {
      if (active !== current) return;
      if ((error as { name?: string } | null)?.name === "AbortError") return;
      fail(ApiError.from(error));
    },
  );
}

/**
 * The words a failure is shown in, under a bubble or in a toast: a Carbon's (lib/voice/words.ts) where the service's
 * are written for Silicons or developers (a route, a setting, the request that failed), else the service's own.
 */
export function playbackError(error: ApiError, kind: TrackKind = "speech"): ApiError {
  return voiceWords(error, kind === "sample" ? "sample" : "listen");
}

function fail(failure: ApiError): void {
  const failed = active;
  const error = playbackError(failure, failed?.track.kind);
  stopFrames();
  if (failed) {
    failed.controller.abort();
    release(failed);
    failed.loaded = null;
  }
  set({ status: "error", error });
  sessionState("none");
  if (failed) emit({ type: "failed", key: failed.track.key, origin: failed.track.origin });
}

/** Back to nothing playing (after the end, or Stop). */
function finish(): void {
  const node = audio;
  const previous = active;
  active = null;
  stopFrames();
  if (previous) {
    previous.controller.abort();
    release(previous);
  }
  if (node) {
    node.pause();
    node.removeAttribute("src");
    delete node.dataset.track;
    node.load();
  }
  set({ ...IDLE, rate: state.rate });
  sessionState("none");
}

export function pause(): void {
  if (!active) return;
  if (state.status === "loading") {
    stop();
    return;
  }
  audio?.pause();
}

export function resume(): void {
  const node = audio;
  if (!node || !active?.loaded || state.status === "playing") return;
  const current = active;
  node.play().catch((error: unknown) => {
    if (active !== current) return;
    const name = (error as { name?: string } | null)?.name;
    if (name === "AbortError") return;
    if (name === "NotAllowedError") {
      set({ status: "paused", error: new ApiError({ status: 0, code: "autoplay_blocked", message: "This browser kept the audio from playing on its own.", hint: "Press play to hear it." }) });
      return;
    }
    fail(ApiError.from(error));
  });
}

/** Play, or pause what is playing; pressing it while the audio loads cancels the load. */
export function toggle(track: Track, options: { gesture?: boolean } = {}): void {
  if (state.key === track.key) {
    if (state.status === "playing") return pause();
    if (state.status === "loading") return stop();
    // Paused on audio that was since edited: the press plays the new words from the start.
    if (state.status === "paused" && active?.loaded && active.track.version === track.version) {
      if (options.gesture) unlockAudio();
      return resume();
    }
  }
  play(track, options);
}

export function stop(): void {
  const previous = active;
  if (!previous) {
    if (state.key) set({ ...IDLE, rate: state.rate });
    return;
  }
  finish();
  emit({ type: "stopped", key: previous.track.key, origin: previous.track.origin });
}

/** Stops what is playing if it is this track (a bubble leaving the screen, a conversation closing). */
export function stopIf(test: (key: string, origin: TrackOrigin) => boolean): void {
  if (active && test(active.track.key, active.track.origin)) stop();
  else if (!active && state.key && state.status === "error" && test(state.key, state.origin ?? "bubble")) set({ ...IDLE, rate: state.rate });
}

export function seek(seconds: number): void {
  const duration = state.duration || (audio && Number.isFinite(audio.duration) ? audio.duration : 0);
  const target = Math.max(0, duration ? Math.min(seconds, duration) : seconds);
  if (!active) return;
  if (!active.loaded) {
    active.track = { ...active.track, startAt: target };
    set({ position: target });
    return;
  }
  const node = audio;
  if (!node) return;
  try {
    node.currentTime = target;
  } catch {
    return;
  }
  set({ position: target });
  sessionPosition();
}

export function seekBy(delta: number): void {
  seek(state.position + delta);
}

/** The speed for everything played from now on (and what plays now). */
export function setRate(rate: PlaybackSpeed): void {
  const next = RATES.includes(rate) ? rate : 1;
  updateChatSettings({ playbackSpeed: next });
  if (audio) {
    audio.defaultPlaybackRate = next;
    audio.playbackRate = next;
    keepPitch(audio);
  }
  set({ rate: next });
  sessionPosition();
}

/* Reading the state ---------------------------------------------------------------------------------------------------- */

export function playerState(): PlayerState {
  return state;
}

export function subscribePlayer(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Hears what happens to tracks (the Listen mode queue moves on when one ends). */
export function onPlayerEvent(listener: (event: PlayerEvent) => void): () => void {
  eventListeners.add(listener);
  return () => eventListeners.delete(listener);
}

const serverState = () => IDLE;

/** Everything the player is doing (re-renders on every progress step while something plays). */
export function usePlayer(): PlayerState {
  return useSyncExternalStore(subscribePlayer, playerState, serverState);
}

/** One track's state: the player's while it is this track, else idle (and then no re-render on progress). */
export function useTrack(key: string | null): PlayerState {
  return useSyncExternalStore(
    subscribePlayer,
    () => (key && state.key === key ? state : IDLE),
    serverState,
  );
}

/** For tests and the e2e checks: what the shared element is doing. */
export function audioElement(): HTMLAudioElement | null {
  return audio;
}
