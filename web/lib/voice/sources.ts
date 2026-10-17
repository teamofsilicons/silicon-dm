"use client";

/**
 * Where the audio comes from, and the caches that make a replay instant (spec §6.6).
 *
 * - Spoken messages and Listen: GET …/messages/{code}/speech through the website's /api proxy (24 kHz WAV). The first
 *   listen to a text message can take a few seconds (the service renders it); after that the service and this tab
 *   both have it.
 * - Voice notes: the file itself (Briefcase bytes through displayUrl), decoded once in the browser for its waveform
 *   and played as a WAV made from the decoded samples, so its length and seeking are exact even for a MediaRecorder
 *   recording, which carries no duration.
 * - Voice samples: POST /api/v1/speech with the sample line, in the voice being tried.
 *
 * Caches: an in-memory LRU of object URLs (64 MB; what is playing is pinned), and Cache Storage behind it so a reload
 * (or a dropped connection) still has what was heard. The persisted copy belongs to the signed-in account (one cache
 * per account; another account's is deleted when this one starts), lives 7 days at most and 48 MB in all. A spoken
 * message's audio never changes (its voice is pinned), so its copy is used as it is; Listen on a text message speaks
 * in the sender's current voice, so a persisted copy is checked with its ETag first (304: it is still right).
 */
import { DM_CONTRACT, messageSpeechUrl, renderSpeech } from "@/lib/chat/api";
import type { Message, Speech, VoiceOption } from "@/lib/chat/types";
import { ensureFreshSession, markSignedOut } from "@/lib/client/session";
import { ApiError, errorFromResponse, networkError } from "@/lib/errors";
import { displayUrl, isBriefcaseFile } from "@/lib/upload/display";
import { readJson, writeJson } from "@/lib/chat/storage";
import { Lru } from "./lru";
import { computePeaks } from "./peaks";
import type { LoadedAudio, Track, TrackOrigin } from "./player";
import { sampleScript } from "./voices";
import { encodeWav, readWavInfo } from "./wav";

interface CachedAudio {
  url: string;
  blob: Blob;
  durationMs: number | null;
  etag: string | null;
  /** Whose current voice it is in (Listen on a text message), so a voice change can drop it. */
  speaker: string | null;
}

const MEMORY_BYTES = 64 * 1024 * 1024;
const MEMORY_ENTRIES = 80;

const memory = new Lru<CachedAudio>({
  maxBytes: MEMORY_BYTES,
  maxEntries: MEMORY_ENTRIES,
  onEvict: (_key, value) => {
    try {
      URL.revokeObjectURL(value.url);
    } catch {
      // Already gone.
    }
  },
});

function remember(key: string, blob: Blob, meta: { durationMs: number | null; etag: string | null; speaker: string | null }): CachedAudio {
  const entry: CachedAudio = { url: URL.createObjectURL(blob), blob, ...meta };
  memory.set(key, entry, blob.size);
  return entry;
}

/** Pins a cached entry while the player uses it; the returned function lets it go. */
function loaded(key: string, entry: CachedAudio): LoadedAudio {
  memory.pin(key);
  let released = false;
  return {
    url: entry.url,
    durationMs: entry.durationMs,
    release: () => {
      if (released) return;
      released = true;
      memory.unpin(key);
    },
  };
}

