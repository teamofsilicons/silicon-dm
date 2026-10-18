"use client";

/**
 * The tray at run time (the model is ./tray.ts): one tray per account and conversation, kept outside the components so
 * an upload keeps going when you switch to another conversation and is there when you come back. Files of any size
 * upload into your Silicon Briefcase (lib/upload/client.ts), at most three at a time (the rest wait their turn), with
 * their progress in the tray: preparing (the file's SHA-256), then the bytes sent with the speed and the time left
 * (lib/upload/progress.ts), at most a few updates a second. Removing an item that is on its way cancels it (the
 * preparation or the transfer stops, and the website's server cancels Briefcase's reservation); a failed one can be
 * retried (this tab keeps the File, and its digest) or removed. What is ready (files already
 * saved, links, a GIF, a voice note) is also kept in this browser per conversation (`dm:tray:v1:<account>:<address>`),
 * so a reload or another conversation in between loses nothing; sending clears it.
 *
 * The wish to send when the uploads are done (./send-latch.ts) lives here too, one per conversation: Enter while a file
 * uploads, then opening another conversation, still sends it. Whoever can send it registers a `TraySender`: the
 * composer while it is on screen, and what it leaves behind when it goes (the words then, and a note if it cannot go).
 *
 * Offline, uploads wait for the connection (lib/upload/client.ts) and the tray says so; one that failed because the
 * website could not be reached goes again by itself when the browser is back online. Leaving the page while a file is
 * still on its way, or while a send waits for one, asks first (the browser's own question): it would be lost.
 *
 * While a file uploads, the others in the conversation see "uploading a file…" (reportActivity, throttled there).
 */
import { useCallback, useMemo, useSyncExternalStore } from "react";
import { reportActivity } from "@/lib/chat/presence";
import { readJson, writeJson } from "@/lib/chat/storage";
import { ApiError } from "@/lib/errors";
import { browserOnline, uploadFile } from "@/lib/upload/client";
import { BIG_FILE_BYTES, bytesWords, RateMeter } from "@/lib/upload/progress";
import { SendLatch, trayState, type SendDecision } from "./send-latch";
import { keptItems, restoreItems, TRAY_MAX_ITEMS, trayReducer, type TrayAction, type TrayItem } from "./tray";

const MAX_PARALLEL = 3;
/** A file's progress reaches the tray at most this often (and at every change of stage, and at its end). */
const PROGRESS_MS = 250;
/** Files smaller than this are prepared in a blink: the tray never says "Preparing…" for them. */
const PREPARE_SHOWN_FROM = 16 * 1024 * 1024;

/** Whoever sends a conversation's waiting message (the latch decides when). */
export interface TraySender {
  /** The words that would go with it now. */
  words: () => string;
  /** Sends the words and the tray's ready items now (and empties the tray). */
  send: () => void;
  /** It cannot go: something failed and waits for Retry or Remove (say so). */
  blocked: (items: TrayItem[]) => void;
}

interface Snapshot {
  items: TrayItem[];
  /** A send waits for the uploads. */
  pending: boolean;
}

interface Entry {
  key: string;
  address: string;
  items: TrayItem[];
  latch: SendLatch;
  sender: TraySender | null;
  snapshot: Snapshot;
  listeners: Set<() => void>;
}

const EMPTY: Snapshot = { items: [], pending: false };
const entries = new Map<string, Entry>();
/** This tab's files by item id (to upload, and to retry), their uploads, and their thumbnails (object URLs). */
const files = new Map<string, File>();
const running = new Map<string, AbortController>();
const previews = new Map<string, string>();
/** Item ids waiting for an upload slot, oldest first, with the tray they belong to. */
const waiting: Array<{ entry: Entry; id: string }> = [];
/** Files that failed because the website could not be reached: they go again when the browser is back online. */
const unreachable = new Map<string, Entry>();

const storageKey = (account: string, address: string) => `dm:tray:v1:${account}:${address}`;

function entryFor(account: string, address: string): Entry {
  const key = storageKey(account, address);
  let entry = entries.get(key);
  if (!entry) {
    const items = restoreItems(readJson<unknown>(key, null));
    entry = { key, address, items, latch: new SendLatch(), sender: null, snapshot: { items, pending: false }, listeners: new Set() };
    entries.set(key, entry);
    watchWindow();
  }
  return entry;
}

function publish(entry: Entry) {
  if (entry.snapshot.items !== entry.items || entry.snapshot.pending !== entry.latch.pending) entry.snapshot = { items: entry.items, pending: entry.latch.pending };
  for (const listener of entry.listeners) listener();
  guardLeaving();
}

