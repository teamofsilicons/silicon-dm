"use client";

/**
 * The chat's three layouts (spec §6.3), from the window's width:
 *   wide    ≥ 1100 px  sidebar | thread | details panel
 *   medium  768–1099   sidebar | thread, details as a drawer
 *   narrow  < 768      one pane (list or thread), details as a bottom sheet
 * The server renders "wide"; the first client render settles it. CSS lays the panes out by itself (shell.module.css),
 * so only what has no CSS answer (which layer the details use) reads this.
 */
import { useSyncExternalStore } from "react";

export type LayoutMode = "wide" | "medium" | "narrow";

const WIDE = "(min-width: 1100px)";
const MEDIUM = "(min-width: 768px)";

function subscribe(listener: () => void) {
  const queries = [window.matchMedia(WIDE), window.matchMedia(MEDIUM)];
  for (const query of queries) query.addEventListener("change", listener);
  return () => {
    for (const query of queries) query.removeEventListener("change", listener);
  };
}

const read = (): LayoutMode => (window.matchMedia(WIDE).matches ? "wide" : window.matchMedia(MEDIUM).matches ? "medium" : "narrow");

export function useLayoutMode(): LayoutMode {
  return useSyncExternalStore(subscribe, read, () => "wide");
}

/** A media query's answer (false on the server). */
export function useMediaQuery(query: string): boolean {
  return useSyncExternalStore(
    listener => {
      const list = window.matchMedia(query);
      list.addEventListener("change", listener);
      return () => list.removeEventListener("change", listener);
    },
    () => window.matchMedia(query).matches,
    () => false,
  );
}
