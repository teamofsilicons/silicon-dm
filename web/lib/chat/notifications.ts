"use client";

/**
 * Browser notifications for new messages, opt-in (Settings → Notifications): only when the conversation is on screen in
 * no tab of this browser, one per conversation at a time (a newer one replaces it), and a click opens the message.
 * Nothing is sent anywhere: the browser shows them from what this tab already fetched.
 *
 * Every tab runs its own sync loop, so every tab hears of a message. Each visible tab therefore writes which
 * conversation it shows (`dm:screen:v1:<account>`, refreshed every 10 s while visible, removed when hidden), and a tab
 * leaves a message alone that another tab has on screen: one tab on the inbox used to notify about the message the
 * Carbon was reading in the other.
 */
import { accountName, conversationHref } from "./address";
import { plainPreview } from "./markdown";
import { spokenWords } from "./preview";
import { chatSettings, updateChatSettings } from "./settings";
import { readJson, writeJson } from "./storage";
import type { Message } from "./types";

/** This page's own name among the tabs (a duplicated tab copies sessionStorage, so not from there). */
const tab = typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : String(Math.random());
const BEAT_MS = 10_000;
/** A tab not heard from this long has gone (closed without saying so). */
const STALE_MS = 30_000;
type Screens = Record<string, { address: string | null; at: number }>;
const screenKey = (account: string) => `dm:screen:v1:${account}`;
let screen: { account: string; address: string | null } | null = null;

function writeScreen(): void {
  if (!screen || typeof document === "undefined") return;
  const now = Date.now();
  const all = readJson<Screens>(screenKey(screen.account), {});
  const next: Screens = {};
  for (const [id, entry] of Object.entries(all)) if (id !== tab && entry && now - entry.at < STALE_MS) next[id] = entry;
  if (document.visibilityState === "visible" && screen.address) next[tab] = { address: screen.address, at: now };
  writeJson(screenKey(screen.account), Object.keys(next).length ? next : null);
}

/**
 * Says, for this browser's other tabs, which conversation this tab shows while it is visible (the chat shell, once per
 * tab; `setScreenConversation` follows the page). Returns a stop function.
 */
export function trackScreen(account: string): () => void {
  screen = { account, address: screen?.account === account ? screen.address : null };
  const beat = window.setInterval(() => {
    if (document.visibilityState === "visible") writeScreen();
  }, BEAT_MS);
  document.addEventListener("visibilitychange", writeScreen);
  window.addEventListener("pagehide", forget);
  writeScreen();
  return () => {
    window.clearInterval(beat);
    document.removeEventListener("visibilitychange", writeScreen);
    window.removeEventListener("pagehide", forget);
    forget();
    screen = null;
  };
}

function forget(): void {
  if (!screen) return;
  const all = readJson<Screens>(screenKey(screen.account), {});
  if (!(tab in all)) return;
  delete all[tab];
  writeJson(screenKey(screen.account), Object.keys(all).length ? all : null);
}

/** The conversation this tab shows now (null: the inbox, the settings, a custodian's view). */
export function setScreenConversation(address: string | null): void {
  if (!screen || screen.address === address) return;
  screen.address = address;
  writeScreen();
}

/** Whether another visible tab of this browser shows this conversation. */
function onScreenElsewhere(conversation: string): boolean {
  if (!screen) return false;
  const now = Date.now();
  return Object.entries(readJson<Screens>(screenKey(screen.account), {})).some(([id, entry]) => id !== tab && entry?.address === conversation && now - entry.at < STALE_MS);
}

export type NotificationState = "unsupported" | "default" | "granted" | "denied";

export function notificationState(): NotificationState {
  if (typeof window === "undefined" || typeof Notification === "undefined") return "unsupported";
  return Notification.permission;
}

/** Asks the browser (a click must call this) and turns notifications on when it says yes. */
export async function enableNotifications(): Promise<NotificationState> {
  if (notificationState() === "unsupported") return "unsupported";
  const answer = Notification.permission === "granted" ? "granted" : await Notification.requestPermission();
  updateChatSettings({ notifications: answer === "granted" });
  return answer;
}

export function disableNotifications(): void {
  updateChatSettings({ notifications: false });
}

/**
 * What a notification says about a message: the words, or what it carries. A spoken message says its written caption
 * when it has one (as the thread shows it), else what the voice says, without the [cues].
 */
export function messageSummary(message: Message): string {
  if (message.deleted_at) return "This message was deleted";
  if (message.speech?.script) return plainPreview(message.message ?? "", 140) || plainPreview(spokenWords(message.speech.script), 140) || "Spoken message";
  const text = plainPreview(message.message ?? "", 140);
  if (text) return text;
  if (message.voice || message.voice_transcript) return message.voice_transcript ? `Voice message: ${plainPreview(message.voice_transcript, 120)}` : "Voice message";
  if (message.gif) return "GIF";
  if (message.attachments.length) return message.attachments.length === 1 ? "Attachment" : `${message.attachments.length} attachments`;
  return "New message";
}

/**
 * Shows a notification for a new message from someone else when the Carbon would not see it otherwise.
 * `openAddress` is the conversation on screen (null when none is).
 */
export function maybeNotify(message: Message, options: { title: string; openAddress: string | null; navigate: (href: string) => void }): void {
  if (!chatSettings().notifications || notificationState() !== "granted") return;
  const onScreen = document.visibilityState === "visible" && options.openAddress === message.conversation_id;
  if (onScreen || onScreenElsewhere(message.conversation_id)) return;
  try {
    const notification = new Notification(options.title || accountName(message.sender), {
      body: messageSummary(message),
      tag: `dm:${message.conversation_id}`,
      icon: "/apple-icon",
    });
    notification.onclick = () => {
      window.focus();
      options.navigate(conversationHref(message.conversation_id, { code: message["message-id"] }));
      notification.close();
    };
  } catch {
    // Some browsers only allow notifications from a service worker; there is nothing more to do here.
  }
}
