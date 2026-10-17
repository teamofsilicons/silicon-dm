/**
 * "Send as soon as it can go": Enter while a file is still uploading, and the microphone's `sendWhenReady()` after a
 * voice note (voice-bridge.ts), ask for a send that waits for the tray. The latch holds that one wish: asking again
 * while it waits changes nothing (exactly one send, however many times it is asked), and it is let go the moment the
 * message is sent, cancelled (Esc) or blocked by a failed upload (the Carbon decides: retry or remove it).
 *
 * Pure: tray-store.ts keeps one latch per conversation (so a wish outlives the composer when the Carbon opens another
 * conversation) and settles it whenever the tray changes. tests/composer-send.test.ts pins it down, and the words the
 * composer says about it.
 */
import { trayBusy, trayFailed, trayHasContent, type TrayItem } from "./tray";

export interface TrayState {
  /** Something is still uploading. */
  busy: boolean;
  /** Something failed (it waits for Retry or Remove). */
  failed: boolean;
  /** Words or ready items to send. */
  content: boolean;
}

export type SendDecision = "send" | "wait" | "blocked" | "nothing";

/** What the latch decides on: uploading, failed, and whether anything is there to send. */
export const trayState = (items: TrayItem[], words: string): TrayState => ({
  busy: trayBusy(items),
  failed: trayFailed(items),
  content: words.trim().length > 0 || trayHasContent(items),
});

/** What a send asked for right now does: go, wait for the tray, refuse because something failed, or nothing to send. */
export function decideSend(state: TrayState): SendDecision {
  if (state.failed) return "blocked";
  if (state.busy) return "wait";
  return state.content ? "send" : "nothing";
}

export class SendLatch {
  private waiting = false;

  /** Whether a send is waiting for the tray. */
  get pending(): boolean {
    return this.waiting;
  }

  /** Asks for a send. Answers what to do now; "wait" holds the wish until `settle` lets it go. */
  request(state: TrayState): SendDecision {
    const decision = decideSend(state);
    if (decision === "wait") this.waiting = true;
    else if (decision === "send") this.waiting = false;
    return decision;
  }

  /** The tray changed: "send" exactly once when the waiting send can go, "blocked" when it cannot, else null. */
  settle(state: TrayState): Exclude<SendDecision, "wait"> | null {
    if (!this.waiting) return null;
    const decision = decideSend(state);
    if (decision === "wait") return null;
    this.waiting = false;
    return decision;
  }

  cancel(): boolean {
    const was = this.waiting;
    this.waiting = false;
    return was;
  }
}

/** "The GIF", "harbour.png": how the composer names an item in a sentence. */
export const itemName = (item: TrayItem) => (item.kind === "file" ? item.name : item.kind === "voice" ? "The voice message" : item.kind === "gif" ? "The GIF" : "The link");

const inSentence = (item: TrayItem) => itemName(item).replace(/^The /, "the ");

/** The line under the field while a send waits: what it waits for (a Cancel button beside it, and Esc, take it back). */
export function waitingWords(items: TrayItem[], online = true): string {
  const uploading = items.filter(item => item.status === "uploading");
  const what = uploading.length === 1 ? `${inSentence(uploading[0]!)} is uploaded` : `${uploading.length} files are uploaded`;
  return online ? `Sends when ${what}.` : `You are offline: it sends when you are back and ${what}.`;
}

/**
 * Why a send cannot go: what failed, and what to do about it. Retry is offered only for what can go again (a file
 * this tab still has); an empty file or a voice message that failed can only be removed.
 */
export function blockedWords(items: TrayItem[], canRetry: (id: string) => boolean = () => true): string {
  const failed = items.filter(item => item.status === "failed");
  const retryable = failed.filter(item => item.kind === "file" && canRetry(item.id)).length;
  if (failed.length === 1) {
    return `${itemName(failed[0]!)} did not upload. ${retryable ? "Retry it or remove it" : "Remove it"}, then send.`;
  }
  const what = retryable === failed.length ? "Retry or remove them" : retryable ? "Retry or remove each one" : "Remove them";
  return `${failed.length} attachments did not upload. ${what}, then send.`;
}
