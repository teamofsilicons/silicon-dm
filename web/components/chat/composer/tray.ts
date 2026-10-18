/**
 * The composer's tray: what will go with the next message besides its words (files being uploaded, links, a GIF, a
 * voice note). The composer fills it from the + menu, paste, drag and drop, the GIF picker and slash commands; the
 * voice area adds its voice note through the bridge (voice-bridge.ts). A message is sent with the ready items
 * (`trayPayload`) once nothing is still on its way (`trayBusy`) and nothing failed (`trayFailed`). Pure, so it can be
 * tested on its own; tray-store.ts keeps it per conversation and runs the uploads.
 */
import type { DraftAttachment, DraftVoice, Gif, MessageCreate } from "@/lib/chat/types";
import { BIG_FILE_BYTES, progressWords } from "@/lib/upload/progress";

/**
 * Where a file's upload is, beyond its status: `preparing` (its SHA-256 is read before it is sent: Briefcase reserves a
 * file by its size and digest) or `sending`; `sent` bytes, the `rate` (bytes per second) and the seconds `left` when known;
 * `restarted` once a lost connection made it start again from its first byte.
 */
export interface FileProgress {
  /** `saving`: every byte arrived and Briefcase is storing a large file (minutes for the largest). */
  stage?: "preparing" | "sending" | "saving";
  sent?: number;
  rate?: number | null;
  left?: number | null;
  restarted?: boolean;
}

export type TrayItem =
  | ({ id: string; kind: "file"; name: string; size?: number; contentType?: string; status: "uploading" | "ready" | "failed"; progress: number; url?: string; error?: string; queued?: boolean } & FileProgress)
  | { id: string; kind: "link"; url: string; name?: string; status: "ready" }
  | { id: string; kind: "gif"; gif: Gif; status: "ready" }
  /** A voice note: uploading, then (its bytes saved, its words still coming) `transcribing`, then ready. */
  | { id: string; kind: "voice"; voice?: DraftVoice; transcript?: string; status: "uploading" | "ready" | "failed"; progress: number; error?: string; transcribing?: boolean };

export type TrayAction =
  | { type: "add"; item: TrayItem }
  /** How far it is (0 to 1); a file's upload also says its stage, bytes, speed and time left (FileProgress). */
  | ({ type: "progress"; id: string; progress: number } & Omit<FileProgress, "restarted">)
  /** A file's transfer starts again from its first byte (the connection was lost on the way). */
  | { type: "restart"; id: string }
  /** Uploaded: a file takes the name the website saved it under (folders and invisible characters taken out). */
  | { type: "ready"; id: string; url?: string; name?: string; voice?: DraftVoice; transcript?: string; size?: number; contentType?: string }
  | { type: "fail"; id: string; error: string }
  /** A voice note's bytes are saved and only its words are still coming (the tray says "Transcribing…"). */
  | { type: "transcribing"; id: string }
  | { type: "remove"; id: string }
  | { type: "clear" }
  /** A failed file goes again (waiting for its turn). */
  | { type: "retry"; id: string }
  /** A waiting file's upload started. */
  | { type: "start"; id: string }
  /** Put back what a conversation kept (tray-store.ts, from this browser's storage). */
  | { type: "restore"; items: TrayItem[] };

/** The most items one message takes from the tray: DM's own limit, 100 attachments (a voice note counts as one). */
export const TRAY_MAX_ITEMS = 100;

/** A drop of folders bringing more files than this asks before it adds them (a folder's contents are not seen). */
export const ASK_ABOVE_FILES = 20;

export function trayReducer(items: TrayItem[], action: TrayAction): TrayItem[] {
  switch (action.type) {
    case "add":
      // One GIF and one voice note per message: a new one replaces the old.
      return [...items.filter(item => item.id !== action.item.id && !(item.kind === action.item.kind && (item.kind === "gif" || item.kind === "voice"))), action.item];
    case "progress":
      return items.map(item => {
        if (item.id !== action.id || !("progress" in item)) return item;
        const progress = Math.max(0, Math.min(1, action.progress));
        if (item.kind !== "file" || !action.stage) return { ...item, progress };
        return { ...item, progress, stage: action.stage, sent: action.sent, rate: action.rate ?? null, left: action.left ?? null };
      });
    case "restart":
      return items.map(item => (item.id === action.id && item.kind === "file" && item.status === "uploading" ? { ...item, progress: 0, stage: "sending", sent: 0, rate: null, left: null, restarted: true } : item));
    case "ready":
      return items.map(item => {
        if (item.id !== action.id) return item;
        if (item.kind === "file") return { id: item.id, kind: "file", status: "ready", progress: 1, url: action.url ?? item.url, name: action.name || item.name, size: action.size ?? item.size, contentType: action.contentType ?? item.contentType };
        if (item.kind === "voice") return { ...item, status: "ready", progress: 1, error: undefined, transcribing: false, voice: action.voice ?? item.voice, transcript: action.transcript ?? item.transcript };
        return item;
      });
    case "fail":
      return items.map(item => (item.id === action.id && (item.kind === "file" || item.kind === "voice") ? { ...item, status: "failed", error: action.error, ...(item.kind === "file" ? { queued: false, stage: undefined, rate: null, left: null } : { transcribing: false }) } : item));
    case "transcribing":
      return items.map(item => (item.id === action.id && item.kind === "voice" && item.status === "uploading" ? { ...item, progress: 1, transcribing: true } : item));
    case "remove":
      return items.filter(item => item.id !== action.id);
    case "clear":
      return [];
    case "retry":
      return items.map(item => (item.id === action.id && item.kind === "file" && item.status === "failed" ? { ...item, status: "uploading", progress: 0, error: undefined, queued: true, stage: undefined, sent: 0, rate: null, left: null, restarted: false } : item));
    case "start":
      return items.map(item => (item.id === action.id && item.kind === "file" ? { ...item, queued: false } : item));
    case "restore": {
      const known = new Set(items.map(item => item.id));
      return [...action.items.filter(item => !known.has(item.id)), ...items];
    }
  }
}

