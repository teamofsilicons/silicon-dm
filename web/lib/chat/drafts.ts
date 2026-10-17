/**
 * Drafts are local first and follow you between devices (UNDERSTANDING: Drafts). What you type is kept in this
 * browser at once and saved to the service shortly after, with the version you last saw (If-Match); the service
 * refuses a save made against an older version (409), and then the text you are typing wins and is saved again
 * against the newer version. This file is the pure part: what a local draft and the service's draft make together.
 * lib/chat/draft-store.ts does the storage and the saving.
 */

export interface LocalDraft {
  text: string;
  /** The message code being replied to, if any. */
  replyTo: string | null;
  /** The service's version this text was based on (0: the service has none). */
  version: number;
  /** Changed here since the service last saw it. */
  dirty: boolean;
  /** When it was last changed here (epoch ms). */
  updatedAt: number;
}

export interface ServerDraft {
  text: string;
  replyTo: string | null;
  version: number;
  /** When the service last saved it (epoch ms). */
  updatedAt: number;
}

export type MergeAction =
  /** Nothing to do: both agree. */
  | "none"
  /** Show the service's draft (written on another device). */
  | "adopt"
  /** Save the local text to the service (with the merged version). */
  | "push"
  /** The service has no draft any more and this one was saved there: it was sent or discarded elsewhere. */
  | "clear";

export interface MergeResult {
  draft: LocalDraft | null;
  action: MergeAction;
}

export const emptyDraft = (now: number): LocalDraft => ({ text: "", replyTo: null, version: 0, dirty: false, updatedAt: now });

const isBlank = (draft: Pick<LocalDraft, "text" | "replyTo"> | null | undefined) => !draft || (!draft.text.trim() && !draft.replyTo);

/**
 * What the composer should hold when the service's draft (or its absence: null) is known.
 *
 * - Unsaved local changes always win; they are saved against the service's current version.
 * - A clean local draft follows the service: newer text from another device is adopted.
 * - A clean local draft the service no longer has was sent or discarded on another device, so it is cleared.
 */
export function mergeDraft(local: LocalDraft | null, server: ServerDraft | null, now: number): MergeResult {
  if (!server) {
    if (!local) return { draft: null, action: "none" };
    if (local.dirty) return isBlank(local) ? { draft: { ...local, version: 0, dirty: false }, action: "none" } : { draft: { ...local, version: 0 }, action: "push" };
    return local.version > 0 ? { draft: null, action: "clear" } : { draft: local, action: "none" };
  }
  if (!local) return { draft: { text: server.text, replyTo: server.replyTo, version: server.version, dirty: false, updatedAt: server.updatedAt || now }, action: isBlank(server) ? "none" : "adopt" };
  if (local.dirty) {
    const same = local.text === server.text && local.replyTo === server.replyTo;
    if (same) return { draft: { ...local, version: server.version, dirty: false }, action: "none" };
    return { draft: { ...local, version: server.version }, action: "push" };
  }
  if (server.version === local.version && server.text === local.text && server.replyTo === local.replyTo) return { draft: local, action: "none" };
  return { draft: { text: server.text, replyTo: server.replyTo, version: server.version, dirty: false, updatedAt: Math.max(server.updatedAt, local.updatedAt) }, action: "adopt" };
}

/** A local edit: the text (and reply) the Carbon now has, still to be saved. */
export function editDraft(current: LocalDraft | null, change: Partial<Pick<LocalDraft, "text" | "replyTo">>, now: number): LocalDraft {
  const base = current ?? emptyDraft(now);
  return { ...base, ...change, dirty: true, updatedAt: now };
}

/** The service saved `saved` (the text that was sent to it) and answered `version`. */
export function savedDraft(current: LocalDraft, saved: Pick<LocalDraft, "text" | "replyTo">, version: number): LocalDraft {
  const unchanged = current.text === saved.text && current.replyTo === saved.replyTo;
  return { ...current, version, dirty: !unchanged };
}

/** Whether a draft is worth keeping or saving at all. */
export const hasContent = (draft: Pick<LocalDraft, "text" | "replyTo"> | null | undefined) => !isBlank(draft);

/**
 * What the conversation list says about the signed-in account's draft in a conversation (`draft_version` on a
 * conversation summary): its version, null when there is none, "unknown" when the service does not say (it answers
 * the draft itself, and 404 when there is none), "pending" while the summary is still on its way.
 */
export type DraftHint = number | null | "unknown" | "pending";

/** The hint a conversation summary gives (draftHint), or "pending" before the summary is there. */
export function draftHint(conversation: object | null | undefined): DraftHint {
  if (!conversation) return "pending";
  if (!("draft_version" in conversation)) return "unknown";
  const value = (conversation as { draft_version?: unknown }).draft_version;
  if (value === null) return null;
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : "unknown";
}

/**
 * Whether to ask the service for the draft: "fetch" (it may have one this browser has not seen), "absent" (it has
 * none: settle with that, no request), "same" (it has the version this browser holds), or "wait" (not known yet).
 * Asking for a draft that is not there answers 404, which every browser logs as an error, so the list's word is taken
 * whenever the service gives it.
 */
export function draftCheck(local: LocalDraft | null, hint: DraftHint): "fetch" | "absent" | "same" | "wait" {
  if (hint === "pending") return "wait";
  if (hint === "unknown") return "fetch";
  // A list read before this browser saved its draft says "none": what was saved here is checked, never dropped on its word.
  if (hint === null) return local && local.version > 0 ? "fetch" : "absent";
  return local && !local.dirty && local.version === hint ? "same" : "fetch";
}