/** A short, stable fingerprint of text (FNV-1a), for cache keys. */
export function fingerprint(text: string): string {
  let hash = 0x811c9dc5;
  for (let index = 0; index < text.length; index++) {
    hash ^= text.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(36);
}

/** A message's audio, wherever it is played from (the bubble, Listen, Listen mode): one key. */
export const messageKey = (address: string, code: string) => `msg|${address}|${code}`;

/* Cache Storage ----------------------------------------------------------------------------------------------------- */

const CACHE_PREFIX = "dm-voice-v1";
const INDEX_KEY = "dm:voice-cache:v1";
const PERSIST_MAX_BYTES = 48 * 1024 * 1024;
const PERSIST_TTL_MS = 7 * 24 * 3600 * 1000;

interface PersistIndex {
  name: string | null;
  entries: Record<string, { bytes: number; at: number }>;
}

let persistName: string | null = null;

const cachesAvailable = () => typeof window !== "undefined" && "caches" in window && window.isSecureContext !== false;
const requestFor = (key: string) => new Request(`${window.location.origin}/__dm-voice-cache/${encodeURIComponent(key)}`);

/**
 * Says whose audio this tab keeps. Another account's persisted audio in this browser is deleted, so one Carbon's
 * messages never stay readable to the next one who signs in here.
 */
export function setVoiceCacheOwner(account: string | null): void {
  const name = account ? `${CACHE_PREFIX}:${fingerprint(account)}` : null;
  if (name === persistName) return;
  persistName = name;
  if (!cachesAvailable()) return;
  void caches
    .keys()
    .then(names => Promise.all(names.filter(each => each.startsWith(CACHE_PREFIX) && each !== name).map(each => caches.delete(each))))
    .catch(() => undefined);
  const index = readJson<PersistIndex>(INDEX_KEY, { name: null, entries: {} });
  if (index.name !== name) writeJson(INDEX_KEY, { name, entries: {} } satisfies PersistIndex);
}

/**
 * Forgets every persisted voice audio in this browser (signing out). The signed-in account's cache is deleted at
 * once (the page is about to leave), then any other left behind.
 */
export function clearPersistedVoice(): void {
  writeJson(INDEX_KEY, null);
  memory.clear();
  if (!cachesAvailable()) return;
  if (persistName) void caches.delete(persistName).catch(() => undefined);
  persistName = null;
  void caches.keys().then(names => Promise.all(names.filter(each => each.startsWith(CACHE_PREFIX)).map(each => caches.delete(each)))).catch(() => undefined);
}

function readIndex(): PersistIndex {
  const index = readJson<PersistIndex>(INDEX_KEY, { name: persistName, entries: {} });
  return index && index.name === persistName && index.entries && typeof index.entries === "object" ? index : { name: persistName, entries: {} };
}

async function persistedGet(key: string): Promise<{ blob: Blob; etag: string | null; durationMs: number | null } | null> {
  if (!persistName || !cachesAvailable()) return null;
  try {
    const cache = await caches.open(persistName);
    const response = await cache.match(requestFor(key));
    const index = readIndex();
    if (!response) {
      if (index.entries[key]) {
        delete index.entries[key];
        writeJson(INDEX_KEY, index);
      }
      return null;
    }
    const at = Number(response.headers.get("x-dm-stored-at")) || 0;
    if (Date.now() - at > PERSIST_TTL_MS) {
      await cache.delete(requestFor(key));
      delete index.entries[key];
      writeJson(INDEX_KEY, index);
      return null;
    }
    if (index.entries[key]) {
      index.entries[key] = { ...index.entries[key]!, at: Date.now() };
      writeJson(INDEX_KEY, index);
    }
    const duration = Number(response.headers.get("x-dm-duration-ms"));
    return { blob: await response.blob(), etag: response.headers.get("etag"), durationMs: Number.isFinite(duration) && duration > 0 ? duration : null };
  } catch {
    return null;
  }
}

async function persistedPut(key: string, blob: Blob, meta: { etag: string | null; durationMs: number | null }): Promise<void> {
  const name = persistName;
  if (!name || !cachesAvailable() || blob.size > PERSIST_MAX_BYTES / 4) return;
  try {
    const cache = await caches.open(name);
    const headers = new Headers({ "content-type": blob.type || "audio/wav", "x-dm-stored-at": String(Date.now()) });
    if (meta.etag) headers.set("etag", meta.etag);
    if (meta.durationMs) headers.set("x-dm-duration-ms", String(meta.durationMs));
    await cache.put(requestFor(key), new Response(blob, { headers }));
    const index = readIndex();
    index.entries[key] = { bytes: blob.size, at: Date.now() };
    // Over the cap: the least recently used go.
    let total = Object.values(index.entries).reduce((sum, entry) => sum + entry.bytes, 0);
    for (const [old] of Object.entries(index.entries).sort((a, b) => a[1].at - b[1].at)) {
      if (total <= PERSIST_MAX_BYTES) break;
      if (old === key) continue;
      total -= index.entries[old]!.bytes;
      delete index.entries[old];
      await cache.delete(requestFor(old));
    }
    writeJson(INDEX_KEY, index);
  } catch {
    // Quota or a private window: the in-memory copy is enough for this tab.
  }
}

/* Fetching -------------------------------------------------------------------------------------------------------- */

interface Fetched {
  blob: Blob | null;
  etag: string | null;
  durationMs: number | null;
}

async function durationOf(blob: Blob, header: string | null): Promise<number | null> {
  const fromHeader = Number(header);
  if (header && Number.isFinite(fromHeader) && fromHeader > 0) return fromHeader;
  try {
    return readWavInfo(await blob.slice(0, 512).arrayBuffer())?.durationMs ?? null;
  } catch {
    return null;
  }
}

/** GET a message's speech, with If-None-Match when a copy is at hand (blob null: 304, the copy is right). */
async function fetchSpeech(url: string, etag: string | null): Promise<Fetched> {
  await ensureFreshSession();
  const path = url.replace(/^\/api/, "");
  let response: Response;
  try {
    response = await fetch(url, {
      credentials: "same-origin",
      cache: "no-store",
      headers: { "x-dm-contract-version": DM_CONTRACT, Accept: "audio/wav", ...(etag ? { "If-None-Match": etag } : {}) },
    });
  } catch (error) {
    throw networkError(error, "GET", path, "this site");
  }
  if (response.status === 304) return { blob: null, etag, durationMs: null };
  if (!response.ok) {
    const text = await response.text().catch(() => "");
    let body: unknown = null;
    try {
      body = text ? JSON.parse(text) : null;
    } catch {
      body = null;
    }
    const error = errorFromResponse(response, body, "GET", path);
    if (error.status === 401) markSignedOut();
    throw error;
  }
  const blob = await response.blob();
  return { blob, etag: response.headers.get("etag"), durationMs: await durationOf(blob, response.headers.get("x-dm-duration-ms")) };
}

/* Spoken messages and Listen ---------------------------------------------------------------------------------------- */

/** What a message's speech is: where to fetch it, and what makes one rendering differ from another. */
export interface SpeechSource {
  address: string;
  code: string;
  /** A custodian's view (`?for=si:x`), or null. */
  view: string | null;
  /** The words and voice it is made of (an edit, or another voice, is other audio). */
  content: string;
  /** Whose current voice it is in (Listen on words), or null for a spoken message (its voice is pinned). */
  speaker: string | null;
}

/** The content of a spoken message's audio: its pinned voice, model and script. */
export const speechContent = (speech: Pick<Speech, "voice" | "model" | "script">) => `speech:${speech.voice}:${speech.model ?? ""}:${speech.script}`;

/** Speech being fetched, so a warm-up on hover and the press that follows share one request. */
const speechLoads = new Map<string, Promise<CachedAudio>>();

async function fetchSpeechAudio(source: SpeechSource, key: string): Promise<CachedAudio> {
  // A spoken message plays its pinned voice for ever; Listen on words uses the sender's voice of the day.
  const pinned = source.speaker === null;
  const speaker = source.speaker;
  const stored = await persistedGet(key);
  if (stored && pinned) return remember(key, stored.blob, { durationMs: stored.durationMs, etag: stored.etag, speaker });
  let fetched: Fetched;
  try {
    fetched = await fetchSpeech(messageSpeechUrl(source.address, source.code, source.view), stored?.etag ?? null);
  } catch (error) {
    // Offline with a copy at hand: the copy, even if a newer voice exists.
    if (stored && ApiError.from(error).isNetwork) return remember(key, stored.blob, { durationMs: stored.durationMs, etag: stored.etag, speaker });
    throw error;
  }
  if (!fetched.blob && stored) return remember(key, stored.blob, { durationMs: stored.durationMs, etag: stored.etag, speaker });
  if (!fetched.blob) throw new ApiError({ status: 304, code: "audio_missing", message: "The service said the audio had not changed, but this tab has no copy of it.", hint: "Try again." });
  void persistedPut(key, fetched.blob, { etag: fetched.etag, durationMs: fetched.durationMs });
  return remember(key, fetched.blob, { durationMs: fetched.durationMs, etag: fetched.etag, speaker });
}

/**
 * Loads a message's speech: memory, then the persisted copy, then the service. The request is shared and finishes
 * even when the player lets go of it (the service renders it once either way, and the next press is instant).
 */
async function loadSpeech(source: SpeechSource): Promise<LoadedAudio> {
  const key = `${messageKey(source.address, source.code)}|${fingerprint(source.content)}`;
  const hit = memory.get(key);
  if (hit) return loaded(key, hit);
  let pending = speechLoads.get(key);
  if (!pending) {
    pending = fetchSpeechAudio(source, key).finally(() => speechLoads.delete(key));
    speechLoads.set(key, pending);
  }
  return loaded(key, await pending);
}

/** Starts loading a track so the press that likely follows (a pointer over ▶, the keyboard on it) plays at once. */
export function warmTrack(track: Track): void {
  void track.load(new AbortController().signal).then(
    audio => audio.release?.(),
    () => undefined,
  );
}

/** A voice change makes Listen audio in that account's old voice stale (spoken messages keep their pinned voice). */
export function forgetSpeaker(account: { id: string; uuid?: string }): void {
  memory.deleteWhere((_key, value) => value.speaker !== null && (value.speaker === account.uuid || value.speaker === account.id));
}

/* Voice notes ------------------------------------------------------------------------------------------------------- */

/** What decoding a voice note gives: its waveform and exact length (null when the browser could not decode it). */
export interface NoteInfo {
  peaks: number[] | null;
  durationMs: number | null;
}

const NOTE_RATE = 24_000;
/** Larger files are played as they are (decoding ten minutes of audio at once costs too much memory). */
const DECODE_LIMIT_BYTES = 12 * 1024 * 1024;
const noteInfo = new Map<string, NoteInfo>();
const preparing = new Map<string, Promise<CachedAudio>>();
const noteListeners = new Set<() => void>();
let running = 0;
const waitingTurns: Array<() => void> = [];

/** At most two voice notes are fetched and decoded at a time (a long thread of them scrolls into view at once). */
async function turn<T>(work: () => Promise<T>): Promise<T> {
  if (running >= 2) await new Promise<void>(resolve => waitingTurns.push(resolve));
  running += 1;
  try {
    return await work();
  } finally {
    running -= 1;
    waitingTurns.shift()?.();
  }
}

/**
 * Where a voice note is read from: Briefcase files through this website (or, for a file this tab just uploaded, the
 * bytes it already has: a blob: URL), other links only over https. `fetchable` says whether this page may read the
 * bytes itself (for the waveform and the exact length): only from this site, because its Content-Security-Policy lets
 * the page fetch from its own origin only (connect-src 'self'). Anything else, a blob: URL or a link elsewhere, is
 * played by the <audio> element as it is (media-src allows blob: and https:), without a waveform.
 */
export function noteSource(url: string): { url: string; fetchable: boolean } | null {
  if (isBriefcaseFile(url)) {
    const shown = displayUrl(url);
    return { url: shown, fetchable: shown.startsWith("/") };
  }
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "https:" || parsed.username || parsed.password) return null;
    const here = typeof window !== "undefined" ? window.location.origin : null;
    return { url: parsed.href, fetchable: parsed.origin === here };
  } catch {
    return null;
  }
}

