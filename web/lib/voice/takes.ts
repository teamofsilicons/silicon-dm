"use client";

/**
 * Where a recording's bytes go while the microphone records (spec §6.5). A voice message may last 48 hours, so a
 * recording never lives in memory: MediaRecorder hands over a chunk every second, chunks gather into segments of a few
 * seconds (a minute at most), and each segment is written to this browser's own disk as soon as it is complete. The
 * Origin Private File System holds them (a directory per recording, a file per segment); where it cannot be written
 * (an older Safari, a private window), IndexedDB (a record per segment); and where neither works, memory, as before.
 * Handed over, the recording is one Blob whose parts are those files, read from disk as it uploads.
 *
 * A recording on disk survives a reload, a crash or a closed tab: its record (`dm:recordings:v1` in localStorage, small
 * and written at once) says whose it is, where it was going and how long it was, and the composer offers to send it or
 * throw it away (components/chat/voice/composer-voice.tsx). A recording still being made in another tab is held by a
 * Web Lock (`dm-recording:<id>`) and never offered elsewhere. Its bytes are deleted once it is uploaded, thrown away,
 * or (a dictation, which only ever goes to transcription) left behind; a recording of another account, or of an
 * account that signed out of this browser, is deleted too, because a voice is personal.
 *
 * The pure part (when a segment is complete, which records are offered) is exported for tests/voice-takes.test.ts.
 */
import { readJson, writeJson } from "@/lib/chat/storage";
import type { RecordMode } from "./recorder";

export type TakeStore = "opfs" | "idb" | "memory";

export interface TakeInfo {
  id: string;
  /** Who recorded it (the account's uuid, else its id): it is offered to them only. */
  account: string;
  /** The conversation it is for. */
  address: string;
  mode: RecordMode;
  /** The recorder's media type (`audio/webm;codecs=opus`). */
  type: string;
  /** When recording started (Date.now()). */
  startedAt: number;
  /** How much is on disk: the time into the recording of the last chunk written. */
  durationMs: number;
  /** Bytes on disk. */
  bytes: number;
  segments: number;
  store: TakeStore;
  /** "recording" until it is handed over; a "recording" no tab holds was cut off (a reload, a crash). */
  state: "recording" | "stopped";
  /** Written every few seconds while it records (tells a live recording apart where Web Locks are missing). */
  updatedAt: number;
}

export interface Take {
  readonly id: string;
  readonly store: TakeStore;
  /** A chunk from the recorder, `elapsedMs` into the recording. */
  append: (chunk: Blob, elapsedMs: number) => void;
  /** Writes what is gathered now (the tab is being hidden or closed): as little as possible is lost. */
  flush: () => void;
  /** Bytes taken so far, written or waiting. */
  bytes: () => number;
  /** Writes the rest, and hands the recording over as one Blob (its parts on disk where they were written). */
  finish: (durationMs: number, type: string) => Promise<Blob>;
  /** Throws the recording away: its bytes and its record. */
  discard: () => Promise<void>;
}

const RECORDS_KEY = "dm:recordings:v1";
const DIRECTORY = "dm-recordings";
const DATABASE = "dm-recordings";
const SEGMENTS = "segments";
const LOCK = (id: string) => `dm-recording:${id}`;
/** A record not written for this long, of a recording no tab holds, was cut off (when Web Locks cannot tell). */
const LIVE_MS = 15_000;
const HEARTBEAT_MS = 5_000;

/* When a segment is complete (pure) -------------------------------------------------------------------------------- */

/** The longest a segment gathers (the most a crash can lose), the shortest, and its largest size. */
export const SEGMENT_MAX_MS = 60_000;
export const SEGMENT_MIN_MS = 5_000;
export const SEGMENT_MAX_BYTES = 1024 * 1024;

/**
 * Whether the chunks gathered since the last segment make one: after five seconds at first (a short message loses
 * little to a crash), growing with the recording to a minute (a long one is not thousands of files), or at a megabyte.
 */
export function segmentDue(gatheredMs: number, gatheredBytes: number, elapsedMs: number): boolean {
  if (gatheredBytes <= 0) return false;
  if (gatheredBytes >= SEGMENT_MAX_BYTES) return true;
  return gatheredMs >= Math.min(SEGMENT_MAX_MS, Math.max(SEGMENT_MIN_MS, elapsedMs / 20));
}

/** A segment's file name: its number, padded so the names sort in recording order. */
export const segmentName = (index: number) => `${String(index).padStart(7, "0")}.part`;

