"use client";

/**
 * Listen mode at run time (spec §6.6): while it is on for the open conversation, new messages from Silicons are spoken
 * in order (lib/voice/queue.ts decides which and when); a spoken message waits until the service has rendered it.
 * "Play spoken messages when they arrive" (Settings → Listening) runs the same queue for spoken messages only.
 *
 * - It watches the conversation's messages in the TanStack cache (the sync loop and the custodian view's polling put
 *   new ones there), so it needs no events of its own. The same watch stops whatever plays from a message that was
 *   just deleted: its words are gone for everyone, so they are not heard any more either.
 * - It holds its tongue while the Carbon types in the composer or records (the current message pauses, and resumes
 *   two and a half seconds after the last keystroke or when the recording ends).
 * - Turning it on is a click or ⌘⇧L: that gesture unlocks audio for what it speaks later.
 * - Only the open conversation speaks; leaving it stops whatever plays from it. Whether Listen mode is on is
 *   remembered for each conversation while the tab is open, and Settings → Listening can turn it on by default.
 * - One tab speaks. With the conversation open in two tabs of this browser (Listen mode on by default does that), each
 *   new message would be heard twice; the tab holding the Web Lock `dm-listen:<address>` speaks and the others wait in
 *   line, taking over when it closes, leaves the conversation or stops listening. Turning Listen mode on in a tab is a
 *   choice of that tab: it takes the lock over, and the tab that had it says so ("On in another tab", Listen here).
 */
import { hashKey, type QueryClient } from "@tanstack/react-query";
import { messagesOldestFirst, type MessagePages } from "@/lib/chat/cache";
import { chatKeys } from "@/lib/chat/keys";
import type { AccountRef, Message } from "@/lib/chat/types";
import { notifyError } from "@/lib/notify";
import { onPlayerEvent, pause, play, playerState, resume, stop, stopIf, subscribePlayer, unlockAudio } from "./player";
import { beginSpeaking, clearQueue, EMPTY_QUEUE, finishSpeaking, headDeadline, nextToSpeak, observeMessages, returnToQueue, startQueue, type ListenQueue, type QueueFilter } from "./queue";
import { messageKey, messageTrack } from "./sources";

export interface ListenModeView {
  /** Listen mode is on for this conversation. */
  on: boolean;
  /** Spoken messages play as they arrive (the setting), even with Listen mode off. */
  autoplay: boolean;
  /** The message being spoken now (its code), or null. */
  speaking: string | null;
  /** Who is speaking (for "Speaking: Scout"). */
  speaker: string | null;
  /** How many are waiting after it. */
  waiting: number;
  /** Paused while the Carbon types or records. */
  held: boolean;
  /** On here, but another tab of this browser on the same conversation is the one speaking. */
  elsewhere: boolean;
  /** The next message waits while another tab's Listen mode speaks (they take turns). */
  turnElsewhere: boolean;
}

const OFF: ListenModeView = { on: false, autoplay: false, speaking: null, speaker: null, waiting: 0, held: false, elsewhere: false, turnElsewhere: false };
const TYPING_HOLD_MS = 2500;
const LOCK_PREFIX = "dm-listen:";

/** Conversations with Listen mode on in this tab (true) or turned off by the Carbon (false, wins over the default). */
const choices = new Map<string, boolean>();
/** Each open conversation's custodian view, for Listen (toggleListen has only the address). */
const views = new Map<string, string | null>();

interface Attached {
  queryClient: QueryClient;
  address: string;
  view: string | null;
  perspective: Pick<AccountRef, "id" | "uuid">;
  queryHash: string;
  /** The service speaks (without it Listen mode and autoplay stay off; deletions are still watched). */
  available: boolean;
  on: boolean;
  autoplay: boolean;
  queue: ListenQueue;
  holds: Set<string>;
  /** The queue's message that a hold paused, to resume. */
  pausedByHold: string | null;
  graceTimer: ReturnType<typeof setTimeout> | null;
  typingTimer: ReturnType<typeof setTimeout> | null;
  /** This tab is the one that speaks for the conversation (it holds the lock, or the browser has no Web Locks). */
  speaker: boolean;
  /** Another tab holds the lock while this one listens (it waits in line). */
  elsewhere: boolean;
  /** The message to speak waits for another tab's Listen mode to finish its own. */
  turnElsewhere: boolean;
  /** This tab's claim on the lock: aborting it lets go (a waiting request is withdrawn, a held lock released). */
  claim: AbortController | null;
  cleanup: Array<() => void>;
}

