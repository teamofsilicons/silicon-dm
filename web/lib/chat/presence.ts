"use client";

/**
 * Presence: this tab's own lease (you are online while a DM page is visible) and what you are doing right now
 * (typing, recording a voice message…), and the presence of the people in the open conversation.
 *
 * - The lease is renewed every 25 s while the page is visible and closed on `pagehide` (a keepalive request, so it
 *   goes out while the page unloads). A hidden tab stops renewing and the lease runs out by itself.
 * - Activity is reported at most once every 3 s and cleared 5 s after the last report (spec §6.5), so a burst of
 *   keystrokes is one request and a pause clears "typing…" for the others.
 * - Others' presence is polled every 15 s for the open conversation only (useParticipantsPresence), and every 4 s
 *   while someone's activity shows there, so "typing…" does not linger 15 s after they stopped.
 * - Activity is about one conversation. A service that announces it (lib/chat/features.ts, `presence_activity`) hears
 *   which conversation this tab's activity is about (the composer's own, else the one on screen) and answers someone's
 *   activity in the open conversation only. Without it, an account's activity is the same in all of its conversations:
 *   "Ada is typing…" showed in Cy's thread while Ada wrote to Bo. Then nobody's activity is shown (online and last
 *   seen still are), rather than one that may be about another conversation.
 * - On such a service, activity that is about no conversation (or about one the service will not take: left, only
 *   invited to) is not reported at all: without a conversation the service would show it to older clients in every
 *   one. A renewal the service refuses renews nothing, not even the lease, so the lease is then renewed alone.
 */
import { useQueries } from "@tanstack/react-query";
import { useSyncExternalStore } from "react";
import { ApiError } from "@/lib/errors";
import { closePresence, getPresence, renewPresence } from "./api";
import { scopedActivityOffered, useScopedActivity } from "./features";
import { chatKeys } from "./keys";
import { readText, writeText } from "./storage";
import type { Activity, Presence } from "./types";

const RENEW_MS = 25_000;
const ACTIVITY_THROTTLE_MS = 3000;
const ACTIVITY_IDLE_MS = 5000;
const POLL_MS = 15_000;
/** While someone's activity shows, their presence is asked for this often, so "typing…" goes soon after they stop. */
const ACTIVE_POLL_MS = 4000;

let device: string | null = null;

/** This tab's device id: stable across reloads of the tab (sessionStorage), never a hardware id or a credential. */
export function deviceId(): string {
  if (device) return device;
  const stored = readText("dm:device:v1", "session");
  device = stored && /^web-[a-z0-9-]{8,64}$/.test(stored) ? stored : `web-${crypto.randomUUID()}`;
  writeText("dm:device:v1", device, "session");
  return device;
}

let activity: Activity | null = null;
/** The conversation `activity` is about: the one its composer belongs to, else the one on screen when it was reported. */
let activityWhere: string | null = null;
let lastSentActivity: Activity | null = null;
let lastSentWhere: string | null = null;
let lastSentAt = 0;
/** Conversations the service would not take activity about, until when (a minute): meanwhile the lease goes alone. */
const refused = new Map<string, number>();
const REFUSED_MS = 60_000;
let idleTimer: ReturnType<typeof setTimeout> | null = null;
let renewTimer: ReturnType<typeof setInterval> | null = null;
let running = 0;

const visible = () => typeof document === "undefined" || document.visibilityState === "visible";

/** The conversation on screen in this tab (null on the inbox, the settings and in a custodian's view). */
let onScreen: string | null = null;
const screenListeners = new Set<() => void>();

/** The chat shell says which conversation is on screen: what this tab is doing is about that one. */
export function setPresenceConversation(address: string | null): void {
  if (onScreen === address) return;
  onScreen = address;
  for (const listener of screenListeners) listener();
}

function useOnScreen(): string | null {
  return useSyncExternalStore(
    listener => {
      screenListeners.add(listener);
      return () => screenListeners.delete(listener);
    },
    () => onScreen,
    () => null,
  );
}

/** A renewal refused because of the conversation it named (left, only invited to, not visible, malformed). */
function refusedConversation(error: unknown): boolean {
  const failure = ApiError.from(error);
  return failure.details.field === "conversation_id" || failure.status === 403 || failure.status === 404 || failure.status === 422;
}