/** The tray changed: a waiting send goes now (exactly once), or is told why it cannot. */
function settle(entry: Entry) {
  const sender = entry.sender;
  if (!entry.latch.pending || !sender) return;
  const outcome = entry.latch.settle(trayState(entry.items, sender.words()));
  if (outcome === "send") sender.send();
  else if (outcome === "blocked") sender.blocked(entry.items);
}

function dispatchTo(entry: Entry, action: TrayAction) {
  if (action.type === "progress" && !action.stage) {
    // One change per percent (a voice note's upload): progress comes far more often than anyone can see it move. A
    // file's progress (with its stage) is already paced by its upload.
    const item = entry.items.find(candidate => candidate.id === action.id);
    if (item && "progress" in item && Math.floor(item.progress * 100) === Math.floor(Math.max(0, Math.min(1, action.progress)) * 100)) return;
  }
  const next = trayReducer(entry.items, action);
  if (next === entry.items) return;
  entry.items = next;
  // Progress never changes what is kept (only ready items are), so it never writes to storage.
  if (action.type !== "progress" && action.type !== "restart") {
    const kept = keptItems(next);
    writeJson(entry.key, kept.length ? kept : null);
  }
  settle(entry);
  publish(entry);
}

function release(id: string) {
  running.get(id)?.abort();
  running.delete(id);
  unreachable.delete(id);
  const index = waiting.findIndex(job => job.id === id);
  if (index >= 0) waiting.splice(index, 1);
  const preview = previews.get(id);
  if (preview) {
    previews.delete(id);
    // A moment later: the tray item may still be on screen for a frame.
    setTimeout(() => URL.revokeObjectURL(preview), 1000);
  }
  files.delete(id);
}

/** Words for a failed upload: the service's own message and what to do. */
const failureWords = (error: unknown) => {
  const failure = ApiError.from(error);
  return [failure.message, failure.hint].filter(Boolean).join(" ");
};

function pump() {
  while (running.size < MAX_PARALLEL && waiting.length) {
    const job = waiting.shift()!;
    const file = files.get(job.id);
    if (!file) continue;
    upload(job.entry, job.id, file);
  }
}

function upload(entry: Entry, id: string, file: File) {
  const controller = new AbortController();
  running.set(id, controller);
  dispatchTo(entry, { type: "start", id });
  const meter = new RateMeter();
  const preparing = new RateMeter();
  let shownAt = 0;
  let shownStage = "";
  let shownPercent = -1;
  /**
   * One render per change worth seeing, at most four a second: a new stage, the start, the end, a new percent, and for
   * a big file (whose words are bytes, speed and time left) every quarter of a second.
   */
  const report = (stage: "preparing" | "sending", done: number, total: number) => {
    if (controller.signal.aborted) return;
    const now = performance.now();
    const timing = stage === "sending" ? meter : preparing;
    timing.note(done, now);
    const progress = total > 0 ? Math.min(1, done / total) : 0;
    const percent = Math.floor(progress * 100);
    const changed = stage !== shownStage || done >= total || done === 0;
    if (!changed && (now - shownAt < PROGRESS_MS || (percent === shownPercent && total < BIG_FILE_BYTES))) return;
    shownAt = now;
    shownStage = stage;
    shownPercent = percent;
    dispatchTo(entry, { type: "progress", id, progress, stage, sent: done, rate: timing.rate(), left: timing.left(total) });
  };
  // A file big enough to take a moment says so from the start (its SHA-256 is read before any byte is sent).
  if (file.size >= PREPARE_SHOWN_FROM) report("preparing", 0, file.size);
  uploadFile(file, {
    name: file.name || "file",
    contentType: file.type || undefined,
    signal: controller.signal,
    onPrepare: (done, total) => {
      if (total >= PREPARE_SHOWN_FROM) report("preparing", done, total);
    },
    onProgress: (sent, total) => {
      // Every byte sent is 100 %: the item then says "Saving…" while the website hands the file to Briefcase.
      report("sending", sent, total);
      if (sent > 0) reportActivity("uploading_file");
    },
    onRestart: () => {
      meter.reset();
      shownStage = "";
      dispatchTo(entry, { type: "restart", id });
    },
    onSaving: () => {
      if (!controller.signal.aborted) dispatchTo(entry, { type: "progress", id, progress: 1, stage: "saving", sent: file.size, rate: null, left: null });
    },
  }).then(
    uploaded => {
      if (controller.signal.aborted) return;
      dispatchTo(entry, { type: "ready", id, url: uploaded.url, name: uploaded.name, size: uploaded.size, contentType: uploaded.content_type });
    },
    error => {
      if (controller.signal.aborted) return;
      const failure = ApiError.from(error);
      if (failure.isNetwork && files.has(id)) unreachable.set(id, entry);
      dispatchTo(entry, { type: "fail", id, error: failureWords(failure) });
    },
  ).finally(() => {
    if (running.get(id) === controller) running.delete(id);
    pump();
    guardLeaving();
  });
}

