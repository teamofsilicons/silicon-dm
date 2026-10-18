"use client";

/**
 * False while the server renders and the browser hydrates, true from the browser's own first render on. What only
 * the browser knows (the reader's time zone, this browser's drafts) waits for it, so the server's HTML and the first
 * client render always match.
 */
import { useSyncExternalStore } from "react";

const subscribeNothing = () => () => undefined;

export function useHydrated(): boolean {
  return useSyncExternalStore(subscribeNothing, () => true, () => false);
}