function renew() {
  if (!visible()) return;
  const scoped = scopedActivityOffered();
  // On a service that says activity per conversation, activity goes with its conversation or not at all.
  const where = activity && scoped && activityWhere && (refused.get(activityWhere) ?? 0) <= Date.now() ? activityWhere : null;
  const sending: Activity | null = scoped && !where ? null : activity;
  lastSentAt = Date.now();
  lastSentActivity = activity;
  lastSentWhere = activityWhere;
  void renewPresence(deviceId(), sending, where).catch(error => {
    if (!where || !refusedConversation(error)) return;
    refused.set(where, Date.now() + REFUSED_MS);
    // A refused renewal renews nothing, not even the lease: renew it alone, so this tab stays online.
    void renewPresence(deviceId(), null, null).catch(() => undefined);
  });
}

/** Starts this tab's lease (once, however many callers) and returns a stop function. */
export function startPresence(): () => void {
  running += 1;
  if (running === 1) {
    renew();
    renewTimer = setInterval(renew, RENEW_MS);
    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("pagehide", onPageHide);
  }
  return () => {
    running -= 1;
    if (running > 0) return;
    if (renewTimer) clearInterval(renewTimer);
    renewTimer = null;
    document.removeEventListener("visibilitychange", onVisibility);
    window.removeEventListener("pagehide", onPageHide);
    void closePresence(deviceId()).catch(() => undefined);
  };
}

function onVisibility() {
  if (visible()) renew();
}

function onPageHide() {
  void closePresence(deviceId(), true);
}

/**
 * Says what you are doing (or null when you stopped), and in which conversation: `conversation` is the address of the
 * composer it happens in (an upload goes on in its own conversation after you moved to another); without it, the one
 * on screen. Repeated calls within 3 s send nothing new; 5 s after the last call the activity clears by itself.
 */
export function reportActivity(next: Activity | null, conversation?: string | null): void {
  if (!running) return;
  const where = next ? conversation ?? onScreen : null;
  activity = next;
  activityWhere = where;
  if (idleTimer) clearTimeout(idleTimer);
  idleTimer = null;
  if (next) {
    idleTimer = setTimeout(() => reportActivity(null), ACTIVITY_IDLE_MS);
    if (next === lastSentActivity && where === lastSentWhere && Date.now() - lastSentAt < ACTIVITY_THROTTLE_MS) return;
  } else if (lastSentActivity === null) {
    return;
  }
  renew();
}

/**
 * What may be shown of someone's presence in the open conversation: everything, when the service said its activity is
 * about this conversation; otherwise all but the activity (it may be about another one).
 */
export function presenceHere(presence: Presence, scoped: boolean): Presence {
  return scoped || !presence.activity ? presence : { ...presence, activity: null };
}

/**
 * The presence of these accounts, polled every 15 s while `enabled` (the open conversation's other members); with
 * `enabled` false, what was last heard (the sidebar's row for the open conversation reads it so).
 */
export function useParticipantsPresence(accountIds: string[], enabled: boolean): Map<string, Presence> {
  const scoped = useScopedActivity();
  const conversation = useOnScreen();
  const scope = scoped ? conversation : null;
  const results = useQueries({
    queries: accountIds.map(id => ({
      queryKey: chatKeys.presence(id, scope),
      queryFn: async ({ signal }: { signal: AbortSignal }) => presenceHere(await getPresence(id, signal, scope), !!scope),
      enabled,
      refetchInterval: (query: { state: { data?: Presence } }) => (query.state.data?.activity ? ACTIVE_POLL_MS : POLL_MS),
      refetchIntervalInBackground: false,
      staleTime: 10_000,
      retry: false,
      meta: { toast: false },
    })),
  });
  const out = new Map<string, Presence>();
  results.forEach((result, index) => {
    if (result.data) out.set(accountIds[index]!, result.data);
  });
  return out;
}

/** "online", "typing…", "recording a voice message…", "last seen 5 minutes ago": what the header says. */
export function activityWords(value: Activity): string {
  switch (value) {
    case "typing":
      return "typing…";
    case "recording_voice":
      return "recording a voice message…";
    case "transcribing_voice":
      return "transcribing a voice message…";
    case "uploading_file":
      return "uploading a file…";
    case "searching_gifs":
      return "looking for a GIF…";
  }
}
