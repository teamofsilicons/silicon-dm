"use client";

/**
 * Brings a form's error into view when it appears. A refusal added at the end of a long form (New group on a phone, Add
 * members in the bottom sheet) lands below the fold, and the Carbon who pressed the button would see nothing happen.
 * Screen readers already hear it (the error is role="alert"); this is for eyes. Nothing moves when it is already in
 * view, and under reduced motion the scroll is instant.
 *
 *   const errorBox = useShowWhenSet<HTMLDivElement>(error);
 *   {error ? <div ref={errorBox}><ErrorAlert error={error} /></div> : null}
 */
import { useEffect, useRef } from "react";

/** The nearest ancestor that scrolls vertically, or null when only the page would. */
function scrollParent(node: HTMLElement): HTMLElement | null {
  for (let at = node.parentElement; at && at !== document.body; at = at.parentElement) {
    if (/(auto|scroll)/.test(getComputedStyle(at).overflowY) && at.scrollHeight > at.clientHeight) return at;
  }
  return null;
}

export function useShowWhenSet<T extends HTMLElement>(value: unknown) {
  const ref = useRef<T>(null);
  useEffect(() => {
    if (!value) return;
    const frame = requestAnimationFrame(() => {
      const node = ref.current;
      if (!node) return;
      const box = node.getBoundingClientRect();
      const parent = scrollParent(node);
      const view = parent ? parent.getBoundingClientRect() : { top: 0, bottom: window.innerHeight };
      if (box.top >= view.top && box.bottom <= view.bottom) return;
      const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      // Centred, so what follows it (the button that was pressed) shows too when there is room.
      node.scrollIntoView({ block: "center", behavior: reduced ? "auto" : "smooth" });
    });
    return () => cancelAnimationFrame(frame);
  }, [value]);
  return ref;
}