let attached: Attached | null = null;
let snapshot: ListenModeView = OFF;
const listeners = new Set<() => void>();

function publish(): void {
  const current = attached;
  const next: ListenModeView = current
    ? {
        on: current.available && current.on,
        autoplay: current.available && current.autoplay,
        speaking: current.queue.current,
        speaker: current.queue.current ? speakerOf(current, current.queue.current) : null,
        waiting: current.queue.waiting.length,
        held: current.holds.size > 0,
        elsewhere: current.available && current.on && current.elsewhere && !current.speaker,
        turnElsewhere: !!current.queue.current && current.turnElsewhere,
      }
    : OFF;
  if (
    next.on === snapshot.on &&
    next.autoplay === snapshot.autoplay &&
    next.speaking === snapshot.speaking &&
    next.speaker === snapshot.speaker &&
    next.waiting === snapshot.waiting &&
    next.held === snapshot.held &&
    next.elsewhere === snapshot.elsewhere &&
    next.turnElsewhere === snapshot.turnElsewhere
  )
    return;
  snapshot = next;
  for (const listener of listeners) listener();
}

function messagesOf(current: Attached): Message[] {
  return messagesOldestFirst(current.queryClient.getQueryData<MessagePages>(chatKeys.messages(current.address, current.view)));
}

function speakerOf(current: Attached, code: string): string | null {
  const message = messagesOf(current).find(item => item["message-id"] === code);
  return message ? message.sender.display_name?.trim() || message.sender.id : null;
}

/**
 * A queue for which everything loaded now is the past. Before the conversation's messages have loaded there is no
 * past yet: the queue starts on the first look at them (so the backlog that loads with the page stays quiet).
 */
function freshQueue(current: Attached): ListenQueue {
  return current.queryClient.getQueryData(chatKeys.messages(current.address, current.view)) ? startQueue(messagesOf(current)) : EMPTY_QUEUE;
}

const filterOf = (current: Attached): QueueFilter => (current.on ? "all" : "speech");
const listening = (current: Attached) => current.available && (current.on || current.autoplay);

/* One tab speaks ------------------------------------------------------------------------------------------------------ */

function lockManager(): LockManager | null {
  return typeof navigator !== "undefined" && navigator.locks && typeof navigator.locks.request === "function" ? navigator.locks : null;
}

/**
 * Asks to be the tab that speaks for the attached conversation. `takeOver` (the Carbon turned Listen mode on here)
 * takes the lock from the tab that has it; otherwise this tab waits in line.
 */
function claimSpeaker(current: Attached, takeOver = false): void {
  const locks = lockManager();
  if (!locks) {
    current.speaker = true;
    return;
  }
  if (current.speaker || (current.claim && !takeOver)) return;
  const previous = current.claim;
  const claim = new AbortController();
  current.claim = claim;
  previous?.abort();
  const name = `${LOCK_PREFIX}${current.address}`;
  // `steal` cannot be withdrawn with a signal (it is granted at once); a waiting request can.
  locks
    .request(name, takeOver ? { steal: true } : { signal: claim.signal }, () => {
      if (attached !== current || current.claim !== claim) return undefined;
      current.speaker = true;
      current.elsewhere = false;
      // What another tab said, and what arrived while this one waited, is the past.
      current.queue = freshQueue(current);
      pump();
      publish();
      return new Promise<void>(resolve => claim.signal.addEventListener("abort", () => resolve(), { once: true }));
    })
    .catch(() => {
      // Another tab took the lock over (or this claim was let go, which leaves `claim` behind).
      if (attached !== current || current.claim !== claim) return;
      current.claim = null;
      claim.abort();
      yieldSpeaker(current);
      claimSpeaker(current);
    });
  if (!takeOver) {
    // Waiting in line: say so when another tab is the one speaking.
    void locks
      .query()
      .then(state => {
        if (attached !== current || current.claim !== claim || current.speaker) return;
        current.elsewhere = (state.held ?? []).some(lock => lock.name === name);
        publish();
      })
      .catch(() => undefined);
  }
}

