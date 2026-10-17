"use client";

/**
 * The thread's messages (spec §6.4): a `role="log"` that announces new messages politely, day dividers, the "New
 * messages" divider at the first unread, runs of one sender, the typing line at the end, and the reading position
 * kept carefully:
 *
 * - it opens at the address's `#<code>`, else at the first unread message, else at the bottom;
 * - it stays at the bottom while you are there; otherwise "Jump to latest" counts what you have not seen below;
 * - older messages come in above as you near the top (first from the cache, a window at a time, then page by page
 *   from the service) without moving what you read: the first message on screen is the anchor, and every change of
 *   size (new rows, a photo loading, a bubble drawn for the first time) moves the thread by exactly its shift; they are
 *   not read out (only new messages from others are);
 * - a message named by a link or a reply quote is loaded if needed, scrolled to, selected and highlighted; one that no
 *   older page can bring (newer than the thread, bundled away, not in this conversation) is said at once.
 *
 * Speed: rows are memoized on what they show, the actions they share never change, the message in use is a store
 * two rows watch, the log is block flow with layout-contained rows, bubbles off screen are not drawn
 * (`content-visibility`), and a long cached thread renders its newest rows first and the rest as you scroll up.
 * Messages from others that are on screen in a focused tab are read (lib/chat/receipts.ts). The keyboard moves between
 * messages (↑/↓, K/J, Home, End) and acts on the selected one (R, E, C, L, Space, Delete, I, Esc).
 */
import { forwardRef, useCallback, useEffect, useImperativeHandle, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type FocusEvent, type KeyboardEvent, type MouseEvent, type PointerEvent, type ReactNode } from "react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import { ArrowDown } from "lucide-react";
import { motionTokens } from "@/components/silicon-ui/lib/motion-tokens";
import { compareCodes, isAfter, isMessageCode } from "@/lib/chat/codes";
import { canEditHere } from "@/lib/chat/compose";
import type { OutboxItem } from "@/lib/chat/outbox";
import { noteRead } from "@/lib/chat/receipts";
import { buildTimeline, type TimelineEntry, type TimelineRow } from "@/lib/chat/timeline";
import type { AccountRef, Message } from "@/lib/chat/types";
import { notify } from "@/lib/notify";
import { useShortcut } from "@/lib/shortcuts/react";
import { canListen, useVoiceAvailability } from "@/lib/voice";
import { AccountAvatar } from "../common/avatars";
import { MessageRow, PendingRow } from "./message-row";
import { arrivedSends, busyWords, locateCode, unreachableWords, unseenCount, type Busy, type FirstUnread } from "./thread-model";
import { useThreadActions, valueStore, type InUse } from "./thread-actions";
import styles from "./thread.module.css";

type Item = { kind: "message"; message: Message } | { kind: "pending"; item: OutboxItem };
type Row = TimelineRow<Item>;

export interface MessageListHandle {
  scrollToBottom: () => void;
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
  /** The first unread message when the conversation opened, and how many there were. */
  firstUnread: FirstUnread | null;
  /** Nothing to place yet (the first page, or the pages back to the first unread, are still loading). */
  loading: boolean;
  /** Who is busy right now (typing, recording…): the line at the end of the thread. */
  busy: Busy[];
  /** The message whose delete question shows. */
  deleting: string | null;
  /** What the beginning of the thread shows (and an empty conversation). */
  beginning: ReactNode;
}

/** Within this distance of the bottom counts as at the bottom: new messages keep it there. */
const STICK_PX = 48;
/** Further than this from the bottom shows "Jump to latest". */
const PILL_PX = 360;
/** Older messages are rendered (or loaded) this far before the top, so the top is rarely reached. */
const LOAD_PX = 1600;
/** Rows a long cached thread renders when it opens, and how many more each time you near the top. */
const WINDOW = 140;
const STEP = 120;
/** Older pages a link or a reply quote may load to find its message. */
const JUMP_PAGES = 40;

const senderKey = (message: Message) => message.sender.uuid ?? message.sender.id;
const NO_GHOSTS: OutboxItem[] = [];

let hoverQuery: MediaQueryList | null = null;
/** Whether this device's main pointer hovers (a mouse or a trackpad, not a finger). */
const devicesHover = () => (hoverQuery ??= window.matchMedia("(hover: hover)")).matches;
let onePaneQuery: MediaQueryList | null = null;
/** The one-pane layout (a phone): messages are tapped, not hovered (the shell's breakpoint, shell.module.css). */
const onePane = () => (onePaneQuery ??= window.matchMedia("(max-width: 767px)")).matches;
/** A message's own controls: tapping one uses it, and selects nothing. */
const CONTROLS = "a[href], button, input, textarea, select, audio, video, canvas, pre, [role='slider'], [role='button'], [contenteditable]";
/** Layers opened from a message (its menu, its details, the lightbox): the message stays selected while they are open. */
const LAYERS = "[role='menu'], [role='dialog'], [role='alertdialog'], [data-radix-popper-content-wrapper]";

