/**
 * The composer's tray: what will go with the next message besides its words (files being uploaded, links, a GIF, a
 * voice note). The composer fills it from the + menu, paste, drag and drop, the GIF picker and slash commands; the
 * voice area adds its voice note through the bridge (voice-bridge.ts). A message is sent with the ready items
 * (`trayPayload`) once nothing is still on its way (`trayBusy`) and nothing failed (`trayFailed`). Pure, so it can be
 * tested on its own; tray-store.ts keeps it per conversation and runs the uploads.
 */
import type { DraftAttachment, DraftVoice, Gif, MessageCreate } from "@/lib/chat/types";

export type TrayItem =
  | { id: string; kind: "file"; name: string; size?: number; contentType?: string; status: "uploading" | "ready" | "failed"; progress: number; url?: string; error?: string; queued?: boolean }
  | { id: string; kind: "link"; url: string; name?: string; status: "ready" }
  | { id: string; kind: "gif"; gif: Gif; status: "ready" }
  | { id: string; kind: "voice"; voice?: DraftVoice; transcript?: string; status: "uploading" | "ready" | "failed"; progress: number; error?: string };

export type TrayAction =
  | { type: "add"; item: TrayItem }
  | { type: "progress"; id: string; progress: number }
  /** Uploaded: a file takes the name the website saved it under (folders and invisible characters taken out). */
  | { type: "ready"; id: string; url?: string; name?: string; voice?: DraftVoice; transcript?: string; size?: number; contentType?: string }
  | { type: "fail"; id: string; error: string }
  | { type: "remove"; id: string }
  | { type: "clear" }
  /** A failed file goes again (waiting for its turn). */
  | { type: "retry"; id: string }
  /** A waiting file's upload started. */
  | { type: "start"; id: string }
  /** Put back what a conversation kept (tray-store.ts, from this browser's storage). */
  | { type: "restore"; items: TrayItem[] };

/** The most items one message takes from the tray (DM takes up to 100 attachments; a tray stays readable). */
export const TRAY_MAX_ITEMS = 20;

export function trayReducer(items: TrayItem[], action: TrayAction): TrayItem[] {
  switch (action.type) {
    case "add":
      // One GIF and one voice note per message: a new one replaces the old.
      return [...items.filter(item => item.id !== action.item.id && !(item.kind === action.item.kind && (item.kind === "gif" || item.kind === "voice"))), action.item];
    case "progress":
      return items.map(item => (item.id === action.id && "progress" in item ? { ...item, progress: Math.max(0, Math.min(1, action.progress)) } : item));
    case "ready":
      return items.map(item => {
        if (item.id !== action.id) return item;
        if (item.kind === "file") return { ...item, status: "ready", progress: 1, queued: false, error: undefined, url: action.url ?? item.url, name: action.name || item.name, size: action.size ?? item.size, contentType: action.contentType ?? item.contentType };
        if (item.kind === "voice") return { ...item, status: "ready", progress: 1, error: undefined, voice: action.voice ?? item.voice, transcript: action.transcript ?? item.transcript };
        return item;
      });
    case "fail":
      return items.map(item => (item.id === action.id && (item.kind === "file" || item.kind === "voice") ? { ...item, status: "failed", error: action.error, ...(item.kind === "file" ? { queued: false } : {}) } : item));
    case "remove":
      return items.filter(item => item.id !== action.id);
    case "clear":
      return [];
    case "retry":
      return items.map(item => (item.id === action.id && item.kind === "file" && item.status === "failed" ? { ...item, status: "uploading", progress: 0, error: undefined, queued: true } : item));
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