async function decode(bytes: ArrayBuffer): Promise<{ samples: Float32Array; durationMs: number } | null> {
  const scope = window as unknown as { OfflineAudioContext?: typeof OfflineAudioContext; webkitOfflineAudioContext?: typeof OfflineAudioContext };
  const Context = scope.OfflineAudioContext ?? scope.webkitOfflineAudioContext;
  if (!Context) return null;
  const context = new Context(1, 1, NOTE_RATE);
  const buffer = await context.decodeAudioData(bytes.slice(0));
  const samples = new Float32Array(buffer.length);
  for (let channel = 0; channel < buffer.numberOfChannels; channel++) {
    const data = buffer.getChannelData(channel);
    for (let index = 0; index < data.length; index++) samples[index] += data[index]! / buffer.numberOfChannels;
  }
  return { samples, durationMs: Math.round(buffer.duration * 1000) };
}

function noteKey(url: string): string {
  return `note|${url}`;
}

/** Decodes a voice note's bytes: its waveform, its length, and a WAV to play (or the bytes as they are). */
async function prepareFromBytes(url: string, bytes: ArrayBuffer, type: string): Promise<CachedAudio> {
  const key = noteKey(url);
  let decoded: Awaited<ReturnType<typeof decode>> = null;
  if (bytes.byteLength <= DECODE_LIMIT_BYTES) decoded = await decode(bytes).catch(() => null);
  if (decoded && decoded.samples.length) {
    noteInfo.set(url, { peaks: computePeaks(decoded.samples), durationMs: decoded.durationMs });
    for (const listener of noteListeners) listener();
    return remember(key, new Blob([encodeWav(decoded.samples, NOTE_RATE)], { type: "audio/wav" }), { durationMs: decoded.durationMs, etag: null, speaker: null });
  }
  noteInfo.set(url, { peaks: null, durationMs: null });
  for (const listener of noteListeners) listener();
  return remember(key, new Blob([bytes], { type }), { durationMs: null, etag: null, speaker: null });
}

