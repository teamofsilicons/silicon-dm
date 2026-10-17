"use client";

/**
 * The thread's messages (spec §6.4): a `role="log"` that announces new messages politely, day dividers, the "New
 * messages" divider at the first unread, runs of one sender, and the reading position kept carefully:
 *
 * - it opens at the first unread message (or at the bottom), or at `#<code>` when the address names a message;
 * - it stays at the bottom while you are there, and otherwise counts what arrives in a "Jump to latest" pill;
 * - older pages load as you reach the top, without moving what you are reading;
 * - a message named by a link or a reply quote is loaded if needed, scrolled to and highlighted.
 *
 * Messages from others that are on screen in a focused tab are marked read (lib/chat/receipts.ts). The keyboard moves
 * between messages (↑/↓, K/J) and acts on the selected one (R, E, C, L, Delete, I, End, Esc).
 */
import { forwardRef, useCallback, useEffect, useImperativeHandle, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import { ArrowDown } from "lucide-react";
import { motionTokens } from "@/components/silicon-ui/lib/motion-tokens";
import { isMessageCode } from "@/lib/chat/codes";
import type { OutboxItem } from "@/lib/chat/outbox";
import { noteRead } from "@/lib/chat/receipts";
import { buildTimeline, type TimelineEntry } from "@/lib/chat/timeline";
import type { AccountRef, Message } from "@/lib/chat/types";
import { useShortcut } from "@/lib/shortcuts/react";
import { MessageRow, PendingRow } from "./message-row";
import { useThreadActions } from "./thread-actions";
import styles from "./thread.module.css";

type Item = { kind: "message"; message: Message } | { kind: "pending"; item: OutboxItem };

export interface MessageListHandle {
  scrollToBottom: (smooth?: boolean) => void;
  /** Selects the last message (⌘↑ from the composer). Returns false when there is none. */
  focusLast: () => boolean;
  jumpTo: (code: string) => void;
}

export interface MessageListProps {
  address: string;
  title: string;
  messages: Message[];
  pending: OutboxItem[];
  perspective: AccountRef;
  group: boolean;
  hasOlder: boolean;
  loadingOlder: boolean;
  loadOlder: () => Promise<unknown>;
  /** The first unread message when the conversation opened, and how many came after it. */
  firstUnread: { code: string; count: number } | null;
  /** Nothing is loaded yet. */
  loading: boolean;
}

const STICK_PX = 64;
const PILL_PX = 240;
const JUMP_PAGES = 30;

const keyOf = (message: Message) => message.sender.uuid ?? message.sender.id;

export const MessageList = forwardRef<MessageListHandle, MessageListProps>(function MessageList({ address, title, messages, pending, perspective, group, hasOlder, loadingOlder, loadOlder, firstUnread, loading }, ref) {
  const actions = useThreadActions();
  const reduced = useReducedMotion();
  const scroller = useRef<HTMLDivElement>(null);
  const topSentinel = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const anchor = useRef<{ height: number; top: number } | null>(null);
  const positioned = useRef(false);
  const seenCount = useRef(0);
  const [away, setAway] = useState(false);
  const [arrived, setArrived] = useState(0);
  const [highlight, setHighlight] = useState<string | null>(null);
  const [wanted, setWanted] = useState<string | null>(null);
  const jumpPages = useRef(0);
  const perspectiveKey = perspective.uuid ?? perspective.id;

  const entries = useMemo<Array<TimelineEntry<Item>>>(() => [
    ...messages.map(message => ({ key: message["message-id"], senderKey: keyOf(message), at: message.created_at, value: { kind: "message" as const, message } })),
    ...pending.map(item => ({ key: item.key, senderKey: perspectiveKey, at: new Date(item.createdAt).toISOString(), value: { kind: "pending" as const, item } })),
  ], [messages, pending, perspectiveKey]);
  const rows = useMemo(() => buildTimeline(entries, { firstUnreadKey: firstUnread?.code ?? null, unreadCount: firstUnread?.count }), [entries, firstUnread]);
  const byCode = useMemo(() => new Map(messages.map(message => [message["message-id"], message])), [messages]);

  const scrollToBottom = useCallback((smooth = false) => {
    const node = scroller.current;
    if (!node) return;
    stick.current = true;
    node.scrollTo({ top: node.scrollHeight, behavior: smooth && !reduced ? "smooth" : "auto" });
    setArrived(0);
    setAway(false);
  }, [reduced]);

  const flash = useCallback((code: string) => {
    setHighlight(code);
    window.setTimeout(() => setHighlight(current => (current === code ? null : current)), 2600);
  }, []);

  /** Scrolls to a loaded message and highlights it; returns false when it is not loaded. */
  const reveal = useCallback((code: string) => {
    const node = scroller.current?.querySelector<HTMLElement>(`[data-message-code="${CSS.escape(code)}"]`);
    if (!node) return false;
    stick.current = false;
    node.scrollIntoView({ block: "center", behavior: reduced ? "auto" : "smooth" });
    node.focus({ preventScroll: true });
    flash(code);
    return true;
  }, [flash, reduced]);

  const jumpTo = useCallback((code: string) => {
    if (reveal(code)) return;
    jumpPages.current = 0;
    setWanted(code);
  }, [reveal]);

  // A wanted message that is not loaded yet: load older pages until it is (or there are no more).
  useEffect(() => {
    if (!wanted) return;
    if (reveal(wanted)) {
      setWanted(null);
      return;
    }
    if (!hasOlder || jumpPages.current >= JUMP_PAGES) {
      setWanted(null);
      return;
    }
    if (loadingOlder) return;
    jumpPages.current += 1;
    void loadOlder();
  }, [wanted, rows, hasOlder, loadingOlder, loadOlder, reveal]);

  const focusLast = useCallback(() => {
    const items = scroller.current?.querySelectorAll<HTMLElement>("[data-message-row]");
    const last = items?.[items.length - 1];
    if (!last) return false;
    last.focus({ preventScroll: true });
    last.scrollIntoView({ block: "nearest" });
    return true;
  }, []);

  useImperativeHandle(ref, () => ({ scrollToBottom, focusLast, jumpTo }), [scrollToBottom, focusLast, jumpTo]);

  // Where the conversation opens: the address's #message, the first unread, or the bottom.
  useLayoutEffect(() => {
    const node = scroller.current;
    if (!node || positioned.current || loading) return;
    positioned.current = true;
    seenCount.current = messages.length;
    const hash = decodeURIComponent(window.location.hash.slice(1));
    if (hash && isMessageCode(hash)) {
      jumpTo(hash);
      return;
    }
    const divider = node.querySelector<HTMLElement>("[data-unread-divider]");
    if (divider) {
      stick.current = false;
      node.scrollTop = Math.max(0, divider.offsetTop - 24);
      return;
    }
    node.scrollTop = node.scrollHeight;
  }, [loading, messages.length, jumpTo]);

  useEffect(() => {
    const onHash = () => {
      const hash = decodeURIComponent(window.location.hash.slice(1));
      if (hash && isMessageCode(hash)) jumpTo(hash);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, [jumpTo]);

  // After every change of rows: keep the reading position (older pages above), or stay at the bottom, or count.
  useLayoutEffect(() => {
    const node = scroller.current;
    if (!node || !positioned.current) return;
    if (anchor.current) {
      const grown = node.scrollHeight - anchor.current.height;
      if (grown > 0) node.scrollTop = anchor.current.top + grown;
      anchor.current = null;
      return;
    }
    if (stick.current) {
      node.scrollTop = node.scrollHeight;
      seenCount.current = messages.length;
      return;
    }
    const fresh = messages.slice(seenCount.current).filter(message => !actions.isMine(message)).length;
    seenCount.current = messages.length;
    if (fresh) setArrived(count => count + fresh);
  }, [rows, messages, actions]);

  // Your own new send always brings the thread to the bottom.
  const pendingCount = pending.length;
  const lastPending = useRef(pendingCount);
  useLayoutEffect(() => {
    if (pendingCount > lastPending.current) scrollToBottom();
    lastPending.current = pendingCount;
  }, [pendingCount, scrollToBottom]);

  const onScroll = () => {
    const node = scroller.current;
    if (!node) return;
    const distance = node.scrollHeight - node.scrollTop - node.clientHeight;
    stick.current = distance < STICK_PX;
    setAway(distance > PILL_PX);
    if (stick.current) setArrived(0);
  };

  // Older messages load before you reach the very top, keeping what you read where it is.
  useEffect(() => {
    const root = scroller.current;
    const node = topSentinel.current;
    if (!root || !node || !hasOlder) return;
    const observer = new IntersectionObserver(found => {
      if (!found.some(entry => entry.isIntersecting) || loadingOlder || !positioned.current) return;
      anchor.current = { height: root.scrollHeight, top: root.scrollTop };
      void loadOlder();
    }, { root, rootMargin: "800px 0px 0px 0px" });
    observer.observe(node);
    return () => observer.disconnect();
  }, [hasOlder, loadingOlder, loadOlder]);

  // Messages from others on screen in a focused tab are read.
  useEffect(() => {
    const root = scroller.current;
    if (!root || actions.readOnly) return;
    const observer = new IntersectionObserver(found => {
      let newest: string | null = null;
      for (const entry of found) {
        if (!entry.isIntersecting) continue;
        const code = (entry.target as HTMLElement).dataset.messageCode;
        if (code && (!newest || code.length > newest.length || (code.length === newest.length && code > newest))) newest = code;
      }
      if (newest) noteRead(address, newest);
    }, { root, threshold: 0.6 });
    for (const node of root.querySelectorAll<HTMLElement>("[data-message-code]:not([data-mine]):not([data-deleted])")) observer.observe(node);
    return () => observer.disconnect();
  }, [rows, address, actions.readOnly]);

  /* Keyboard: the selected message and its actions (message scope), and the log itself. */
  const currentRow = () => (document.activeElement instanceof HTMLElement ? document.activeElement.closest<HTMLElement>("[data-message-row]") : null);
  const currentMessage = () => {
    const code = currentRow()?.dataset.messageCode;
    return code ? byCode.get(code) ?? null : null;
  };
  const step = (by: 1 | -1) => {
    const items = Array.from(scroller.current?.querySelectorAll<HTMLElement>("[data-message-row]") ?? []);
    const at = items.indexOf(currentRow() as HTMLElement);
    const next = items[at < 0 ? items.length - 1 : Math.min(items.length - 1, Math.max(0, at + by))];
    if (!next) return;
    next.focus({ preventScroll: true });
    next.scrollIntoView({ block: "nearest" });
  };
  useShortcut("message.previous", () => step(-1));
  useShortcut("message.next", () => step(1));
  useShortcut("message.reply", () => { const message = currentMessage(); if (!message || message.deleted_at || actions.readOnly) return false; actions.reply(message); });
  useShortcut("message.edit", () => { const message = currentMessage(); if (!message || message.deleted_at || !actions.isMine(message) || actions.readOnly) return false; actions.edit(message); });
  useShortcut("message.copy", () => { const message = currentMessage(); if (!message) return false; actions.copyText(message); });
  useShortcut("message.copyLink", () => { const message = currentMessage(); if (!message) return false; actions.copyLink(message); });
  useShortcut("message.delete", () => { const message = currentMessage(); if (!message || message.deleted_at || !actions.isMine(message) || actions.readOnly) return false; actions.requestDelete(message); });
  useShortcut("message.details", () => { const message = currentMessage(); if (!message) return false; actions.openDetails(message); });
  useShortcut("message.jumpLatest", () => { scrollToBottom(true); focusLast(); });
  useShortcut("message.back", () => actions.focusComposer());

  const onLogKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.altKey || event.metaKey || event.ctrlKey) return;
    // The menu key (or Shift+F10) on a selected message opens its menu under it.
    if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
      const row = currentRow();
      const message = currentMessage();
      if (!row || !message) return;
      event.preventDefault();
      const rect = row.querySelector("[data-sq]")?.getBoundingClientRect() ?? row.getBoundingClientRect();
      actions.openMenu(message, { x: rect.left + 12, y: rect.bottom + 4 });
      return;
    }
    // On the log itself, ↑ or ↓ selects the last message (or the first with Home).
    if (event.target === event.currentTarget && (event.key === "ArrowUp" || event.key === "ArrowDown" || event.key === "End")) {
      event.preventDefault();
      focusLast();
    } else if (event.target === event.currentTarget && event.key === "Home") {
      event.preventDefault();
      scroller.current?.querySelector<HTMLElement>("[data-message-row]")?.focus();
    }
  };

  const empty = !loading && !messages.length && !pending.length;

  return (
    <div className={styles.listFrame}>
      <div ref={scroller} className={styles.scroller} onScroll={onScroll}>
        <div className={styles.column}>
          {hasOlder ? (
            <div ref={topSentinel} className={styles.older} aria-live="polite">
              {loadingOlder ? "Loading earlier messages…" : (
                <button type="button" className={styles.olderButton} onClick={() => {
                  const root = scroller.current;
                  if (root) anchor.current = { height: root.scrollHeight, top: root.scrollTop };
                  void loadOlder();
                }}>Load earlier messages</button>
              )}
            </div>
          ) : !loading && messages.length ? (
            <p className={styles.beginning}>This is the beginning of {group ? title : `your conversation with ${title}`}.</p>
          ) : null}
          <div role="log" aria-live="polite" aria-relevant="additions" aria-label={`Messages, ${title}`} className={styles.log} tabIndex={0} onKeyDown={onLogKeyDown}>
            {loading ? (
              <div className={styles.loadingRows} aria-hidden="true">
                {[62, 38, 74, 46, 58].map((width, index) => <div key={index} className={styles.loadingRow} data-mine={index % 2 ? "" : undefined}><span data-sq="surface" style={{ width: `${width}%` }} /></div>)}
              </div>
            ) : null}
            {rows.map(row => {
              if (row.type === "day") {
                return <div key={row.key} className={styles.day} role="separator" aria-label={row.label}><span>{row.label}</span></div>;
              }
              if (row.type === "unread") {
                return <div key={row.key} className={styles.unread} role="separator" aria-label="New messages" data-unread-divider=""><span>New messages</span></div>;
              }
              const value = row.entry.value;
              if (value.kind === "pending") return <PendingRow key={row.key} item={value.item} first={row.first} last={row.last} group={group} />;
              return <MessageRow key={row.key} message={value.message} first={row.first} last={row.last} group={group} highlighted={highlight === row.key} />;
            })}
          </div>
          {empty ? (
            <div className={styles.emptyThread}>
              <p className={styles.emptyTitle}>No messages yet</p>
              <p className={styles.emptyText}>{actions.readOnly ? "Nothing has been said here yet." : "Say hello. Your message is kept even if you go offline: it sends as soon as it can."}</p>
            </div>
          ) : null}
        </div>
      </div>
      <AnimatePresence>
        {away || arrived ? (
          <motion.button
            key="jump"
            type="button"
            className={styles.jump}
            data-sq="surface"
            onClick={() => scrollToBottom(true)}
            initial={reduced ? { opacity: 0 } : { opacity: 0, y: 10 }}
            animate={{ opacity: 1, y: 0 }}
            exit={reduced ? { opacity: 0 } : { opacity: 0, y: 10 }}
            transition={reduced ? { duration: 0.12 } : motionTokens.spring.snappy}
            aria-keyshortcuts="End"
          >
            <ArrowDown size={14} strokeWidth={2} aria-hidden="true" />
            <span>Jump to latest</span>
            {arrived ? <span className={styles.jumpCount}>{arrived} new</span> : null}
          </motion.button>
        ) : null}
      </AnimatePresence>
    </div>
  );
});