function retryIn(entry: Entry, id: string) {
  if (!files.has(id)) return;
  unreachable.delete(id);
  dispatchTo(entry, { type: "retry", id });
  waiting.push({ entry, id });
  pump();
}

/** The largest file this website takes (GET /api/uploads/config): 0 when not known (the upload itself then says). */
export interface UploadLimit {
  maxBytes: number;
  /** The website operator's own limit (UPLOADS_MAX_BYTES) rather than Silicon Briefcase's 5 TiB. */
  capped?: boolean;
}

/** Why a file cannot go, before any byte is read: empty, or larger than the limit. Null when it can. */
export function refusalOf(file: Pick<File, "size">, limit: UploadLimit): string | null {
  if (file.size === 0) return "This file is empty (0 bytes), so there is nothing to send. Remove it.";
  if (limit.maxBytes > 0 && file.size > limit.maxBytes) {
    return limit.capped
      ? `It is larger than ${bytesWords(limit.maxBytes)}, the most this DM website takes. Remove it, or share a link to it instead.`
      : `It is larger than ${bytesWords(limit.maxBytes)}, the most Silicon Briefcase takes in one file. Remove it, or share a link to it instead.`;
  }
  return null;
}

/** Adds files to a tray and starts uploading them. Returns how many fit (TRAY_MAX_ITEMS per message). */
function addFilesTo(entry: Entry, list: File[], limit: UploadLimit): number {
  const room = Math.max(0, TRAY_MAX_ITEMS - entry.items.length);
  const taken = list.slice(0, room);
  for (const file of taken) {
    const id = `file-${crypto.randomUUID()}`;
    const name = file.name || "file";
    const base = { id, kind: "file" as const, name, size: file.size, contentType: file.type || undefined, progress: 0 };
    const refusal = refusalOf(file, limit);
    if (refusal) {
      dispatchTo(entry, { type: "add", item: { ...base, status: "failed", error: refusal } });
      continue;
    }
    files.set(id, file);
    if (file.type.startsWith("image/") && typeof URL.createObjectURL === "function") previews.set(id, URL.createObjectURL(file));
    dispatchTo(entry, { type: "add", item: { ...base, status: "uploading", queued: true } });
    waiting.push({ entry, id });
  }
  pump();
  return taken.length;
}

/* The window: back online, and leaving the page. ----------------------------------------------------------------- */

let watching = false;

/**
 * Another tab changed what this conversation keeps (it added a link, a file finished there, or it sent the message):
 * this tab takes the same ready items, keeping its own uploads still on their way (they are never kept). Both tabs then
 * show the same tray, and a message sent in one is not sent again from the other.
 */
function adopt(entry: Entry, raw: string | null) {
  let stored: unknown = null;
  try {
    stored = raw ? JSON.parse(raw) : null;
  } catch {
    stored = null;
  }
  const ready = restoreItems(stored);
  const keep = new Set(ready.map(item => item.id));
  for (const item of entry.items) if (item.status === "ready" && !keep.has(item.id)) release(item.id);
  entry.items = [...ready, ...entry.items.filter(item => item.status !== "ready" && !keep.has(item.id))];
  settle(entry);
  publish(entry);
}

function watchWindow() {
  if (watching || typeof window === "undefined") return;
  watching = true;
  window.addEventListener("storage", event => {
    if (!event.key?.startsWith("dm:tray:v1:")) return;
    const entry = entries.get(event.key);
    if (entry) adopt(entry, event.newValue);
  });
  const changed = () => {
    // The trays say "Waiting for the connection…" while offline.
    for (const entry of entries.values()) for (const listener of entry.listeners) listener();
  };
  window.addEventListener("offline", changed);
  window.addEventListener("online", () => {
    // What failed only because the website was out of reach goes again now.
    for (const [id, entry] of [...unreachable]) {
      const item = entry.items.find(candidate => candidate.id === id);
      if (item?.status === "failed") retryIn(entry, id);
      else unreachable.delete(id);
    }
    changed();
  });
}

/** Something would be lost by leaving the page now: a file on its way, or a send waiting for one. */
const unfinished = () => running.size > 0 || waiting.length > 0 || [...entries.values()].some(entry => entry.latch.pending);

function onLeaving(event: BeforeUnloadEvent) {
  if (!unfinished()) return;
  event.preventDefault();
  // Older browsers ask only when a value is set.
  event.returnValue = "";
}

let guarding = false;