/**
 * The bytes of a voice note this tab just recorded and uploaded: decoded now, so its own bubble draws its waveform
 * and plays at once, with nothing downloaded again.
 */
export function rememberNote(url: string, recording: Blob): void {
  const key = noteKey(url);
  if (memory.has(key) || preparing.has(key)) return;
  const work = turn(async () => prepareFromBytes(url, await recording.arrayBuffer(), recording.type.split(";")[0] || "audio/webm")).finally(() => preparing.delete(key));
  preparing.set(key, work);
  void work.catch(() => undefined);
}

/**
 * Fetches and decodes a voice note once (later callers share the work, which a paused player does not cancel),
 * keeping its audio and waveform.
 */
export function prepareNote(url: string): Promise<CachedAudio> {
  const key = noteKey(url);
  const hit = memory.get(key);
  if (hit) return Promise.resolve(hit);
  const pending = preparing.get(key);
  if (pending) return pending;
  const source = noteSource(url);
  if (!source) return Promise.reject(new ApiError({ status: 0, code: "voice_note_unsafe", message: "This voice message's link is not an https link, so it is not played.", hint: "Ask the sender to send it again." }));
  // Not this page's to read (loadNote plays it as it is); nothing is asked of the network for it here.
  if (!source.fetchable) return Promise.reject(new ApiError({ status: 0, code: "voice_note_direct", message: "This voice message plays as it is, without a waveform.", hint: "" }));
  const work = turn(async () => {
    let response: Response;
    try {
      response = await fetch(source.url, { credentials: "same-origin" });
    } catch (error) {
      throw networkError(error, "GET", source.url, "this site");
    }
    if (!response.ok) {
      const text = await response.text().catch(() => "");
      let body: unknown = null;
      try {
        body = text ? JSON.parse(text) : null;
      } catch {
        body = null;
      }
      const error = errorFromResponse(response, body, "GET", source.url);
      throw error.code === "unexpected_response" || error.code === "server_unavailable"
        ? new ApiError({ status: response.status, code: "voice_note_unavailable", message: `The voice message could not be loaded (HTTP ${response.status}).`, hint: response.status === 404 ? "It may have been removed from its sender's Briefcase." : "Try again in a moment." })
        : error;
    }
    return prepareFromBytes(url, await response.arrayBuffer(), response.headers.get("content-type")?.split(";")[0]?.trim() || "audio/webm");
  }).finally(() => preparing.delete(key));
  preparing.set(key, work);
  return work;
}

