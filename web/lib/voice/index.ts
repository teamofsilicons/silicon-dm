"use client";

/**
 * The voice module's public face (phase 2 fills it in: the playback engine, Listen, Listen mode, recording and
 * dictation; spec §6.6). Until then everything here answers "not yet", so the website shows the voice controls
 * switched off with the reason instead of controls that do nothing.
 *
 * Callers depend only on these exports: the thread header (Listen mode), message actions (Listen), the composer (record,
 * dictate) and Settings (Voice, Listening). The voice area may change everything behind them.
 */
import type { Message } from "@/lib/chat/types";

export interface VoiceAvailability {
  /** Listening to messages (speech rendered by the service). */
  listen: boolean;
  /** Recording voice notes and dictation (needs the microphone and transcription). */
  record: boolean;
  /** Why something is off, in the words the controls show. */
  reason: string;
}

const NOT_YET: VoiceAvailability = { listen: false, record: false, reason: "Voice arrives with the next part of DM: listening, voice notes and dictation." };

/** What voice features this website offers right now. */
export function useVoiceAvailability(): VoiceAvailability {
  return NOT_YET;
}

export interface ListenModeHandle {
  available: boolean;
  on: boolean;
  /** A word for what it is doing ("Speaking…"), or null. */
  status: string | null;
  toggle: () => void;
}

/** Listen mode for a conversation (new Silicon messages are spoken in order while it is on). */
export function useListenMode(address: string | null): ListenModeHandle {
  void address;
  return { available: false, on: false, status: null, toggle: () => undefined };
}

/** Whether "Listen" applies to a message (a Silicon's text or speech). */
export function canListen(message: Pick<Message, "sender" | "deleted_at" | "message" | "speech">): boolean {
  if (message.deleted_at) return false;
  return message.sender.type === "silicon" && (!!message.speech?.script || !!message.message?.trim());
}

/** Plays (or pauses) a message in its sender's voice. The voice area implements it. */
export function toggleListen(address: string, message: Message): void {
  void address;
  void message;
}