/** Asks before the page goes only while something is unfinished (a page with the question set is never kept in the back/forward cache). */
function guardLeaving() {
  if (typeof window === "undefined") return;
  const wanted = unfinished();
  if (wanted === guarding) return;
  guarding = wanted;
  if (wanted) window.addEventListener("beforeunload", onLeaving);
  else window.removeEventListener("beforeunload", onLeaving);
}

/** Whether the browser is online, kept current for the tray's words. */
export function useBrowserOnline(): boolean {
  return useSyncExternalStore(
    useCallback((listener: () => void) => {
      window.addEventListener("online", listener);
      window.addEventListener("offline", listener);
      return () => {
        window.removeEventListener("online", listener);
        window.removeEventListener("offline", listener);
      };
    }, []),
    browserOnline,
    () => true,
  );
}

/* The hook. ------------------------------------------------------------------------------------------------------- */

export interface TrayHandle {
  items: TrayItem[];
  /** A send waits for the uploads ("Sends when … is uploaded"). */
  pending: boolean;
  dispatch: (action: TrayAction) => void;
  /** Adds files and uploads them; answers how many fit in this message. */
  addFiles: (list: File[], limit: UploadLimit) => number;
  retry: (id: string) => void;
  remove: (id: string) => void;
  /** The message was sent (or the Carbon discarded what it carried): empty, here and in storage. */
  clear: () => void;
  /**
   * Asks to send these words with the tray: "send" (do it now), "wait" (it goes by itself when the uploads are done,
   * through the registered sender), "blocked" (something failed) or "nothing".
   */
  request: (words: string) => SendDecision;
  /** Takes back a waiting send (Esc, or a change to what goes with it). Answers whether one was waiting. */
  cancelPending: () => boolean;
  /** Who sends the waiting message when it can go (the composer, or what it leaves behind); a waiting one is checked at once. */
  setSender: (sender: TraySender | null) => void;
  /** A thumbnail for an image this tab is uploading (an object URL), if any. */
  previewOf: (id: string) => string | undefined;
  /** Whether a failed file can go again (this tab still has it; a file refused before uploading cannot). */
  canRetry: (id: string) => boolean;
  /** The items as they are right now (for callbacks that outlive a render). */
  current: () => TrayItem[];
  /** Calls `listener` after every change of this tray (uploads finishing, the microphone's voice note). */
  subscribe: (listener: () => void) => () => void;
}

export type TrayActions = Omit<TrayHandle, "items" | "pending">;

/** What can be done with one conversation's tray, outside React too (useTray hands out the same, and the tests use it). */
export function trayActions(account: string, address: string, enabled = true): TrayActions {
  const entry = () => entryFor(account, address);
  return {
    dispatch: action => {
      if (enabled) dispatchTo(entry(), action);
    },
    addFiles: (list, limit) => (enabled ? addFilesTo(entry(), list, limit) : 0),
    retry: id => {
      if (enabled) retryIn(entry(), id);
    },
    remove: id => {
      release(id);
      dispatchTo(entry(), { type: "remove", id });
      guardLeaving();
    },
    clear: () => {
      for (const item of entry().items) release(item.id);
      dispatchTo(entry(), { type: "clear" });
      guardLeaving();
    },
    request: words => {
      if (!enabled) return "nothing";
      const target = entry();
      const decision = target.latch.request(trayState(target.items, words));
      publish(target);
      return decision;
    },
    cancelPending: () => {
      if (!enabled) return false;
      const target = entry();
      const was = target.latch.cancel();
      if (was) publish(target);
      return was;
    },
    setSender: sender => {
      if (!enabled) return;
      const target = entry();
      target.sender = sender;
      settle(target);
      publish(target);
    },
    previewOf: id => previews.get(id),
    canRetry: id => files.has(id),
    current: () => (enabled ? entry().items : EMPTY.items),
    subscribe: listener => {
      if (!enabled) return () => undefined;
      const target = entry();
      target.listeners.add(listener);
      return () => target.listeners.delete(listener);
    },
  };
}

/** The tray of one conversation. `enabled` is false in a custodian's read-only view: nothing is kept or uploaded. */
export function useTray(account: string, address: string, enabled = true): TrayHandle {
  const snapshot = useSyncExternalStore(
    useCallback((listener: () => void) => {
      if (!enabled) return () => undefined;
      const entry = entryFor(account, address);
      entry.listeners.add(listener);
      return () => entry.listeners.delete(listener);
    }, [account, address, enabled]),
    () => (enabled ? entryFor(account, address).snapshot : EMPTY),
    () => EMPTY,
  );
  // The actions stay the same object while the conversation does (they are safe in effects and in the bridge).
  const actions = useMemo(() => trayActions(account, address, enabled), [account, address, enabled]);
  return useMemo(() => ({ items: snapshot.items, pending: snapshot.pending, ...actions }), [snapshot, actions]);
}