/** A voice note's waveform and length once it was decoded (undefined until then). */
export function noteInfoOf(url: string): NoteInfo | undefined {
  return noteInfo.get(url);
}

export function subscribeNotes(listener: () => void): () => void {
  noteListeners.add(listener);
  return () => noteListeners.delete(listener);
}

async function loadNote(url: string, durationHint: number | null): Promise<LoadedAudio> {
  const key = noteKey(url);
  try {
    return loaded(key, await prepareNote(url));
  } catch (error) {
    const failure = ApiError.from(error);
    // This tab's own copy (a blob: URL), or a link elsewhere: the element plays it, without a waveform.
    const source = noteSource(url);
    if (source && !source.fetchable) return { url: source.url, durationMs: durationHint };
    throw failure;
  }
}

/* Tracks ------------------------------------------------------------------------------------------------------------ */

const httpsArtwork = (url: string | null | undefined) => (url && url.startsWith("https://") ? url : null);

interface TrackBase {
  /** Who is speaking, for the system's media controls. */
  title: string;
  artwork?: string | null;
  origin: TrackOrigin;
  startAt?: number;
}

/** A spoken message, or a Silicon's words in its current voice. */
export function speechTrack(input: TrackBase & { source: SpeechSource; durationMs?: number | null }): Track {
  return {
    key: messageKey(input.source.address, input.source.code),
    version: fingerprint(input.source.content),
    kind: "speech",
    origin: input.origin,
    title: input.title,
    artwork: httpsArtwork(input.artwork),
    durationMs: input.durationMs ?? null,
    startAt: input.startAt,
    load: () => loadSpeech(input.source),
  };
}

