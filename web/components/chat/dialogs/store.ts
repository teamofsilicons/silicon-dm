"use client";

/**
 * Which of the chat's own dialogs is open (one at a time): new conversation, new group, invitations, keyboard
 * shortcuts. Shortcuts, the palette, buttons and addresses (`/c?new=1`, `/c?new=group`) all open them through here, so
 * each is mounted once (components/chat/shell/chat-app.tsx).
 *
 * It also remembers what to give focus back to when the dialog closes (WCAG 2.4.3). Silicon UI's Dialog returns focus
 * to whatever had it when it opened, but a dialog chosen from a menu ("New" → New conversation, the account menu) opens
 * while the menu item has focus, and that item is gone by the time the dialog closes: focus would fall to the page.
 * Here a menu item stands for the button that opened its menu.
 */
import { useSyncExternalStore } from "react";

export type ChatDialog = "new-conversation" | "new-group" | "invitations" | "shortcuts";

let open: ChatDialog | null = null;
let opener: HTMLElement | null = null;
const listeners = new Set<() => void>();

function emit() {
  for (const listener of listeners) listener();
}

/** Layers whose controls never stay focused once the layer is gone. */
const LAYERS = '[role="dialog"], [role="alertdialog"], [role="menu"], [role="listbox"]';

/**
 * The last control that had focus outside every layer. A dialog chosen in the command palette opens while the palette
 * is still on its way out with focus inside it; the palette would hand focus back to this control, so the dialog does.
 */
// Kept on the window with its one listener, so a module reloaded in development reads the same value.
type Resting = Window & { __dmRestingFocus?: { element: HTMLElement | null } };
function restingFocus(): { element: HTMLElement | null } {
  const host = window as Resting;
  if (!host.__dmRestingFocus) {
    const state = { element: null as HTMLElement | null };
    host.__dmRestingFocus = state;
    document.addEventListener("focusin", event => {
      if (event.target instanceof HTMLElement && !event.target.closest(LAYERS)) state.element = event.target;
    }, true);
  }
  return host.__dmRestingFocus;
}
if (typeof document !== "undefined") restingFocus();

/**
 * The control a dialog opened from: the focused element; the button whose menu the focused item is in; or, when nothing
 * has focus or focus is in a layer on its way out (the palette after a choice: it has closed, and the chosen command
 * runs a frame later), the last control the Carbon was in.
 */
function openerOf(active: Element | null): HTMLElement | null {
  const closing = active instanceof HTMLElement && active !== document.body && !!active.closest('[role="dialog"][data-state="closed"], [role="alertdialog"][data-state="closed"]');
  if (!(active instanceof HTMLElement) || active === document.body || closing) {
    const resting = restingFocus().element;
    return resting?.isConnected ? resting : null;
  }
  const menu = active.closest<HTMLElement>('[role="menu"]');
  if (!menu) return active;
  const trigger = menu.getAttribute("aria-labelledby");
  return (trigger && document.getElementById(trigger)) || document.querySelector<HTMLElement>(`[aria-controls="${CSS.escape(menu.id)}"]`) || null;
}

export function openDialog(dialog: ChatDialog): void {
  if (open === dialog) return;
  if (typeof document !== "undefined" && !open) opener = openerOf(document.activeElement);
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

/**
 * A dialog's `onCloseAutoFocus`: focus goes back to the control it was opened from while that is still on the page and
 * nothing else took focus on purpose (the dialog opened a conversation, say). Otherwise Silicon UI's own rule applies.
 */
export function returnFocus(event: Event): void {
  const target = opener;
  opener = null;
  if (!target?.isConnected || target.closest("[inert]")) return;
  const active = document.activeElement;
  const layer = event.currentTarget instanceof Element ? event.currentTarget : null;
  if (active && active !== document.body && !layer?.contains(active)) return;
  event.preventDefault();
  target.focus({ preventScroll: true });
}

/** Forget the opener: the dialog's own outcome decides where focus goes (a new conversation focuses its composer). */
export function forgetOpener(): void {
  opener = null;
}