/** Something in the tray is still on its way (the message waits for it). */
export const trayBusy = (items: TrayItem[]) => items.some(item => item.status === "uploading");

/** Something in the tray failed: the message waits until it is retried or removed (nothing is left out silently). */
export const trayFailed = (items: TrayItem[]) => items.some(item => item.status === "failed");

/** Something in the tray goes with the message. */
export const trayHasContent = (items: TrayItem[]) => items.some(item => item.status === "ready");

/** What the ready items add to a message. */
export function trayPayload(items: TrayItem[]): Pick<MessageCreate, "attachments" | "gif" | "voice" | "voice_transcript"> {
  const attachments: Array<string | DraftAttachment> = [];
  let gif: Gif | undefined;
  let voice: DraftVoice | undefined;
  let transcript: string | undefined;
  for (const item of items) {
    if (item.status !== "ready") continue;
    if (item.kind === "file" && item.url) attachments.push({ permanent_url: item.url, name: item.name, ...(item.contentType ? { content_type: item.contentType } : {}), ...(item.size !== undefined ? { size: item.size } : {}) });
    if (item.kind === "link") attachments.push(item.url);
    if (item.kind === "gif") gif = item.gif;
    if (item.kind === "voice" && item.voice) {
      voice = item.voice;
      transcript = item.transcript;
    }
  }
  return { ...(attachments.length ? { attachments } : {}), ...(gif ? { gif } : {}), ...(voice ? { voice, ...(transcript ? { voice_transcript: transcript } : {}) } : {}) };
}

/** What a conversation keeps of its tray in this browser: the ready items (files already saved, links, a GIF, a voice note). */
export function keptItems(items: TrayItem[]): TrayItem[] {
  return items.filter(item => item.status === "ready" && (item.kind !== "file" || !!item.url) && (item.kind !== "voice" || !!item.voice));
}

const text = (value: unknown, max = 8192): value is string => typeof value === "string" && value.length > 0 && value.length <= max;
const https = (value: unknown): value is string => text(value) && /^https:\/\//i.test(value);

/** Items read back from storage, checked one by one (anything malformed, or not ready, is left out). */
export function restoreItems(value: unknown): TrayItem[] {
  if (!Array.isArray(value)) return [];
  const out: TrayItem[] = [];
  for (const raw of value.slice(0, TRAY_MAX_ITEMS)) {
    if (!raw || typeof raw !== "object") continue;
    const item = raw as Record<string, unknown>;
    if (!text(item.id, 200) || item.status !== "ready") continue;
    const id = item.id;
    if (item.kind === "file" && https(item.url) && text(item.name, 1024)) {
      out.push({ id, kind: "file", name: item.name, url: item.url, status: "ready", progress: 1, ...(typeof item.size === "number" ? { size: item.size } : {}), ...(text(item.contentType, 255) ? { contentType: item.contentType } : {}) });
    } else if (item.kind === "link" && https(item.url)) {
      out.push({ id, kind: "link", url: item.url, status: "ready", ...(text(item.name, 1024) ? { name: item.name } : {}) });
    } else if (item.kind === "gif" && item.gif && typeof item.gif === "object") {
      const gif = item.gif as Record<string, unknown>;
      if (text(gif.provider_id, 255) && https(gif.url)) out.push({ id, kind: "gif", status: "ready", gif: { provider_id: gif.provider_id, url: gif.url, ...(https(gif.preview_url) ? { preview_url: gif.preview_url } : {}), ...(text(gif.title, 1000) ? { title: gif.title } : {}) } });
    } else if (item.kind === "voice" && item.voice && typeof item.voice === "object") {
      const voice = item.voice as Record<string, unknown>;
      if (https(voice.permanent_url)) {
        out.push({ id, kind: "voice", status: "ready", progress: 1, voice: voice as unknown as DraftVoice, ...(text(item.transcript, 20_000) ? { transcript: item.transcript } : {}) });
      }
    }
  }
  return out;
}

/** The words under a file while it uploads: waiting, preparing, the bytes on their way, saving (and saving a while); or why it waits. */
export function fileProgressWords(item: Extract<TrayItem, { kind: "file" }>, online: boolean): string {
  const total = item.size ?? 0;
  if (!online) {
    // Every byte arrived and the website saves it meanwhile: the page hears how it went once the connection is back.
    if (item.stage === "saving") return "Offline: it finishes when you are back";
    // A big file cut by a lost connection starts again from its first byte when the connection is back: say so.
    return (item.sent ?? 0) > 0 && total >= BIG_FILE_BYTES ? "Offline: it starts again from the beginning when you are back" : "Uploads when online";
  }
  if (item.queued) return progressWords({ stage: "queued", done: 0, total });
  if (item.stage === "preparing") return progressWords({ stage: "preparing", done: item.sent ?? Math.round(item.progress * total), total, left: item.left });
  if (item.stage === "saving") return progressWords({ stage: "saving", done: total, total, long: true });
  if (item.progress >= 1) return progressWords({ stage: "saving", done: total, total });
  const words = progressWords({ stage: "sending", done: item.sent ?? Math.round(item.progress * total), total, rate: item.rate, left: item.left });
  return item.restarted && total >= BIG_FILE_BYTES && (item.sent ?? 0) === 0 ? "Starting again from the beginning…" : words;
}
