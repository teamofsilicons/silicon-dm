"use client";

/**
 * Presence: this tab's own lease (you are online while a DM page is visible) and what you are doing right now
 * (typing, recording a voice message…), and the presence of the people in the open conversation.
 *
 * - The lease is renewed every 25 s while the page is visible and closed on `pagehide` (a keepalive request, so it
 *   goes out while the page unloads). A hidden tab stops renewing and the lease runs out by itself.
 * - Activity is reported at most once every 3 s and cleared 5 s after the last report (spec §6.5), so a burst of
 *   keystrokes is one request and a pause clears "typing…" for the others.
 * - Others' presence is polled every 15 s for the open conversation only (useParticipantsPresence).
 */
import { useQueries } from "@tanstack/react-query";
import { closePresence, getPresence, renewPresence } from "./api";
import { chatKeys } from "./keys";
import { readText, writeText } from "./storage";
import type { Activity, Presence } from "./types";

const RENEW_MS = 25_000;
const ACTIVITY_THROTTLE_MS = 3000;
const ACTIVITY_IDLE_MS = 5000;
const POLL_MS = 15_000;

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
let lastSentActivity: Activity | null = null;
let lastSentAt = 0;
let idleTimer: ReturnType<typeof setTimeout> | null = null;
let renewTimer: ReturnType<typeof setInterval> | null = null;
let running = 0;

const visible = () => typeof document === "undefined" || document.visibilityState === "visible";

function renew() {
  if (!visible()) return;
  lastSentAt = Date.now();
  lastSentActivity = activity;
  void renewPresence(deviceId(), activity).catch(() => undefined);
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
 * Says what you are doing (or null when you stopped). Repeated calls within 3 s send nothing new; 5 s after the last
 * call the activity clears by itself.
 */
export function reportActivity(next: Activity | null): void {
  if (!running) return;
  activity = next;
  if (idleTimer) clearTimeout(idleTimer);
  idleTimer = null;
  if (next) {
    idleTimer = setTimeout(() => reportActivity(null), ACTIVITY_IDLE_MS);
    if (next === lastSentActivity && Date.now() - lastSentAt < ACTIVITY_THROTTLE_MS) return;
  } else if (lastSentActivity === null) {
    return;
  }
  renew();
}

/** The presence of these accounts, polled every 15 s while `enabled` (the open conversation's other members). */
export function useParticipantsPresence(accountIds: string[], enabled: boolean): Map<string, Presence> {
  const results = useQueries({
    queries: accountIds.map(id => ({
      queryKey: chatKeys.presence(id),
      queryFn: ({ signal }: { signal: AbortSignal }) => getPresence(id, signal),
      enabled,
      refetchInterval: POLL_MS,
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
