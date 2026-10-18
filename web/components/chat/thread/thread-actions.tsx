"use client";

/**
 * What can be done with a message, shared by every way to reach it: the hover toolbar, the context menu, the keyboard
 * (lib/shortcuts: R, E, C, L, Space, Delete, I…) and the details dialog. ConversationView provides it; rows only call
 * it. The value is stable for the life of a conversation, so a row never re-renders because another row changed:
 * what does change (the delete question, the highlight, the message in use) reaches rows as their own props or through
 * the small stores below.
 *
 * ThreadPeople carries what the account cards and the typing line need (the conversation, presence, custodians); it
 * changes with presence polls, so only those parts read it.
 */
import { createContext, useContext, useEffect, useSyncExternalStore } from "react";
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
  /**
   * Whether Delete is offered on a message: your own; in a custodian's view, the viewed Silicon's own (the one thing a
   * custodian may change there). Never a deleted one.
   */
  canDelete: (message: Message) => boolean;
  /** Asks in place before deleting (the row shows the question). */
  requestDelete: (message: Message) => void;
  /** Takes the delete question away: any (Esc), or only `code`'s (a question answered a moment ago never takes a newer one). */
  cancelDelete: (code?: string) => void;
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
  /**
   * How many layers opened from messages are open (a message's menu, the Edited popover, an account card, Message
   * details, a photo): while any is, no toolbar shows, so there is one action layer at a time (useLayer).
   */
  layers: ValueStore<number>;
}

const ThreadActionsContext = createContext<ThreadActions | null>(null);

export const ThreadActionsProvider = ThreadActionsContext.Provider;

export function useThreadActions(): ThreadActions {
  const value = useContext(ThreadActionsContext);
  if (!value) throw new Error("useThreadActions() needs ConversationView (components/chat/thread/conversation-view.tsx) above it.");
  return value;
}

/** Counts a layer opened from a message while `open` (outside a thread, nothing to hold). */
export function useLayer(open: boolean): void {
  const layers = useContext(ThreadActionsContext)?.layers;
  useEffect(() => {
    if (!open || !layers) return;
    layers.set(layers.get() + 1);
    return () => layers.set(Math.max(0, layers.get() - 1));
  }, [open, layers]);
}

/** Whether a layer opened from a message is open (the toolbar of the message in use then steps back). */
export function useLayerOpen(): boolean {
  const layers = useThreadActions().layers;
  return useSyncExternalStore(layers.subscribe, () => layers.get() > 0, () => false);
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

/**
 * The one message in use, which shows the toolbar (message-list.tsx decides it): the message you selected (with the
 * keyboard, or by tapping it), else the one the pointer rests on. Never two at once.
 */
export interface InUse {
  key: string;
  /** Selected (keyboard, a tap), not only under the pointer: its toolbar may open under it, in the thread's flow. */
  selected: boolean;
}

/** How a row is in use ("selected", "hover") or null, so only the rows whose answer changes re-render. */
export function useInUse(store: ValueStore<InUse | null>, key: string): "selected" | "hover" | null {
  return useSyncExternalStore(
    store.subscribe,
    () => {
      const current = store.get();
      return current?.key === key ? (current.selected ? "selected" : "hover") : null;
    },
    () => null,
  );
}
