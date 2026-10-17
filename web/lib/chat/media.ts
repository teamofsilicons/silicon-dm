/**
 * What an attachment is, from what the sender said about it (`attachment_details.content_type`) or, for links sent
 * without details, from its address. Only https links are ever shown or opened (the service accepts nothing else).
 */
import type { AttachmentDetail, Message } from "./types";

export type MediaKind = "image" | "video" | "audio" | "file";

const EXTENSIONS: Array<[MediaKind, RegExp]> = [
  ["image", /\.(png|jpe?g|gif|webp|avif|heic|heif|bmp|svg)$/i],
  ["video", /\.(mp4|m4v|mov|webm|ogv)$/i],
  ["audio", /\.(mp3|m4a|aac|wav|ogg|oga|opus|flac|weba)$/i],
];

/** A safe https URL, or null for anything else. */
export function safeHttpsUrl(value: string | null | undefined): string | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password ? url.href : null;
  } catch {
    return null;
  }
}

export function mediaKind(url: string, contentType?: string | null): MediaKind {
  const type = contentType?.toLowerCase() ?? "";
  if (type.startsWith("image/")) return "image";
  if (type.startsWith("video/")) return "video";
  if (type.startsWith("audio/")) return "audio";
  if (type) return "file";
  let path = url;
  try {
    path = new URL(url).pathname;
  } catch {
    // Not a URL: judged by its text.
  }
  for (const [kind, pattern] of EXTENSIONS) if (pattern.test(path)) return kind;
  return "file";
}

/** The file name a link stands for: its own name, else the last part of its path. */
export function attachmentName(detail: Pick<AttachmentDetail, "url" | "name">): string {
  if (detail.name?.trim()) return detail.name.trim();
  try {
    const last = new URL(detail.url).pathname.split("/").filter(Boolean).pop();
    return last ? decodeURIComponent(last) : new URL(detail.url).hostname;
  } catch {
    return detail.url;
  }
}

export interface MessageMedia {
  /** Attachments other than the voice note and the GIF, with their details when the service gave them. */
  attachments: Array<AttachmentDetail & { kind: MediaKind }>;
}

/**
 * The attachments a message shows, in order, without the voice note and the GIF (the service also lists those two as
 * attachment links, for older clients).
 */
export function messageMedia(message: Pick<Message, "attachments" | "attachment_details" | "voice" | "gif">): MessageMedia {
  const skip = new Set([message.voice?.url, message.gif?.url].filter(Boolean) as string[]);
  const details = message.attachment_details;
  const list: AttachmentDetail[] = details && details.length === message.attachments.length ? details : message.attachments.map(url => details?.find(detail => detail.url === url) ?? { url });
  return {
    attachments: list
      .filter(detail => !skip.has(detail.url) && safeHttpsUrl(detail.url))
      .map(detail => ({ ...detail, kind: mediaKind(detail.url, detail.content_type) })),
  };
}