/**
 * Which records are a voice message to offer `account` again (newest first): its own voice messages that were stopped
 * or cut off, with something on disk, that no tab is making now. `live` says whether a tab holds one.
 */
export function offerable(records: TakeInfo[], account: string, live: (record: TakeInfo) => boolean): TakeInfo[] {
  return records
    .filter(record => record.account === account && record.mode === "note" && record.bytes > 0 && record.store !== "memory" && !live(record))
    .sort((a, b) => b.startedAt - a.startedAt);
}

/** Records nobody will be offered: dictations and empty ones no tab is making, and other accounts' (privacy). */
export function abandoned(records: TakeInfo[], account: string | null, live: (record: TakeInfo) => boolean): TakeInfo[] {
  return records.filter(record => !live(record) && (record.account !== account || record.mode !== "note" || record.bytes <= 0 || record.store === "memory"));
}

/* Records ---------------------------------------------------------------------------------------------------------- */

function readRecords(): Record<string, TakeInfo> {
  const stored = readJson<Record<string, TakeInfo> | null>(RECORDS_KEY, null);
  return stored && typeof stored === "object" ? stored : {};
}

function writeRecord(info: TakeInfo): void {
  writeJson(RECORDS_KEY, { ...readRecords(), [info.id]: info });
}

function dropRecord(id: string): void {
  const records = readRecords();
  if (!(id in records)) return;
  delete records[id];
  writeJson(RECORDS_KEY, records);
}

/* The disk: OPFS, else IndexedDB ------------------------------------------------------------------------------------ */

interface Disk {
  kind: Exclude<TakeStore, "memory">;
  write: (id: string, index: number, blob: Blob) => Promise<void>;
  read: (id: string, count: number, type: string) => Promise<Blob | null>;
  remove: (id: string) => Promise<void>;
}

async function opfsRoot(): Promise<FileSystemDirectoryHandle | null> {
  try {
    if (typeof navigator === "undefined" || !navigator.storage?.getDirectory || typeof FileSystemFileHandle === "undefined" || !("createWritable" in FileSystemFileHandle.prototype)) return null;
    const root = await navigator.storage.getDirectory();
    return await root.getDirectoryHandle(DIRECTORY, { create: true });
  } catch {
    return null;
  }
}

const opfs: Disk = {
  kind: "opfs",
  async write(id, index, blob) {
    const root = await opfsRoot();
    if (!root) throw new Error("The browser's own file system is not available.");
    const directory = await root.getDirectoryHandle(id, { create: true });
    const file = await directory.getFileHandle(segmentName(index), { create: true });
    const stream = await file.createWritable();
    try {
      await stream.write(blob);
    } finally {
      await stream.close();
    }
  },
  async read(id, count, type) {
    const root = await opfsRoot();
    if (!root) return null;
    let directory: FileSystemDirectoryHandle;
    try {
      directory = await root.getDirectoryHandle(id);
    } catch {
      return null;
    }
    // Every segment on disk, in order (one written just before a crash may not be in the record yet).
    const names: string[] = [];
    try {
      for await (const name of (directory as unknown as { keys: () => AsyncIterable<string> }).keys()) if (/^\d+\.part$/.test(name)) names.push(name);
    } catch {
      for (let index = 0; index < count; index++) names.push(segmentName(index));
    }
    names.sort();
    const parts: File[] = [];
    for (const name of names) {
      try {
        parts.push(await (await directory.getFileHandle(name)).getFile());
      } catch {
        break;
      }
    }
    return parts.length ? new Blob(parts, { type }) : null;
  },
  async remove(id) {
    const root = await opfsRoot();
    await root?.removeEntry(id, { recursive: true }).catch(() => undefined);
  },
};