/** A voice note. `key` is its message's (messageKey) when known, so Listen and the bubble share it. */
export function noteTrack(input: TrackBase & { key: string; url: string; durationMs?: number | null }): Track {
  const durationMs = input.durationMs ?? noteInfo.get(input.url)?.durationMs ?? null;
  return {
    key: input.key,
    version: input.url,
    kind: "note",
    origin: input.origin,
    title: input.title,
    artwork: httpsArtwork(input.artwork),
    durationMs,
    startAt: input.startAt,
    load: () => loadNote(input.url, durationMs),
  };
}

/** A message as something to play: its voice note, its speech, or its words in the sender's voice. */
export function messageTrack(input: { address: string; message: Message; view: string | null; origin: TrackOrigin; startAt?: number }): Track {
  const { message } = input;
  const code = message["message-id"];
  const base = { title: message.sender.display_name?.trim() || message.sender.id, artwork: message.sender.pfp_url, origin: input.origin, startAt: input.startAt };
  if (message.voice?.url) return noteTrack({ ...base, key: messageKey(input.address, code), url: message.voice.url, durationMs: message.voice.duration_milliseconds ?? null });
  const source: SpeechSource = message.speech?.script
    ? { address: input.address, code, view: input.view, content: speechContent(message.speech), speaker: null }
    : { address: input.address, code, view: input.view, content: `text:${message.updated_at ?? message.created_at}:${message.message ?? ""}`, speaker: message.sender.uuid ?? message.sender.id };
  return speechTrack({ ...base, source, durationMs: message.speech?.duration_milliseconds ?? null });
}

/** A voice sample: the sample line for `name`, in `voice`. */
export function sampleTrack(voice: Pick<VoiceOption, "key" | "name">, name: string, origin: TrackOrigin = "sample"): Track {
  const key = `sample|${voice.key}|${name}`;
  return {
    key,
    kind: "sample",
    origin,
    title: `${voice.name}, a sample`,
    load: async signal => {
      const hit = memory.get(key);
      if (hit) return loaded(key, hit);
      const stored = await persistedGet(key);
      if (stored) return loaded(key, remember(key, stored.blob, { durationMs: stored.durationMs, etag: stored.etag, speaker: null }));
      const { blob, headers } = await renderSpeech(sampleScript(name), voice.key, signal);
      const durationMs = await durationOf(blob, headers.get("x-dm-duration-ms"));
      const entry = remember(key, blob, { durationMs, etag: headers.get("etag"), speaker: null });
      void persistedPut(key, blob, { etag: headers.get("etag"), durationMs });
      return loaded(key, entry);
    },
  };
}
