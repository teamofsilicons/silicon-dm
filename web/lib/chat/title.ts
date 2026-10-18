"use client";

/**
 * The browser tab's title: the page's own (a conversation's name, "Settings"), with the unread count in front, "(3) DM".
 * Next sets the title from page metadata on navigation; a page that knows better (a conversation's name is only known in
 * the browser) sets its own with useTitle, and the unread count is put back in front whenever the title changes.
 */
import { useEffect } from "react";
import { unreadBadge } from "./format";

let unread = 0;
let base: string | null = null;
let observer: MutationObserver | null = null;

const strip = (title: string) => title.replace(/^\(\d+\+?\)\s+/, "");

function apply() {
  if (typeof document === "undefined") return;
  const own = base ?? strip(document.title);
  // 1,000 or more is "999+" (the service stops counting there; lib/chat/format.ts).
  const next = unread > 0 ? `(${unreadBadge(unread)}) ${own}` : own;
  if (document.title !== next) document.title = next;
}

function watch() {
  if (observer || typeof MutationObserver === "undefined") return;
  observer = new MutationObserver(apply);
  observer.observe(document.head, { subtree: true, childList: true, characterData: true });
}

/** Keeps "(n) " in front of the title while mounted (the chat shell). */
export function useUnreadTitle(count: number): void {
  useEffect(() => {
    unread = count;
    watch();
    apply();
  }, [count]);
  useEffect(() => () => {
    unread = 0;
    observer?.disconnect();
    observer = null;
    apply();
  }, []);
}

/** The page's own title while mounted ("Ada Lovelace · DM"). */
export function useTitle(title: string | null): void {
  useEffect(() => {
    if (!title) return;
    base = title;
    apply();
    return () => {
      if (base === title) base = null;
    };
  }, [title]);
}