/**
 * How the Carbon acted last: with the keyboard, or with a pointer of some kind. Focus that follows decides whether a
 * message is selected (a browser does not always show focus for keys with a modifier, such as ⌘↑, so :focus-visible
 * cannot tell).
 */
const lastInput = { keyboard: false, pointerType: "" };
let trackingInput = false;
function trackInput(): void {
  if (trackingInput || typeof document === "undefined") return;
  trackingInput = true;
  document.addEventListener("keydown", () => {
    lastInput.keyboard = true;
  }, true);
  document.addEventListener("pointerdown", event => {
    lastInput.keyboard = false;
    lastInput.pointerType = event.pointerType;
  }, true);
}

/** The first row a long thread renders when it opens: enough for the newest messages, the first unread and a link's target. */
function initialWindow(rows: Row[], target: string | null): string | null {
  if (rows.length <= WINDOW) return null;
  let start = rows.length - WINDOW;
  const unread = rows.findIndex(row => row.type === "unread");
  if (unread >= 0) start = Math.min(start, Math.max(0, unread - 12));
  if (target) {
    const at = rows.findIndex(row => row.key === target);
    if (at >= 0) start = Math.min(start, Math.max(0, at - 12));
  }
  return start > 0 ? rows[start]!.key : null;
}

function hashTarget(): string | null {
  if (typeof window === "undefined") return null;
  const hash = decodeURIComponent(window.location.hash.slice(1));
  return hash && isMessageCode(hash) ? hash : null;
}

/* The scroll position as the pill needs it, outside React state so scrolling never re-renders the list. ---------- */

interface ViewState {
  atBottom: boolean;
  away: boolean;
  /** The newest message from others that was on screen ("" before anything was). */
  newestSeen: string;
}

function viewStore() {
  let state: ViewState = { atBottom: true, away: false, newestSeen: "" };
  const listeners = new Set<() => void>();
  return {
    get: () => state,
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    set(next: Partial<ViewState>) {
      const merged = { ...state, ...next };
      if (merged.atBottom === state.atBottom && merged.away === state.away && merged.newestSeen === state.newestSeen) return;
      state = merged;
      for (const listener of listeners) listener();
    },
  };
}

type ViewStore = ReturnType<typeof viewStore>;

