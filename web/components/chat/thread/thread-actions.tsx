"use client";

/**
 * What can be done with a message, shared by every way to reach it: the hover toolbar, the context menu, the keyboard
 * (lib/shortcuts: R, E, C, L, Delete, I…) and the details dialog. ConversationView provides it; rows only call it.
 */
import { createContext, useContext } from "react";
import type { Message } from "@/lib/chat/types";

export interface ThreadActions {
  address: string;
  view: string | null;
  readOnly: boolean;
  /** Whether a message is the Carbon's own (in a view: the viewed Silicon's). */
  isMine: (message: Message) => boolean;
  reply: (message: Message) => void;
  edit: (message: Message) => void;
  copyText: (message: Message) => void;
  copyLink: (message: Message) => void;
  /** Asks in place before deleting (the row shows the question). */
  requestDelete: (message: Message) => void;
  cancelDelete: () => void;
  confirmDelete: (message: Message) => Promise<void>;
  /** The code whose delete question is showing. */
  deleting: string | null;
  openDetails: (message: Message) => void;
  /** Opens the message menu at a point (right click) or under the message (keyboard, the ⋯ button). */
  openMenu: (message: Message, at: { x: number; y: number }) => void;
  /** Scrolls to a message (loading older pages when needed) and highlights it. */
  jumpTo: (code: string) => void;
  retrySend: (key: string) => void;
  discardSend: (key: string) => void;
  focusComposer: () => void;
}

const ThreadActionsContext = createContext<ThreadActions | null>(null);

export const ThreadActionsProvider = ThreadActionsContext.Provider;

export function useThreadActions(): ThreadActions {
  const value = useContext(ThreadActionsContext);
  if (!value) throw new Error("useThreadActions() needs ConversationView (components/chat/thread/conversation-view.tsx) above it.");
  return value;
}

/** Plain text of a message for "Copy text": its words, its spoken words, or its transcript. */
export function copyableText(message: Message): string {
  if (message.deleted_at) return "";
  if (message.speech?.script) return message.speech.script;
  return message.message || message.voice_transcript || message.attachments.join("\n");
}
