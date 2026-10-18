"use client";

/**
 * Browser notifications for new messages, opt-in (Settings → Notifications): only while the tab is hidden or another
 * conversation is open, one per conversation at a time (a newer one replaces it), and a click opens the message.
 * Nothing is sent anywhere: the browser shows them from what this tab already fetched.
 */
import { accountName, conversationHref } from "./address";
import { plainPreview } from "./markdown";
import { spokenWords } from "./preview";
import { chatSettings, updateChatSettings } from "./settings";
import type { Message } from "./types";

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
  if (onScreen) return;
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