function JumpPill({ store, messages, perspective, onJump }: { store: ViewStore; messages: Message[]; perspective: AccountRef; onJump: () => void }) {
  const reduced = useReducedMotion();
  const state = useSyncExternalStore(store.subscribe, store.get, store.get);
  const count = useMemo(() => unseenCount(messages, state.newestSeen, perspective), [messages, state.newestSeen, perspective]);
  const show = !state.atBottom && (state.away || count > 0);
  return (
    <AnimatePresence>
      {show ? (
        <motion.button
          key="jump"
          type="button"
          className={styles.jump}
          data-sq="surface"
          onClick={onJump}
          initial={reduced ? { opacity: 0 } : { opacity: 0, y: 10, scale: 0.96 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          exit={reduced ? { opacity: 0 } : { opacity: 0, y: 10, scale: 0.96 }}
          transition={reduced ? { duration: 0.12 } : motionTokens.spring.snappy}
          aria-keyshortcuts="End"
          aria-label={count ? `Jump to latest, ${count} new ${count === 1 ? "message" : "messages"}` : "Jump to latest"}
        >
          <span>Jump to latest</span>
          <ArrowDown size={14} strokeWidth={2} aria-hidden="true" />
          {count ? <span className={styles.jumpCount} data-sq="surface" aria-hidden="true">{count > 99 ? "99+" : count}</span> : null}
        </motion.button>
      ) : null}
    </AnimatePresence>
  );
}

/** The line at the end of the thread while someone is busy: three dots in a bubble and who is doing what. */
function BusyRow({ busy, group }: { busy: Busy[]; group: boolean }) {
  const reduced = useReducedMotion();
  return (
    <AnimatePresence initial={false}>
      {busy.length ? (
        <motion.div
          key="busy"
          className={styles.busy}
          data-row-key="busy"
          aria-hidden="true"
          initial={reduced ? { opacity: 0 } : { opacity: 0, y: 6 }}
          animate={{ opacity: 1, y: 0 }}
          exit={reduced ? { opacity: 0 } : { opacity: 0, y: 4, transition: { duration: 0.14 } }}
          transition={reduced ? { duration: 0.12 } : motionTokens.spring.snappy}
        >
          {group ? <span className={styles.gutter}><AccountAvatar account={busy[0]!.account as AccountRef} size="bubble" decorative /></span> : null}
          <span className={styles.busyBubble} data-sq="surface"><span className={styles.dots}><i /><i /><i /></span></span>
          <span className={styles.busyWords}>{busyWords(busy)}</span>
        </motion.div>
      ) : null}
    </AnimatePresence>
  );
}

export const MessageList = forwardRef<MessageListHandle, MessageListProps>(function MessageList(props, ref) {
  const { address, title, messages, pending, perspective, group, hasOlder, loadingOlder, loadOlder, firstUnread, loading, busy, deleting, beginning } = props;
  const actions = useThreadActions();
  const voice = useVoiceAvailability();
  const scroller = useRef<HTMLDivElement>(null);
  const log = useRef<HTMLDivElement>(null);
  const store = useMemo(() => viewStore(), []);
  const inUse = useMemo(() => valueStore<InUse | null>(null), []);
  const stick = useRef(true);
  /** Where the last scroll event left the thread: only a move up by the reader lets go of the bottom. */
  const lastTop = useRef(0);
  /** The first message on screen and where it was in the thread (offsetTop) when the position was last read. */
  const anchor = useRef<{ key: string; top: number } | null>(null);
  const positioned = useRef(false);
  const requested = useRef(false);
  const wanted = useRef<string | null>(null);
  const jumpPages = useRef(0);
  const focusAfter = useRef<string | null>(null);
  const [highlight, setHighlight] = useState<string | null>(null);
  const perspectiveKey = perspective.uuid ?? perspective.id;

  /*
   * A send is on screen from the moment it is written until the service's copy is: the outbox and the message cache
   * update separately (either can be first, and the sync feed can bring the message before the send's own answer), so
   * a send that left the outbox stays as a "ghost" until its message is in the thread, and a send whose message is
   * already there is not shown twice (thread-model.ts arrivedSends). No gap, no double, no jump.
   */
  const newestCode = messages.at(-1)?.["message-id"] ?? "";
  const [since, setSince] = useState<Record<string, string>>({});
  const unseen = pending.filter(item => !(item.key in since));
  if (unseen.length) setSince(current => ({ ...current, ...Object.fromEntries(unseen.map(item => [item.key, newestCode])) }));
  const [ghosts, setGhosts] = useState<OutboxItem[]>(NO_GHOSTS);
  const [seenPending, setSeenPending] = useState(pending);
  if (pending !== seenPending) {
    const gone = seenPending.filter(item => item.status !== "failed" && !pending.some(next => next.key === item.key));
    setSeenPending(pending);
    if (gone.length) setGhosts(current => [...current, ...gone]);
  }
  const arrived = useMemo(() => arrivedSends([...pending, ...ghosts], messages, since, perspective), [pending, ghosts, messages, since, perspective]);
  if (ghosts.length && ghosts.some(ghost => arrived.has(ghost.key))) setGhosts(current => current.filter(ghost => !arrived.has(ghost.key)));
  useEffect(() => {
    if (!ghosts.length) return;
    // A send another tab finished, or one whose words the service changed: never kept longer than this.
    const timer = window.setTimeout(() => setGhosts(NO_GHOSTS), 4000);
    return () => window.clearTimeout(timer);
  }, [ghosts]);
  const shownPending = useMemo(() => {
    const all = ghosts.length ? [...pending, ...ghosts.filter(ghost => !pending.some(item => item.key === ghost.key))] : pending;
    return arrived.size ? all.filter(item => !arrived.has(item.key)) : all;
  }, [pending, ghosts, arrived]);

  /* Rows ------------------------------------------------------------------------------------------------------------ */
  const entries = useMemo<Array<TimelineEntry<Item>>>(() => [
    ...messages.map(message => ({ key: message["message-id"], senderKey: senderKey(message), at: message.created_at, value: { kind: "message" as const, message } })),
    ...shownPending.map(item => ({ key: item.key, senderKey: perspectiveKey, at: new Date(item.createdAt).toISOString(), value: { kind: "pending" as const, item } })),
  ], [messages, shownPending, perspectiveKey]);
  const rows = useMemo(() => buildTimeline(entries, { firstUnreadKey: firstUnread?.code ?? null, unreadCount: firstUnread?.count }), [entries, firstUnread]);
  const byCode = useMemo(() => new Map(messages.map(message => [message["message-id"], message])), [messages]);
  // Per message: the day its divider names (its accessible name says it), and whether a send still in the outbox comes
  // right after it in the run (then it shows its own time and state: the run's last line is the Waiting send's).
  const rowFacts = useMemo(() => {
    const days = new Map<string, string>();
    const beforePending = new Set<string>();
    let day = "";
    rows.forEach((row, index) => {
      if (row.type === "day") day = row.label;
      if (row.type !== "entry" || row.entry.value.kind !== "message") return;
      days.set(row.key, day);
      const next = rows[index + 1];
      if (!row.last && next?.type === "entry" && next.entry.value.kind === "pending") beforePending.add(row.key);
    });
    return { days, beforePending };
  }, [rows]);

  // A long thread from the cache renders its newest rows first (undefined: not decided until there is something).
  const [windowStart, setWindowStart] = useState<string | null | undefined>(undefined);
  if (windowStart === undefined && !loading) setWindowStart(initialWindow(rows, hashTarget()));
  const foundStart = windowStart ? rows.findIndex(row => row.key === windowStart) : 0;
  const startIndex = Math.max(0, foundStart);
  const shown = startIndex ? rows.slice(startIndex) : rows;
  // The newest message when the thread opened: what comes after it rises in.
  const [openedNewest, setOpenedNewest] = useState<string | null>(null);
  if (!loading && openedNewest === null) setOpenedNewest(messages.at(-1)?.["message-id"] ?? "");

  // The loaded range of codes (where a linked message can be: thread-model.ts locateCode).
  const loaded = useMemo(() => ({ oldest: messages[0]?.["message-id"] ?? null, newest: messages.at(-1)?.["message-id"] ?? null, has: (code: string) => byCode.has(code) }), [messages, byCode]);

  // What handlers need to read without being re-made on every render.
  const latest = useRef({ rows, startIndex, hasOlder, loadingOlder, loadOlder, loaded, readOnly: actions.readOnly });
  useLayoutEffect(() => {
    latest.current = { rows, startIndex, hasOlder, loadingOlder, loadOlder, loaded, readOnly: actions.readOnly };
  });

  /* The reading position -------------------------------------------------------------------------------------------- */

  /** Remembers the first message on screen and where it is in the thread, so any change of size above it can be undone. */
  const captureAnchor = useCallback(() => {
    const node = scroller.current;
    const list = log.current;
    if (!node || !list) return;
    const children = list.children;
    const top = node.scrollTop;
    let low = 0;
    let high = children.length - 1;
    let found = -1;
    while (low <= high) {
      const middle = (low + high) >> 1;
      const element = children[middle] as HTMLElement;
      if (element.offsetTop + element.offsetHeight > top) {
        found = middle;
        high = middle - 1;
      } else low = middle + 1;
    }
    // Only a message is an anchor: a day's divider moves up when earlier messages of that day come in above it.
    let element = found >= 0 ? (children[found] as HTMLElement) : null;
    while (element && !element.matches("[data-message-row]")) element = element.nextElementSibling as HTMLElement | null;
    anchor.current = element ? { key: element.dataset.rowKey!, top: element.offsetTop } : null;
  }, []);

  /**
   * Undoes what moved the anchor since it was read: rows added or resized above it (older pages, a photo, a bubble
   * drawn for the first time) shift it down by some amount, and the thread scrolls by the same amount. Only the
   * anchor's own move counts, so a scroll by the reader in between is kept. The browser's own scroll anchoring is off
   * for the thread (it would add the same shift a second time); this works the same in every browser.
   */
  const keepAnchor = useCallback(() => {
    const node = scroller.current;
    const kept = anchor.current;
    if (!node || !kept) return;
    const element = log.current?.querySelector<HTMLElement>(`[data-row-key="${CSS.escape(kept.key)}"]`);
    if (!element) return;
    const shift = element.offsetTop - kept.top;
    kept.top = element.offsetTop;
    if (Math.abs(shift) >= 1) {
      node.scrollTop += shift;
      lastTop.current = node.scrollTop;
    }
  }, []);

  const readPosition = useCallback(() => {
    const node = scroller.current;
    if (!node) return;
    const distance = node.scrollHeight - node.scrollTop - node.clientHeight;
    stick.current = distance <= STICK_PX;
    store.set({ atBottom: stick.current, away: distance > PILL_PX });
  }, [store]);

  /** After anything changed: the bottom stays the bottom; otherwise what you were reading stays where it was. */
  const restore = useCallback(() => {
    const node = scroller.current;
    if (!node || !positioned.current) return;
    if (stick.current) {
      if (node.scrollTop + node.clientHeight < node.scrollHeight - 1) node.scrollTop = node.scrollHeight;
      lastTop.current = node.scrollTop;
      return;
    }
    keepAnchor();
  }, [keepAnchor]);

  /** Near the top: render more of the cached rows, or ask the service for the page before. */
  const maybeLoadOlder = useCallback(() => {
    const node = scroller.current;
    if (!node || !positioned.current || node.scrollTop > LOAD_PX) return;
    const now = latest.current;
    if (now.startIndex > 0) {
      const next = Math.max(0, now.startIndex - STEP);
      setWindowStart(next > 0 ? now.rows[next]!.key : null);
      return;
    }
    if (!now.hasOlder || now.loadingOlder || requested.current) return;
    requested.current = true;
    void now.loadOlder().finally(() => {
      requested.current = false;
    });
  }, []);

  const onScroll = useCallback(() => {
    const node = scroller.current;
    if (!node) return;
    // A scroll can arrive after the thread changed size and before the size observer ran: settle that first. At the
    // bottom, more content (a photo, a voice bubble) keeps the bottom unless the reader moved up.
    const movedUp = node.scrollTop < lastTop.current - 1;
    if (positioned.current) {
      if (stick.current && !movedUp) restore();
      else keepAnchor();
    }
    lastTop.current = node.scrollTop;
    captureAnchor();
    readPosition();
    maybeLoadOlder();
  }, [restore, keepAnchor, captureAnchor, readPosition, maybeLoadOlder]);

  // Every change of size inside the thread or of the thread's own height (a growing composer, a rotated phone).
  useEffect(() => {
    const node = scroller.current;
    const list = log.current;
    if (!node || !list) return;
    const observer = new ResizeObserver(() => {
      restore();
      readPosition();
    });
    observer.observe(list);
    observer.observe(node);
    return () => observer.disconnect();
  }, [restore, readPosition]);

  const scrollToBottom = useCallback(() => {
    const node = scroller.current;
    if (!node) return;
    if (latest.current.startIndex > 0 && latest.current.rows.length > WINDOW) setWindowStart(initialWindow(latest.current.rows, null));
    stick.current = true;
    node.scrollTop = node.scrollHeight;
    captureAnchor();
    readPosition();
  }, [captureAnchor, readPosition]);

  const flash = useCallback((code: string) => {
    setHighlight(code);
    window.setTimeout(() => setHighlight(current => (current === code ? null : current)), 2600);
  }, []);

  /** Scrolls a rendered message into the middle, selects and highlights it; false when it is not rendered. */
  const reveal = useCallback((code: string) => {
    const element = log.current?.querySelector<HTMLElement>(`[data-message-code="${CSS.escape(code)}"]`);
    if (!element) return false;
    stick.current = false;
    element.scrollIntoView({ block: "center" });
    element.focus({ preventScroll: true });
    flash(code);
    captureAnchor();
    readPosition();
    return true;
  }, [flash, captureAnchor, readPosition]);

  /** Finds a wanted message: reveals it once rendered, widens the window to it, or loads the page before. */
  const seek = useCallback(() => {
    const code = wanted.current;
    if (!code || !positioned.current) return;
    if (reveal(code)) {
      wanted.current = null;
      return;
    }
    const now = latest.current;
    const at = now.rows.findIndex(row => row.key === code);
    if (at >= 0) {
      if (at < now.startIndex) setWindowStart(at > 12 ? now.rows[at - 12]!.key : null);
      return;
    }
    // Only a message older than everything loaded can come with an older page; anything else is said at once.
    const place = locateCode(code, now.loaded, now.hasOlder);
    if (place !== "older" || jumpPages.current >= JUMP_PAGES) {
      wanted.current = null;
      const words = unreachableWords(code, place === "older" ? "too-far" : place === "here" ? "missing" : place);
      notify.info(words.title, words.hint);
      return;
    }
    if (now.loadingOlder || requested.current) return;
    jumpPages.current += 1;
    requested.current = true;
    void now.loadOlder().finally(() => {
      requested.current = false;
    });
  }, [reveal]);

  const jumpTo = useCallback((code: string) => {
    wanted.current = code;
    jumpPages.current = 0;
    seek();
  }, [seek]);

  const focusRow = useCallback((element: HTMLElement | null | undefined) => {
    if (!element) return false;
    element.focus({ preventScroll: true, focusVisible: true } as FocusOptions);
    element.scrollIntoView({ block: "nearest" });
    return true;
  }, []);

  const focusLast = useCallback(() => {
    const items = log.current?.querySelectorAll<HTMLElement>("[data-message-row]");
    return focusRow(items?.[items.length - 1]);
  }, [focusRow]);

  useImperativeHandle(ref, () => ({ scrollToBottom, focusLast, jumpTo }), [scrollToBottom, focusLast, jumpTo]);

  // Where the conversation opens: the address's #message, the first unread, or the bottom.
  useLayoutEffect(() => {
    const node = scroller.current;
    if (!node || positioned.current || loading || windowStart === undefined) return;
    positioned.current = true;
    const target = hashTarget();
    const divider = log.current?.querySelector<HTMLElement>("[data-unread-divider]");
    if (target) {
      node.scrollTop = node.scrollHeight;
      jumpTo(target);
    } else if (divider) {
      node.scrollTop = Math.max(0, divider.offsetTop - 16);
    } else {
      node.scrollTop = node.scrollHeight;
    }
    // What was on screen is seen; the first unread and everything after it is not, until it comes on screen.
    const before = firstUnread ? messages.filter(message => compareCodes(message["message-id"], firstUnread.code) < 0).at(-1)?.["message-id"] ?? "" : messages.at(-1)?.["message-id"] ?? "";
    store.set({ newestSeen: before });
    captureAnchor();
    readPosition();
    maybeLoadOlder();
  }, [loading, windowStart, firstUnread, messages, store, jumpTo, captureAnchor, readPosition, maybeLoadOlder]);

  useEffect(() => {
    const onHash = () => {
      const target = hashTarget();
      if (target) jumpTo(target);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, [jumpTo]);

  // Your own new send brings the thread to the bottom.
  const pendingCount = pending.length;
  const lastPendingCount = useRef(pendingCount);
  useLayoutEffect(() => {
    if (pendingCount > lastPendingCount.current) scrollToBottom();
    lastPendingCount.current = pendingCount;
  }, [pendingCount, scrollToBottom]);

  // After every render: keep the reading position, look for a wanted message, keep loading while near the top, and
  // finish a step up that needed more rows.
  useLayoutEffect(() => {
    if (!positioned.current) return;
    restore();
    if (wanted.current) seek();
    else maybeLoadOlder();
    const after = focusAfter.current;
    if (after) {
      let candidate = log.current?.querySelector<HTMLElement>(`[data-row-key="${CSS.escape(after)}"]`)?.previousElementSibling as HTMLElement | null | undefined;
      while (candidate && !candidate.matches("[data-message-row]")) candidate = candidate.previousElementSibling as HTMLElement | null;
      if (candidate) {
        focusAfter.current = null;
        focusRow(candidate);
      }
    }
  });

  /* Read and seen --------------------------------------------------------------------------------------------------- */
  const observer = useRef<IntersectionObserver | null>(null);
  const observed = useRef(new WeakSet<Element>());
  useLayoutEffect(() => {
    const root = scroller.current;
    const list = log.current;
    if (!root || !list) return;
    if (!observer.current) {
      observer.current = new IntersectionObserver(found => {
        let newest: string | null = null;
        for (const entry of found) {
          if (!entry.isIntersecting) continue;
          const visible = entry.intersectionRatio >= 0.6 || entry.intersectionRect.height >= (entry.rootBounds?.height ?? Infinity) * 0.5;
          const code = (entry.target as HTMLElement).dataset.messageCode;
          if (visible && code && isAfter(code, newest)) newest = code;
        }
        if (!newest) return;
        if (!latest.current.readOnly) noteRead(address, newest);
        if (isAfter(newest, store.get().newestSeen)) store.set({ newestSeen: newest });
      }, { root, threshold: [0.1, 0.3, 0.6] });
    }
    for (const element of list.querySelectorAll<HTMLElement>("article[data-message-code]:not([data-mine]):not([data-deleted])")) {
      if (observed.current.has(element)) continue;
      observed.current.add(element);
      observer.current.observe(element);
    }
  });
  useEffect(() => () => {
    observer.current?.disconnect();
    // A remount (React's development double effects) makes a new observer and observes every row again.
    observer.current = null;
    observed.current = new WeakSet();
  }, []);

  /* The keyboard: the selected message and its actions (message scope), and the log itself. ----------------------- */
  const currentRow = () => (document.activeElement instanceof HTMLElement ? document.activeElement.closest<HTMLElement>("[data-message-row]") : null);
  const currentMessage = () => {
    const code = currentRow()?.dataset.messageCode;
    return code ? byCode.get(code) ?? null : null;
  };
  const step = (by: 1 | -1) => {
    const items = Array.from(log.current?.querySelectorAll<HTMLElement>("[data-message-row]") ?? []);
    const row = currentRow();
    const at = row ? items.indexOf(row) : -1;
    if (at === 0 && by === -1) {
      // The first rendered message: bring in the ones before it, then move there.
      if (latest.current.startIndex > 0 || latest.current.hasOlder) {
        focusAfter.current = row!.dataset.rowKey ?? null;
        const now = latest.current;
        if (now.startIndex > 0) setWindowStart(Math.max(0, now.startIndex - STEP) > 0 ? now.rows[Math.max(0, now.startIndex - STEP)]!.key : null);
        else if (!now.loadingOlder && !requested.current) {
          requested.current = true;
          void now.loadOlder().finally(() => {
            requested.current = false;
          });
        }
      }
      return;
    }
    focusRow(items[at < 0 ? items.length - 1 : Math.min(items.length - 1, Math.max(0, at + by))]);
  };
  const usable = (message: Message | null) => !!message && !message.deleted_at && !actions.readOnly;
  useShortcut("message.previous", () => step(-1));
  useShortcut("message.next", () => step(1));
  // A message key that does not apply to the selected message (Edit on someone else's, Space on a Carbon's words) does
  // nothing: it is not typed into the composer, and focus stays on the message.
  useShortcut("message.reply", () => { const message = currentMessage(); if (usable(message)) actions.reply(message!); });
  useShortcut("message.edit", () => { const message = currentMessage(); if (usable(message) && actions.isMine(message!) && canEditHere(message!)) actions.edit(message!); });
  useShortcut("message.copy", () => { const message = currentMessage(); if (message && !message.deleted_at) actions.copyText(message); });
  useShortcut("message.copyLink", () => { const message = currentMessage(); if (message) actions.copyLink(message); });
  useShortcut("message.delete", () => { const message = currentMessage(); if (usable(message) && actions.isMine(message!)) actions.requestDelete(message!); });
  useShortcut("message.details", () => { const message = currentMessage(); if (message) actions.openDetails(message); });
  useShortcut("message.listen", () => { const message = currentMessage(); if (message && voice.listen && canListen(message)) actions.listen(message); });
  useShortcut("message.jumpLatest", () => { scrollToBottom(); focusLast(); });
  // Esc answers an open delete question first ("keep it"), then goes back to the composer.
  useShortcut("message.back", () => (deleting ? actions.cancelDelete() : actions.focusComposer()));

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
    if (event.target !== event.currentTarget) {
      // Home on a selected message goes to the first rendered one.
      if (event.key === "Home" && (event.target as HTMLElement).matches("[data-message-row]")) {
        event.preventDefault();
        focusRow(log.current?.querySelector<HTMLElement>("[data-message-row]"));
      }
      return;
    }
    // On the log itself, ↑, ↓ or End select the last message, Home the first.
    if (event.key === "ArrowUp" || event.key === "ArrowDown" || event.key === "End" || event.key === "k" || event.key === "j") {
      event.preventDefault();
      focusLast();
    } else if (event.key === "Home") {
      event.preventDefault();
      focusRow(log.current?.querySelector<HTMLElement>("[data-message-row]"));
    }
  };

  /*
   * The message in use, the one with the toolbar: the message you selected, else the one the pointer rests on. Never two
   * (focus wins over hover). Selected means reached with the keyboard, or tapped (a finger, or any pointer in the
   * one-pane layout, where nothing is hovered); a mouse click on a wide screen only rests the pointer there. Tapping one
   * of a message's own controls (play, a photo, a link) uses that control and selects nothing. A selection lasts while
   * the keyboard is on that message or in a layer opened from it (its menu, its details); pressing anywhere else, or
   * the keyboard going anywhere else, lets it go.
   */
  const hovered = useRef<string | null>(null);
  const selected = useRef<string | null>(null);
  const publishInUse = useCallback(() => {
    const key = selected.current ?? hovered.current;
    const next = key ? { key, selected: selected.current !== null } : null;
    const now = inUse.get();
    if (now?.key !== next?.key || now?.selected !== next?.selected) inUse.set(next);
  }, [inUse]);
  const select = useCallback((key: string | null) => {
    if (selected.current === key) return;
    selected.current = key;
    publishInUse();
  }, [publishInUse]);
  const tapped = () => !lastInput.keyboard && (lastInput.pointerType !== "mouse" || onePane());

  const onPointerOver = (event: PointerEvent<HTMLDivElement>) => {
    // Only a pointer that hovers puts a message in use by resting on it. A phone makes up mouse events after a tap or a
    // scroll, and a toolbar opened by them would cover what the next tap was meant for; there, a tap selects instead.
    if (event.pointerType !== "mouse" || !devicesHover() || onePane()) return;
    const row = (event.target as Element).closest?.<HTMLElement>("[data-message-row]");
    hovered.current = row?.dataset.rowKey ?? null;
    publishInUse();
  };
  const onPointerLeave = () => {
    hovered.current = null;
    publishInUse();
  };
  const onFocusIn = (event: FocusEvent<HTMLDivElement>) => {
    const target = event.target as HTMLElement;
    const row = target.closest<HTMLElement>("[data-message-row]");
    select(row && (lastInput.keyboard || (tapped() && target === row)) ? row.dataset.rowKey ?? null : null);
  };
  // A tap on a message's words selects it, even where the browser does not focus what is tapped.
  const onClick = (event: MouseEvent<HTMLDivElement>) => {
    if (!tapped()) return;
    const target = event.target as HTMLElement;
    const row = target.closest<HTMLElement>("[data-message-row]");
    if (!row || target.closest(CONTROLS) || window.getSelection()?.toString().trim()) return;
    if (document.activeElement === row) select(row.dataset.rowKey ?? null);
    else row.focus({ preventScroll: true });
  };
  useEffect(() => {
    trackInput();
    const outside = (target: EventTarget | null) => {
      const element = target instanceof Element ? target : null;
      const key = selected.current;
      if (!key || element?.closest(LAYERS)) return false;
      return !element?.closest(`[data-row-key="${CSS.escape(key)}"]`);
    };
    // The keyboard went elsewhere (the composer, the sidebar), or a press landed outside the selected message.
    const onFocus = (event: globalThis.FocusEvent) => {
      if (log.current?.contains(event.target as Node)) return;
      if (outside(event.target)) select(null);
    };
    const onDown = (event: globalThis.PointerEvent) => {
      if (outside(event.target)) select(null);
    };
    document.addEventListener("focusin", onFocus, true);
    document.addEventListener("pointerdown", onDown, true);
    return () => {
      document.removeEventListener("focusin", onFocus, true);
      document.removeEventListener("pointerdown", onDown, true);
    };
  }, [select]);

  const empty = !loading && !messages.length && !shownPending.length;
  const atBeginning = !loading && !hasOlder && startIndex === 0;

  /*
   * The log reads new messages out politely. Rows that are not new must not be read: the first page, an older page, the
   * cached rows drawn as you scroll up. The commit that puts rows above the old top has aria-live="off", and the log
   * speaks again a moment later (aria-busy alone only delays the reading). Your own rows never speak (message-row.tsx).
   */
  const topKey = shown.find(row => row.type === "entry")?.key ?? null;
  const [spokenTop, setSpokenTop] = useState<string | null>(null);
  const quiet = topKey !== spokenTop;
  useEffect(() => {
    if (!quiet) return;
    const timer = window.setTimeout(() => setSpokenTop(topKey), 400);
    return () => window.clearTimeout(timer);
  }, [quiet, topKey]);

  return (
    <div className={styles.listFrame}>
      <div ref={scroller} className={styles.scroller} onScroll={onScroll} data-empty={empty ? "" : undefined}>
        <div className={styles.column}>
          {/* The top of history sits over the log; an empty conversation shows it in the log itself (the log is then not an
              empty, invisible stop for Tab or for Esc from the composer). */}
          {atBeginning ? (empty ? null : beginning) : (
            <div className={styles.older} aria-live="polite">
              {loading ? null : loadingOlder ? "Loading earlier messages…" : (
                <button type="button" className={styles.olderButton} onClick={() => {
                  const now = latest.current;
                  if (now.startIndex > 0) setWindowStart(Math.max(0, now.startIndex - STEP) > 0 ? now.rows[Math.max(0, now.startIndex - STEP)]!.key : null);
                  else void loadOlder();
                }}>Show earlier messages</button>
              )}
            </div>
          )}
          <div
            ref={log}
            role="log"
            aria-live={quiet ? "off" : "polite"}
            aria-relevant="additions"
            aria-busy={loading || loadingOlder || undefined}
            aria-label={`Messages, ${title}`}
            className={styles.log}
            tabIndex={0}
            onKeyDown={onLogKeyDown}
            onPointerOver={onPointerOver}
            onPointerLeave={onPointerLeave}
            onFocus={onFocusIn}
            onClick={onClick}
          >
            {loading ? (
              <div className={styles.loadingRows} aria-hidden="true">
                {[52, 34, 66, 41, 58, 30].map((width, index) => <div key={index} className={styles.loadingRow} data-mine={index % 3 === 2 ? "" : undefined}><span data-sq="surface" style={{ width: `${width}%` }} /></div>)}
              </div>
            ) : null}
            {empty ? beginning : null}
            {loading ? null : shown.map(row => {
              if (row.type === "day") {
                return <div key={row.key} data-row-key={row.key} className={styles.day} role="separator" aria-label={row.label}><span>{row.label}</span></div>;
              }
              if (row.type === "unread") {
                return <div key={row.key} data-row-key={row.key} className={styles.unread} role="separator" aria-label={`New messages${row.count ? `, ${row.count}` : ""}`} data-unread-divider=""><span>New messages</span></div>;
              }
              const value = row.entry.value;
              if (value.kind === "pending") return <PendingRow key={row.key} item={value.item} first={row.first} last={row.last} group={group} fresh={openedNewest !== null} />;
              const code = value.message["message-id"];
              const fresh = openedNewest !== null && compareCodes(code, openedNewest) > 0 && !actions.isMine(value.message);
              return <MessageRow key={row.key} message={value.message} first={row.first} last={row.last} group={group} day={rowFacts.days.get(code) ?? null} stateShown={rowFacts.beforePending.has(code)} highlighted={highlight === code} confirming={deleting === code} fresh={fresh} inUse={inUse} />;
            })}
            {loading ? null : <BusyRow busy={busy} group={group} />}
          </div>
        </div>
      </div>
      <JumpPill store={store} messages={messages} perspective={perspective} onJump={() => { scrollToBottom(); log.current?.focus({ preventScroll: true }); }} />
    </div>
  );
});