/** Lets go of the lock (leaving the conversation, or no longer listening): the next tab in line speaks. */
function releaseSpeaker(current: Attached): void {
  const claim = current.claim;
  current.claim = null;
  current.speaker = false;
  current.elsewhere = false;
  claim?.abort();
}

/** Another tab speaks now: what this one was saying stops, and nothing waits here. */
function yieldSpeaker(current: Attached): void {
  const code = queueCodeOf(playerState().key);
  current.speaker = false;
  current.elsewhere = true;
  current.queue = clearQueue(current.queue);
  current.pausedByHold = null;
  endTurn();
  if (code) stop();
  publish();
}

/** Claims or lets go of the lock as listening starts or ends. */
function followListening(current: Attached, takeOver = false): void {
  if (listening(current)) claimSpeaker(current, takeOver);
  else releaseSpeaker(current);
}

/* The queue ----------------------------------------------------------------------------------------------------------- */

/** Speaks the next message if nothing else is playing and nothing holds it back. */
function pump(): void {
  const current = attached;
  if (!current) return;
  if (current.graceTimer) clearTimeout(current.graceTimer);
  current.graceTimer = null;
  if (!listening(current) || !current.speaker || current.holds.size) return publish();
  const player = playerState();
  if (player.status === "loading" || player.status === "playing") return publish();
  if (player.status === "paused" && player.origin === "queue") return publish();
  const messages = messagesOf(current);
  const byCode = new Map(messages.map(message => [message["message-id"], message]));
  const next = nextToSpeak(current.queue, byCode);
  if (!next) {
    const deadline = headDeadline(current.queue, byCode);
    if (deadline) current.graceTimer = setTimeout(pump, Math.max(0, deadline - Date.now()) + 50);
    return publish();
  }
  current.queue = beginSpeaking(current.queue, next["message-id"]);
  publish();
  speakInTurn(current, next);
}

/** This tab's turn to speak (its request for the shared voice lock, or the lock it holds while a message plays). */
let turn: AbortController | null = null;

function endTurn(): void {
  const previous = turn;
  turn = null;
  previous?.abort();
  if (attached?.turnElsewhere) attached.turnElsewhere = false;
}

/**
 * Plays a queued message when no other tab's Listen mode is speaking: tabs listening to different conversations take
 * turns through the Web Lock `dm-listen-voice` (held while the message plays, paused included), so two voices never
 * talk over each other. Without Web Locks it plays at once.
 */
function speakInTurn(current: Attached, message: Message): void {
  const code = message["message-id"];
  const track = messageTrack({ address: current.address, message, view: current.view, origin: "queue" });
  const locks = lockManager();
  if (!locks) {
    play(track);
    return;
  }
  endTurn();
  const mine = new AbortController();
  turn = mine;
  let granted = false;
  locks
    .request("dm-listen-voice", { signal: mine.signal }, () => {
      granted = true;
      if (current.turnElsewhere) {
        current.turnElsewhere = false;
        publish();
      }
      // Stopped, left, or another message took its place while this tab waited: nothing to say.
      if (mine.signal.aborted || attached !== current || current.queue.current !== code) return undefined;
      // The Carbon started something else meanwhile: it plays out first, and this message waits at the head.
      const player = playerState();
      if (player.status === "loading" || player.status === "playing") {
        current.queue = returnToQueue(current.queue, code);
        if (turn === mine) turn = null;
        publish();
        return undefined;
      }
      play(track);
      return new Promise<void>(resolve => {
        const done = () => {
          off();
          if (turn === mine) turn = null;
          resolve();
        };
        const off = onPlayerEvent(event => {
          if (event.type !== "next" && event.key === track.key) done();
        });
        mine.signal.addEventListener("abort", done, { once: true });
      });
    })
    .catch(() => undefined);
  // Waiting for another tab: say so (instead of "Speaking…" while nothing is heard here).
  void locks
    .query()
    .then(state => {
      if (granted || turn !== mine || attached !== current) return;
      current.turnElsewhere = (state.held ?? []).some(lock => lock.name === "dm-listen-voice");
      publish();
    })
    .catch(() => undefined);
}

