/**
 * What an attachment is, from what the sender said about it (`attachment_details.content_type`) or, for links sent
 * without details, from its address. Only https links are ever shown or opened (the service accepts nothing else).
 */
import { isBriefcaseFile } from "../upload/display";
import type { AttachmentDetail, Message } from "./types";

export type MediaKind = "image" | "video" | "audio" | "file" | "link";

const EXTENSIONS: Array<[MediaKind, RegExp]> = [
  ["image", /\.(png|jpe?g|gif|webp|avif|heic|heif|bmp|svg)$/i],
  ["video", /\.(mp4|m4v|mov|webm|ogv)$/i],
  ["audio", /\.(mp3|m4a|aac|wav|ogg|oga|opus|flac|weba)$/i],
  ["file", /\.(pdf|docx?|xlsx?|pptx?|od[stp]|rtf|txt|csv|tsv|md|json|xml|ya?ml|zip|7z|rar|tar|gz|bz2|xz|tgz|epub|ics|vcf|dmg|pkg|exe|msi|apk|wasm)$/i],
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

export function mediaKind(url: string, contentType?: string | null, details?: Pick<AttachmentDetail, "name" | "size">): MediaKind {
  const type = contentType?.split(";")[0]?.trim().toLowerCase() ?? "";
  if (type.startsWith("image/")) return "image";
  if (type.startsWith("video/")) return "video";
  if (type.startsWith("audio/")) return "audio";
  // A saved upload stays a file, including extensionless files and HTML. A web page's MIME type, by itself,
  // describes a link; it must not turn a Calc sheet or an article into a downloadable document.
  const file = isBriefcaseFile(url) || details?.size != null;
  if (type === "text/html" || type === "application/xhtml+xml") return file ? "file" : "link";
  if (type) return "file";
  let path = url;
  try {
    path = decodeURIComponent(new URL(url).pathname);
  } catch {
    // Not a URL: judged by its text.
  }
  for (const [kind, pattern] of EXTENSIONS) if (pattern.test(path) || pattern.test(details?.name?.trim() ?? "")) return kind;
  // The service stores web links and files in the same list. Only evidence of a file earns a file card;
  // an arbitrary last path segment (a sheet id, for example) is not a file name.
  return file ? "file" : "link";
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
      .map(detail => ({ ...detail, kind: mediaKind(detail.url, detail.content_type, detail) })),
  };
}
