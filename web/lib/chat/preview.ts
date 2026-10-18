/**
 * The one line a conversation row says about its last message (spec §6.3): "You: " before your own, the sender's first
 * name in a group, and what a message carries when it has no words ("Voice message · 0:12", "Photo", "GIF").
 */
import { firstName, sameAccount } from "./address";
import { clockDuration } from "./format";
import { plainPreview } from "./markdown";
import { messageMedia } from "./media";
import type { AccountRef, Message } from "./types";

/** The words of a script without its audio cues ("[excited] It works!" → "It works!"). */
export function spokenWords(script: string): string {
  return script.replace(/\[[^\]]*\]/g, " ").replace(/\s+/g, " ").trim();
}

/**
 * What a message is about, in a few words, without who sent it. A spoken message is its written caption when it has
 * one (the thread shows the caption as the message, the spoken words as its transcript), else what the voice says.
 */
export function messageGist(message: Message): string {
  if (message.deleted_at) return "This message was deleted";
  if (message.speech?.script) {
    const caption = plainPreview(message.message ?? "");
    if (caption) return caption;
    const words = spokenWords(message.speech.script);
    return words ? plainPreview(words) : "Spoken message";
  }
  const text = plainPreview(message.message ?? "");
  if (text) return text;
  if (message.voice) return `Voice message · ${clockDuration(message.voice.duration_milliseconds)}`;
  if (message.gif) return "GIF";
  const { attachments } = messageMedia(message);
  if (message.voice_transcript && attachments.some(item => item.kind === "audio")) return "Voice message";
  if (attachments.length) {
    const kinds = new Set(attachments.map(item => item.kind));
    const count = attachments.length;
    if (kinds.size === 1 && kinds.has("image")) return count === 1 ? "Photo" : `${count} photos`;
    if (kinds.size === 1 && kinds.has("video")) return count === 1 ? "Video" : `${count} videos`;
    if (kinds.size === 1 && kinds.has("audio")) return count === 1 ? "Audio" : `${count} audio files`;
    return count === 1 ? "File" : `${count} files`;
  }
  return "Message";
}

export interface RowPreview {
  /** "You", or the sender's first name in a group; null when the line needs no name. */
  who: string | null;
  text: string;
  /** The message has no text of its own (a deletion, a voice message): shown a little quieter. */
  quiet: boolean;
}

export function rowPreview(message: Message | null, options: { me: Pick<AccountRef, "id" | "uuid">; group: boolean }): RowPreview {
  if (!message) return { who: null, text: "No messages yet", quiet: true };
  const mine = sameAccount(message.sender, options.me);
  const who = mine ? "You" : options.group ? firstName(message.sender) : null;
  const words = message.deleted_at ? "" : plainPreview(message.message ?? "") || (message.speech?.script ? spokenWords(message.speech.script) : "");
  return { who, text: messageGist(message), quiet: !words };
}