/** Whatever plays from a message of this conversation stops once the message is deleted. */
function stopDeleted(current: Attached, messages: readonly Message[]): void {
  const key = playerState().key;
  const prefix = messageKey(current.address, "");
  if (!key || !key.startsWith(prefix)) return;
  const code = key.slice(prefix.length);
  if (messages.some(message => message["message-id"] === code && message.deleted_at)) stop();
}

function observe(): void {
  const current = attached;
  if (!current) return;
  const messages = messagesOf(current);
  stopDeleted(current, messages);
  if (!listening(current)) return;
  const before = current.queue;
  current.queue = observeMessages(current.queue, messages, current.perspective, filterOf(current));
  // A waiting spoken message may have just been rendered (its status changed, not the queue): look again.
  if (current.queue !== before || current.queue.waiting.length) pump();
}

/** The code the queue is speaking, if the player's current track is it. */
function queueCodeOf(key: string | null): string | null {
  const current = attached;
  if (!current?.queue.current || !key) return null;
  return key === messageKey(current.address, current.queue.current) ? current.queue.current : null;
}

function onTyping(event: Event): void {
  const target = event.target instanceof Element ? event.target : null;
  if (!target?.closest("[data-composer-field]")) return;
  const current = attached;
  if (!current) return;
  if (current.typingTimer) clearTimeout(current.typingTimer);
  current.typingTimer = setTimeout(() => {
    if (attached === current) holdListening("typing", false);
  }, TYPING_HOLD_MS);
  holdListening("typing", true);
}

/**
 * Holds Listen mode while the Carbon types or records (`reason` names the hold): what it was saying pauses and resumes
 * when every hold is released.
 */
export function holdListening(reason: string, held: boolean): void {
  const current = attached;
  if (!current) return;
  const had = current.holds.size > 0;
  if (held) current.holds.add(reason);
  else current.holds.delete(reason);
  const has = current.holds.size > 0;
  if (has && !had) {
    const player = playerState();
    const code = queueCodeOf(player.key);
    if (code && (player.status === "playing" || player.status === "loading")) {
      current.pausedByHold = code;
      if (player.status === "playing") pause();
    }
  } else if (!has && had) {
    const player = playerState();
    const paused = current.pausedByHold;
    current.pausedByHold = null;
    if (paused && queueCodeOf(player.key) === paused && player.status === "paused") resume();
    else pump();
  }
  publish();
}

export interface AttachOptions {
  /** Listening works here (the service speaks). */
  available: boolean;
  listenMode: boolean;
  autoplay: boolean;
}

/** Starts watching a conversation (the thread header calls this while the conversation is open). */
function attach(queryClient: QueryClient, address: string, view: string | null, perspective: Pick<AccountRef, "id" | "uuid">, options: AttachOptions): () => void {
  detach();
  views.set(address, view);
  const current: Attached = {
    queryClient,
    address,
    view,
    perspective,
    queryHash: hashKey(chatKeys.messages(address, view)),
    available: options.available,
    on: choices.get(address) ?? options.listenMode,
    autoplay: options.autoplay,
    queue: EMPTY_QUEUE,
    holds: new Set(),
    pausedByHold: null,
    graceTimer: null,
    typingTimer: null,
    speaker: false,
    elsewhere: false,
    turnElsewhere: false,
    claim: null,
    cleanup: [],
  };
  attached = current;
  current.queue = freshQueue(current);
  current.cleanup.push(
    queryClient.getQueryCache().subscribe(event => {
      if (event.type === "updated" && event.query.queryHash === current.queryHash && attached === current) observe();
    }),
    // Held (the Carbon writes or records) while a message was still loading: it pauses the moment it starts.
    subscribePlayer(() => {
      if (attached !== current || !current.holds.size) return;
      const player = playerState();
      const code = queueCodeOf(player.key);
      if (!code || player.status !== "playing") return;
      current.pausedByHold = code;
      pause();
    }),
    onPlayerEvent(event => {
      if (attached !== current) return;
      if (event.type === "next") {
        if (queueCodeOf(event.key)) stop();
        return;
      }
      const code = queueCodeOf(event.key);
      if (code) current.queue = finishSpeaking(current.queue, code);
      // Another message started by hand (or a sample) takes over; the queue waits until it is done.
      pump();
    }),
  );
  document.addEventListener("input", onTyping, true);
  current.cleanup.push(() => document.removeEventListener("input", onTyping, true));
  followListening(current);
  publish();
  return () => {
    if (attached === current) detach();
  };
}

