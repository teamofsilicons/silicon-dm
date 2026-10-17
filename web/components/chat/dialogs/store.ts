"use client";

/**
 * Which of the chat's own dialogs is open (one at a time): new conversation, new group, invitations, keyboard
 * shortcuts. Shortcuts, the palette, buttons and addresses (`/c?new=1`, `/c?new=group`) all open them through here, so
 * each is mounted once (components/chat/shell/chat-app.tsx).
 */
import { useSyncExternalStore } from "react";

export type ChatDialog = "new-conversation" | "new-group" | "invitations" | "shortcuts";

let open: ChatDialog | null = null;
const listeners = new Set<() => void>();

function emit() {
  for (const listener of listeners) listener();
}

export function openDialog(dialog: ChatDialog): void {
  if (open === dialog) return;
  open = dialog;
  emit();
}

export function closeDialog(dialog?: ChatDialog): void {
  if (!open || (dialog && open !== dialog)) return;
  open = null;
  emit();
}

export function useOpenDialog(): ChatDialog | null {
  return useSyncExternalStore(
    listener => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => open,
    () => null,
  );
}
