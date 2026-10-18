"use client";

/**
 * Where the keyboard goes as conversations open and close, so that switching is "switch, then type" (spec §6.7; WCAG
 * 2.4.3). The conversation view is keyed by its address, so the message field that had the keyboard goes away with the
 * conversation before, and focus falls to the page itself:
 *
 * - A conversation that opens with the keyboard nowhere (⌥↑/⌥↓ from a message field or a message, a conversation
 *   chosen in the palette, a fresh load) or on the list's row that opened it (a click, Enter) gives its message field
 *   the keyboard. Where the keyboard is anywhere else it stays (the search field, a dialog, a message a link selected),
 *   and ↑/↓ in the list only move between rows, so they never open anything to take it from them.
 * - Never on a touch screen: focusing a field there raises the on-screen keyboard over the thread.
 * - Back to the list (the one-pane layout) puts the keyboard on the row of the conversation it came from.
 */

/** A row of the conversation list (sidebar/conversation-row.tsx). */
const ROW = "a[data-row][data-conversation]";
/** A layer that may still be closing when the conversation opens (the palette gives the keyboard back as it goes). */
const LAYER = "[role='dialog'], [role='alertdialog'], [role='menu'], [role='listbox']";
/** How long after opening a closing layer may still hand the keyboard back. */
const SETTLE_MS = 1500;

/** A device whose main pointer is a finger (a phone, a tablet without a mouse). */
export function touchFirst(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia("(hover: none) and (pointer: coarse)").matches;
}

/** The keyboard is nowhere (the page itself has it), or on a row of the conversation list. */
function keyboardFree(): boolean {
  const active = document.activeElement;
  return !active || active === document.body || active === document.documentElement || !!active.closest(ROW);
}

/**
 * Gives the message field the keyboard as a conversation opens, when the keyboard is free (see above); while a layer
 * that opened the conversation is still closing, it waits for the keyboard to come back. Returns the cleanup.
 */
export function takeKeyboardOnOpen(focusField: () => void): () => void {
  if (typeof document === "undefined" || touchFirst()) return () => undefined;
  const until = performance.now() + SETTLE_MS;
  let frame = 0;
  const attempt = () => {
    frame = 0;
    if (keyboardFree()) {
      focusField();
      return;
    }
    if (performance.now() < until && document.activeElement?.closest(LAYER)) frame = requestAnimationFrame(attempt);
  };
  attempt();
  return () => cancelAnimationFrame(frame);
}

/**
 * A click (or Enter) on the list's row of the conversation already open opens nothing new: it gives the message field
 * the keyboard all the same. Returns the cleanup.
 */
export function takeKeyboardFromOwnRow(address: string, focusField: () => void): () => void {
  if (typeof document === "undefined") return () => undefined;
  // (The link's own handler has prevented the click's default by now: it navigates in the page.)
  const onClick = (event: MouseEvent) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || touchFirst()) return;
    const row = event.target instanceof Element ? event.target.closest<HTMLElement>(ROW) : null;
    if (row?.dataset.conversation === address) requestAnimationFrame(focusField);
  };
  document.addEventListener("click", onClick);
  return () => document.removeEventListener("click", onClick);
}

/**
 * After Back to the list: the keyboard goes to the row of the conversation it came from, once the list is on screen
 * (the conversation's own page goes away first). Only while the keyboard is nowhere; gives up after a few seconds.
 */
export function returnKeyboardToRow(address: string): void {
  if (typeof window === "undefined") return;
  // What has the keyboard now (Back itself): it goes away with the conversation.
  const leaving = document.activeElement;
  const until = Date.now() + 3000;
  const tick = () => {
    const active = document.activeElement;
    // The keyboard went somewhere on purpose meanwhile: it stays there.
    if (active && active !== document.body && active !== document.documentElement && active !== leaving) return;
    const row = [...document.querySelectorAll<HTMLElement>(ROW)].find(element => element.dataset.conversation === address && element.offsetParent !== null);
    if (row && active !== leaving) {
      row.focus();
      return;
    }
    if (Date.now() < until) window.setTimeout(tick, 60);
  };
  window.setTimeout(tick, 0);
}
