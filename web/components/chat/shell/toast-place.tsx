"use client";

/**
 * Where the toast stack sits in the chat, so it never covers what you are working with.
 *
 *   sidebar on screen (768 px and wider)  in the sidebar's column, just above the account bar: the thread, its
 *                                         composer and the details panel stay clear, and so do the newest messages
 *   a phone                               across the screen, just above what is docked at the bottom: the composer
 *                                         in a conversation (following it as it grows: a reply bar, a tray of
 *                                         files, ten lines of text), the account bar in the list
 *
 * The place is CSS variables on <html> (`--toast-left`, `--toast-right`, `--toast-width`, `--toast-bottom`) that
 * components/foundation/providers.module.css reads; without them (a page with nothing docked) the stack keeps the
 * bottom right corner. What toasts stay above carries `data-toast-floor`; the composer is found by its `data-composer`.
 */
import { useEffect } from "react";

/** The space around the stack: from its column's edges, and above the floor. */
const GAP = 12;
/** The widest the stack gets (the kit's own width). */
const MAX_WIDTH = 360;
/** At least this much of the window stays above the stack's bottom edge, so a toast never leaves the screen. */
const KEEP_ON_SCREEN = 140;
const COMPOSER = "main [data-composer]";
const FLOOR = "[data-toast-floor]";

const shown = (element: Element): boolean => element.isConnected && element.getClientRects().length > 0;

export function ToastPlace() {
  useEffect(() => {
    const style = document.documentElement.style;
    let frame = 0;
    // Everything the stack sits above, watched for size: a composer that grows moves the stack up with it.
    const watched = new Set<Element>();
    const resize = new ResizeObserver(() => schedule());

    const clear = () => {
      for (const name of ["--toast-left", "--toast-right", "--toast-width", "--toast-bottom"]) style.removeProperty(name);
    };

    const place = () => {
      frame = 0;
      for (const element of watched) if (!element.isConnected) {
        resize.unobserve(element);
        watched.delete(element);
      }
      const floors = Array.from(document.querySelectorAll(FLOOR)).filter(shown);
      const inSidebar = floors.find(floor => floor.closest("[data-pane='sidebar']"));
      let left: number;
      let width: number;
      let top: number;
      if (inSidebar) {
        const column = (inSidebar.closest("[data-pane]") ?? inSidebar).getBoundingClientRect();
        width = Math.min(MAX_WIDTH, column.width - 2 * GAP);
        left = column.left + GAP;
        top = inSidebar.getBoundingClientRect().top;
      } else {
        const docks = [...floors, ...Array.from(document.querySelectorAll(COMPOSER)).filter(shown)];
        if (!docks.length) return clear();
        width = Math.min(MAX_WIDTH, window.innerWidth - 2 * GAP);
        left = (window.innerWidth - width) / 2;
        top = Math.min(...docks.map(dock => dock.getBoundingClientRect().top));
      }
      const height = window.innerHeight;
      const bottom = Math.max(GAP, Math.min(height - top + GAP, height - KEEP_ON_SCREEN));
      style.setProperty("--toast-left", `${Math.round(left)}px`);
      style.setProperty("--toast-right", "auto");
      style.setProperty("--toast-width", `${Math.round(width)}px`);
      style.setProperty("--toast-bottom", `${Math.round(bottom)}px`);
    };
    function schedule() {
      if (!frame) frame = requestAnimationFrame(place);
    }

    // A composer comes and goes with each conversation, the list's account bar with the inbox page: watch what is new.
    // (Only lookups run per change in the page; a measurement waits for a frame, and only when something moved.)
    const follow = () => {
      for (const element of Array.from(document.querySelectorAll(`${COMPOSER}, ${FLOOR}`))) {
        if (watched.has(element)) continue;
        watched.add(element);
        resize.observe(element);
        schedule();
      }
      for (const element of watched) if (!element.isConnected) schedule();
    };
    const mutations = new MutationObserver(follow);
    mutations.observe(document.body, { childList: true, subtree: true });
    follow();
    window.addEventListener("resize", schedule);
    window.visualViewport?.addEventListener("resize", schedule);
    return () => {
      cancelAnimationFrame(frame);
      mutations.disconnect();
      resize.disconnect();
      window.removeEventListener("resize", schedule);
      window.visualViewport?.removeEventListener("resize", schedule);
      clear();
    };
  }, []);
  return null;
}