function detach(): void {
  const current = attached;
  if (!current) return;
  attached = null;
  for (const cleanup of current.cleanup) cleanup();
  if (current.graceTimer) clearTimeout(current.graceTimer);
  if (current.typingTimer) clearTimeout(current.typingTimer);
  releaseSpeaker(current);
  endTurn();
  // What this conversation was saying stops with it: Listen mode, Listen, and a bubble's own player too (nothing on
  // the next screen would show what plays, or pause it).
  const prefix = messageKey(current.address, "");
  stopIf(key => key.startsWith(prefix));
  publish();
}

/** Turns Listen mode on or off for the attached conversation (a click or ⌘⇧L: it unlocks audio). */
export function setListenMode(address: string, on: boolean): void {
  choices.set(address, on);
  const current = attached;
  if (!current || current.address !== address) return;
  if (on) unlockAudio();
  if (on === current.on) {
    // On already, but another tab speaks: this press brings it here.
    if (on && !current.speaker) followListening(current, true);
    publish();
    return;
  }
  current.on = on;
  // Either way, from now on: what is there already is the past.
  const code = on ? null : queueCodeOf(playerState().key);
  current.queue = freshQueue(current);
  if (!on) endTurn();
  if (code) stop();
  followListening(current, on);
  pump();
  publish();
}

/** Follows Settings → Listening ("Play spoken messages when they arrive") for the attached conversation. */
export function setAutoplay(on: boolean): void {
  const current = attached;
  if (!current || current.autoplay === on) return;
  current.autoplay = on;
  if (!current.on) current.queue = freshQueue(current);
  followListening(current);
  publish();
}

/** Whether the service speaks (the attached conversation's Listen mode and autoplay need it). */
export function setListenAvailable(available: boolean): void {
  const current = attached;
  if (!current || current.available === available) return;
  current.available = available;
  current.queue = freshQueue(current);
  if (!available) {
    const code = queueCodeOf(playerState().key);
    endTurn();
    if (code) stop();
  }
  followListening(current);
  publish();
}

/** Stop: what is being said, and everything waiting; new messages are still spoken while Listen mode is on. */
export function stopSpeaking(): void {
  const current = attached;
  if (!current) return;
  const code = queueCodeOf(playerState().key);
  current.queue = clearQueue(current.queue);
  endTurn();
  if (code) stop();
  publish();
}

/** The custodian view a conversation is open in (for Listen), from the open conversation or the address bar. */
export function viewFor(address: string): string | null {
  if (views.has(address)) return views.get(address) ?? null;
  if (typeof window === "undefined") return null;
  return new URLSearchParams(window.location.search).get("for");
}

export function listenModeView(): ListenModeView {
  return snapshot;
}

export function subscribeListenMode(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export { attach as attachListenMode };

/* Failures of what plays without a bubble (Listen on words, Listen mode) are told in a toast. -------------------------- */

let toasting = false;
export function reportPlaybackFailures(): void {
  if (toasting || typeof window === "undefined") return;
  toasting = true;
  onPlayerEvent(event => {
    if (event.type !== "failed" || (event.origin !== "listen" && event.origin !== "queue")) return;
    const error = playerState().error;
    if (error) notifyError(error, "The message could not be played");
  });
}
