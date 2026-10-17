"use client";

/**
 * What can be done with a message, shared by every way to reach it: the hover toolbar, the context menu, the keyboard
 * (lib/shortcuts: R, E, C, L, Space, Delete, I…) and the details dialog. ConversationView provides it; rows only call
 * it. The value is stable for the life of a conversation, so a row never re-renders because another row changed:
 * what does change (the delete question, the highlight, the pointer) reaches rows as their own props or through the
 * small stores below.
 *
 * ThreadPeople carries what the account cards and the typing line need (the conversation, presence, custodians); it
 * changes with presence polls, so only those parts read it.
 */
import { createContext, useContext, useSyncExternalStore } from "react";
import type { AccountRef, ConversationSummary, Message, Presence } from "@/lib/chat/types";

export interface ThreadActions {
  address: string;
  view: string | null;
  readOnly: boolean;
  /** Whether a message is the Carbon's own (in a view: the viewed Silicon's). */
  isMine: (message: Message) => boolean;
  /** Whether an account is the signed-in Carbon. */
  isSelf: (account: AccountRef) => boolean;
  reply: (message: Message) => void;
  edit: (message: Message) => void;
  copyText: (message: Message) => void;
  copyLink: (message: Message) => void;
  /** Asks in place before deleting (the row shows the question). */
  requestDelete: (message: Message) => void;
  cancelDelete: () => void;
  confirmDelete: (message: Message) => Promise<void>;
  openDetails: (message: Message) => void;
  /** Opens the message menu at a point (right click) or under the message (keyboard, the ⋯ button). */
  openMenu: (message: Message, at: { x: number; y: number }) => void;
  /** Scrolls to a message (loading older pages when needed) and highlights it. */
  jumpTo: (code: string) => void;
  /** Plays or pauses a message in its sender's voice (lib/voice). */
  listen: (message: Message) => void;
  /** Opens your direct conversation with an account (the account card's "Message"). */
  openDirect: (account: AccountRef) => void;
  /** Copies an account's id. */
  copyId: (account: AccountRef) => void;
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

/** Plain text of a message for "Copy text" (thread-model.ts copyableText: what the bubble shows). */
export { copyableText } from "./thread-model";

/* People ------------------------------------------------------------------------------------------------------------ */

export interface ThreadPeople {
  conversation: ConversationSummary | undefined;
  presence: Map<string, Presence>;
  /** Who looks after a Silicon, when the thread can tell (thread-model.ts custodianOf). */
  custodianOf: (account: AccountRef) => AccountRef | null;
}

const ThreadPeopleContext = createContext<ThreadPeople | null>(null);

export const ThreadPeopleProvider = ThreadPeopleContext.Provider;

export function useThreadPeople(): ThreadPeople {
  const value = useContext(ThreadPeopleContext);
  if (!value) throw new Error("useThreadPeople() needs ConversationView above it.");
  return value;
}

/* A tiny store: one value many rows watch, each re-rendering only when its own answer changes. --------------------- */

export interface ValueStore<T> {
  get: () => T;
  set: (next: T) => void;
  subscribe: (listener: () => void) => () => void;
}

export function valueStore<T>(initial: T): ValueStore<T> {
  let value = initial;
  const listeners = new Set<() => void>();
  return {
    get: () => value,
    set: next => {
      if (Object.is(next, value)) return;
      value = next;
      for (const listener of listeners) listener();
    },
    subscribe: listener => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

/** Whether the store holds `key` (the row the pointer is over): true or false, so only two rows re-render per move. */
export function useIsCurrent(store: ValueStore<string | null>, key: string): boolean {
  return useSyncExternalStore(store.subscribe, () => store.get() === key, () => false);
}
