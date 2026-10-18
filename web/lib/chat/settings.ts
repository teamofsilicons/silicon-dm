"use client";

/**
 * The website's own preferences for this browser (Settings → Sending, Notifications, Listening): kept in
 * localStorage (`dm:settings:v1`), the same in every tab (the storage event), and safe without storage (defaults).
 * Preferences that belong to the account and must follow it everywhere (a Silicon's voice) live in the service.
 */
import { useSyncExternalStore } from "react";
import { onStorage, readJson, writeJson } from "./storage";

export type SendKey = "enter" | "mod-enter";
export type PlaybackSpeed = 1 | 1.5 | 2;

export interface ChatSettings {
  /** Enter sends and Shift+Enter is a new line, or ⌘/Ctrl+Enter sends and Enter is a new line. */
  sendWith: SendKey;
  /** Browser notifications for new messages while the tab is hidden or another conversation is open (opt-in). */
  notifications: boolean;
  /** Listening (the voice area fills these in): speak new speech messages on arrival, the default speed, Listen mode on by default. */
  autoplaySpeech: boolean;
  playbackSpeed: PlaybackSpeed;
  listenModeByDefault: boolean;
  /** The details panel is open on wide screens. */
  detailsOpen: boolean;
}

export const DEFAULT_SETTINGS: ChatSettings = {
  sendWith: "enter",
  notifications: false,
  autoplaySpeech: false,
  playbackSpeed: 1,
  listenModeByDefault: false,
  detailsOpen: false,
};

const KEY = "dm:settings:v1";
const listeners = new Set<() => void>();
let current: ChatSettings | null = null;

function load(): ChatSettings {
  const stored = readJson<Partial<ChatSettings>>(KEY, {});
  const merged = { ...DEFAULT_SETTINGS, ...(stored && typeof stored === "object" ? stored : {}) };
  if (merged.sendWith !== "enter" && merged.sendWith !== "mod-enter") merged.sendWith = "enter";
  if (![1, 1.5, 2].includes(merged.playbackSpeed)) merged.playbackSpeed = 1;
  return merged;
}

function snapshot(): ChatSettings {
  if (!current) current = load();
  return current;
}

let watching = false;
function subscribe(listener: () => void) {
  listeners.add(listener);
  if (!watching && typeof window !== "undefined") {
    watching = true;
    onStorage(KEY, () => {
      current = load();
      for (const each of listeners) each();
    });
  }
  return () => listeners.delete(listener);
}

/** The preferences, re-rendering when any of them changes (in this tab or another). */
export function useChatSettings(): ChatSettings {
  return useSyncExternalStore(subscribe, snapshot, () => DEFAULT_SETTINGS);
}

/** The preferences right now (outside React). */
export const chatSettings = (): ChatSettings => snapshot();

export function updateChatSettings(change: Partial<ChatSettings>): void {
  current = { ...snapshot(), ...change };
  writeJson(KEY, current);
  for (const listener of listeners) listener();
}