let database: Promise<IDBDatabase | null> | null = null;
function openDatabase(): Promise<IDBDatabase | null> {
  database ??= new Promise(resolve => {
    try {
      if (typeof indexedDB === "undefined") return resolve(null);
      const request = indexedDB.open(DATABASE, 1);
      request.onupgradeneeded = () => {
        if (!request.result.objectStoreNames.contains(SEGMENTS)) request.result.createObjectStore(SEGMENTS, { keyPath: ["id", "index"] });
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => resolve(null);
      request.onblocked = () => resolve(null);
    } catch {
      resolve(null);
    }
  });
  return database;
}

function transaction<T>(mode: IDBTransactionMode, work: (store: IDBObjectStore) => IDBRequest<T> | void): Promise<T | undefined> {
  return openDatabase().then(db => new Promise<T | undefined>((resolve, reject) => {
    if (!db) return reject(new Error("IndexedDB is not available."));
    const tx = db.transaction(SEGMENTS, mode);
    const request = work(tx.objectStore(SEGMENTS));
    tx.oncomplete = () => resolve(request ? request.result : undefined);
    tx.onerror = () => reject(tx.error ?? new Error("IndexedDB refused the recording."));
    tx.onabort = () => reject(tx.error ?? new Error("IndexedDB refused the recording."));
  }));
}

const range = (id: string) => IDBKeyRange.bound([id, 0], [id, Number.MAX_SAFE_INTEGER]);

const idb: Disk = {
  kind: "idb",
  async write(id, index, blob) {
    await transaction("readwrite", store => store.put({ id, index, blob }));
  },
  async read(id, _count, type) {
    const rows = (await transaction<Array<{ index: number; blob: Blob }>>("readonly", store => store.getAll(range(id)) as IDBRequest<Array<{ index: number; blob: Blob }>>).catch(() => undefined)) ?? [];
    const parts = rows.sort((a, b) => a.index - b.index).map(row => row.blob);
    return parts.length ? new Blob(parts, { type }) : null;
  },
  async remove(id) {
    await transaction("readwrite", store => store.delete(range(id))).catch(() => undefined);
  },
};

/** The disk a new recording is written to: OPFS where it can be written, else IndexedDB, else none (memory). */
async function chooseDisk(): Promise<Disk | null> {
  if (await opfsRoot()) return opfs;
  if (await openDatabase()) return idb;
  return null;
}

const diskOf = (store: TakeStore): Disk | null => (store === "opfs" ? opfs : store === "idb" ? idb : null);

/* Locks ------------------------------------------------------------------------------------------------------------ */

const locksAvailable = () => typeof navigator !== "undefined" && !!navigator.locks?.request;
/** The recordings this tab holds: being made, or handed over and not yet uploaded (deleteTake lets go). */
const heldLocks = new Map<string, () => void>();

/**
 * Holds a recording's lock from its start until it is deleted (uploaded or thrown away), so neither this tab nor
 * another offers it again while this tab still sends it. Nothing is held where Web Locks are missing.
 */
function holdLock(id: string): void {
  if (!locksAvailable() || heldLocks.has(id)) return;
  let release: () => void = () => undefined;
  const held = new Promise<void>(resolve => {
    release = resolve;
  });
  heldLocks.set(id, release);
  void navigator.locks.request(LOCK(id), () => held).catch(() => undefined);
}

function releaseLock(id: string): void {
  heldLocks.get(id)?.();
  heldLocks.delete(id);
}

/** Which recordings a tab is making now (their locks are held), or null where Web Locks cannot tell. */
async function liveIds(): Promise<Set<string> | null> {
  if (!locksAvailable() || !navigator.locks.query) return null;
  try {
    const snapshot = await navigator.locks.query();
    return new Set((snapshot.held ?? []).map(lock => lock.name ?? "").filter(name => name.startsWith("dm-recording:")).map(name => name.slice("dm-recording:".length)));
  } catch {
    return null;
  }
}

async function liveTest(): Promise<(record: TakeInfo) => boolean> {
  const ids = await liveIds();
  const now = Date.now();
  if (ids) return record => ids.has(record.id);
  return record => record.state === "recording" && now - record.updatedAt < LIVE_MS;
}

/* A recording ------------------------------------------------------------------------------------------------------ */

const newId = () => (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function" ? crypto.randomUUID() : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`);

/**
 * Starts keeping a recording: its record now, its segments as they complete. Never fails: without a disk it keeps the
 * recording in memory, and a disk that refuses a segment (it is full) keeps the rest in memory.
 */
export async function startTake(meta: { account: string; address: string; mode: RecordMode; type: string }): Promise<Take> {
  const disk = await chooseDisk();
  const id = newId();
  const info: TakeInfo = { id, ...meta, startedAt: Date.now(), durationMs: 0, bytes: 0, segments: 0, store: disk?.kind ?? "memory", state: "recording", updatedAt: Date.now() };
  if (disk) {
    writeRecord(info);
    holdLock(id);
  }
  /** Chunks not on disk yet: those gathering into the next segment (all of them, in memory). */
  let gathered: Blob[] = [];
  let gatheredBytes = 0;
  let gatheredFrom = 0;
  let lastElapsed = 0;
  /** Parts kept in memory after the disk refused one (a full disk): they follow what is on disk. */
  const spilled: Blob[] = [];
  let failed = !disk;
  let total = 0;
  let writing: Promise<void> = Promise.resolve();
  let closed = false;
  const heartbeat = disk ? window.setInterval(() => writeRecord({ ...info, updatedAt: Date.now() }), HEARTBEAT_MS) : 0;

  const writeSegment = () => {
    if (!gathered.length) return;
    const parts = gathered;
    const elapsed = lastElapsed;
    const size = gatheredBytes;
    gathered = [];
    gatheredBytes = 0;
    gatheredFrom = elapsed;
    if (failed || !disk) {
      spilled.push(...parts);
      return;
    }
    const index = info.segments;
    info.segments += 1;
    writing = writing.then(async () => {
      if (failed) {
        spilled.push(...parts);
        return;
      }
      try {
        await disk.write(id, index, new Blob(parts));
        info.bytes += size;
        info.durationMs = elapsed;
        info.updatedAt = Date.now();
        if (!closed) writeRecord(info);
      } catch {
        // The disk is full or went away: the rest of this recording stays in memory (it still goes out whole).
        failed = true;
        spilled.push(...parts);
      }
    });
  };

  return {
    id,
    store: info.store,
    append(chunk, elapsedMs) {
      if (closed || !chunk.size) return;
      gathered.push(chunk);
      gatheredBytes += chunk.size;
      total += chunk.size;
      // Whole milliseconds: the length a voice message carries (duration_milliseconds) is an integer.
      lastElapsed = Math.max(lastElapsed, Math.round(elapsedMs));
      if (!failed && disk && segmentDue(lastElapsed - gatheredFrom, gatheredBytes, lastElapsed)) writeSegment();
    },
    flush() {
      if (!closed && disk && !failed) writeSegment();
    },
    bytes: () => total,
    async finish(durationMs, type) {
      writeSegment();
      await writing;
      closed = true;
      window.clearInterval(heartbeat);
      info.type = type || info.type;
      info.durationMs = Math.max(info.durationMs, Math.round(durationMs));
      info.state = "stopped";
      info.updatedAt = Date.now();
      if (disk) writeRecord(info);
      const stored = disk && info.segments ? await disk.read(id, info.segments, info.type).catch(() => null) : null;
      return new Blob(stored ? [stored, ...spilled] : spilled, { type: info.type });
    },
    async discard() {
      closed = true;
      window.clearInterval(heartbeat);
      gathered = [];
      spilled.length = 0;
      await writing.catch(() => undefined);
      dropRecord(id);
      releaseLock(id);
      await disk?.remove(id);
    },
  };
}

/* Recordings left behind -------------------------------------------------------------------------------------------- */

/**
 * The voice messages `account` recorded and never sent (a reload, a crash, a closed tab cut them off, or their upload
 * did not finish), newest first, and the recordings nobody will be offered deleted on the way (dictations, other
 * accounts', empty ones).
 */
export async function unsentTakes(account: string): Promise<TakeInfo[]> {
  const records = Object.values(readRecords());
  if (!records.length) return [];
  const live = await liveTest();
  for (const record of abandoned(records, account, live)) await deleteTake(record);
  return offerable(records, account, live);
}

/** A recording left behind, as one Blob read from disk; null when its bytes are gone. */
export async function loadTake(info: TakeInfo): Promise<Blob | null> {
  const disk = diskOf(info.store);
  if (!disk) return null;
  return disk.read(info.id, info.segments, info.type).catch(() => null);
}

/** Deletes a recording (it was uploaded, or thrown away): its bytes, its record, and this tab's hold on it. */
export async function deleteTake(info: Pick<TakeInfo, "id" | "store">): Promise<void> {
  dropRecord(info.id);
  releaseLock(info.id);
  await diskOf(info.store)?.remove(info.id).catch(() => undefined);
}

/** This tab sends a recording left behind (Send on the offer): no other tab offers it meanwhile. */
export function claimTake(info: Pick<TakeInfo, "id">): void {
  holdLock(info.id);
}

/** Deletes every recording no tab is making that is not `account`'s (another Carbon signed in here, or nobody is). */
export async function forgetTakes(account: string | null): Promise<void> {
  const records = Object.values(readRecords());
  if (!records.length) return;
  const live = await liveTest();
  for (const record of records) if (record.account !== account && !live(record)) await deleteTake(record);
}
