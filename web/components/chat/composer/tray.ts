/**
 * The composer's tray: what will go with the next message besides its words (files being uploaded, links, a GIF, a
 * voice note). The composer area (phase 2) fills it from the + menu, paste, drag and drop, the GIF picker and the
 * microphone; the composer only renders it and sends what is ready. Pure, so it can be tested on its own.
 */
import type { DraftAttachment, DraftVoice, Gif, MessageCreate } from "@/lib/chat/types";

export type TrayItem =
  | { id: string; kind: "file"; name: string; size?: number; contentType?: string; status: "uploading" | "ready" | "failed"; progress: number; url?: string; error?: string }
  | { id: string; kind: "link"; url: string; name?: string; status: "ready" }
  | { id: string; kind: "gif"; gif: Gif; status: "ready" }
  | { id: string; kind: "voice"; voice?: DraftVoice; transcript?: string; status: "uploading" | "ready" | "failed"; progress: number; error?: string };

export type TrayAction =
  | { type: "add"; item: TrayItem }
  | { type: "progress"; id: string; progress: number }
  | { type: "ready"; id: string; url?: string; voice?: DraftVoice; transcript?: string; size?: number; contentType?: string }
  | { type: "fail"; id: string; error: string }
  | { type: "remove"; id: string }
  | { type: "clear" };

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
        if (item.kind === "file") return { ...item, status: "ready", progress: 1, url: action.url ?? item.url, size: action.size ?? item.size, contentType: action.contentType ?? item.contentType };
        if (item.kind === "voice") return { ...item, status: "ready", progress: 1, voice: action.voice ?? item.voice, transcript: action.transcript ?? item.transcript };
        return item;
      });
    case "fail":
      return items.map(item => (item.id === action.id && (item.kind === "file" || item.kind === "voice") ? { ...item, status: "failed", error: action.error } : item));
    case "remove":
      return items.filter(item => item.id !== action.id);
    case "clear":
      return [];
  }
}

/** Something in the tray is still on its way (the message waits for it). */
export const trayBusy = (items: TrayItem[]) => items.some(item => item.status === "uploading");

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
