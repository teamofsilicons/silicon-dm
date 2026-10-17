/**
 * What the composer hands its microphone: the contract between the composer area (components/chat/composer/**) and
 * the voice area (components/chat/voice/**, `<ComposerVoice bridge={…} />`). The composer owns this file; the voice
 * area records, and drives everything after the recording through these calls, so neither edits the other's files.
 *
 * A voice note: add a `voice` tray item (status "uploading") → upload it with `uploadFile` (lib/upload/client.ts) and
 * transcribe it (`transcribe` in lib/chat/api.ts) in parallel → `ready` with the voice object and the transcript (or
 * `fail`) → `sendWhenReady()` when the Carbon stopped with Enter or ✓. Dictation: `insertText` at the caret.
 */
import type { TrayAction } from "./tray";

export interface ComposerVoiceBridge {
  /** The open conversation's address. */
  address: string;
  /** Why nothing can be sent here (a custodian's read-only view, a conversation you left), or null. */
  disabledReason: string | null;
  /** Add, update or remove the voice note's tray item. */
  dispatchTray: (action: TrayAction) => void;
  /** Send the composer's message (words and tray) as soon as nothing in the tray is still uploading. */
  sendWhenReady: () => void;
  /** Put text at the caret (replacing the selection), as dictation does, and keep the field focused. */
  insertText: (text: string) => void;
  /** Give the keyboard back to the field (after recording ends or is cancelled). */
  focusField: () => void;
  /**
   * The microphone registers what it can start (null when it unmounts), so the composer's slash commands `/voice` and
   * `/dictate` start the same flows as ⌘⇧M and ⌘⇧D.
   */
  registerControls: (controls: ComposerVoiceControls | null) => void;
}

export interface ComposerVoiceControls {
  /** Start recording a voice note (or, when uploads are off, explain why and offer dictation). */
  record: () => void;
  /** Start dictation: record, transcribe, insert the words at the caret. */
  dictate: () => void;
}
