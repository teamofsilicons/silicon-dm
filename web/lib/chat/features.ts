/**
 * What this DM service offers beyond the basics, as GET /api/v1/contracts says (the sync loop asks once per tab):
 *
 * - Conversation events (`features.conversation_events`): this tab hears about conversations through the sync feed
 *   (`include=conversations`). While it does, a new invitation, a group joined or left on another device, a rename and
 *   a change of members arrive within a second, so the website stops asking for the invitations every 15 s and the
 *   group panel every 30 s; against an older service it keeps asking.
 * - Activity in a conversation (`features.presence_activity` with `scope: "conversation"`): what someone is doing
 *   (typing, recording…) is said about one conversation. A service without it reports an account's activity wherever
 *   it happens, so "Ada is typing…" in one conversation could mean she is typing to someone else: the website shows
 *   nobody's activity until the service says where it is (lib/chat/presence.ts).
 */
import { useSyncExternalStore } from "react";
import type { Contracts } from "./api";

let active = false;
let scopedActivity = false;
const listeners = new Set<() => void>();

/** True when the service's contracts offer conversation events in the sync feed this website can read. */
export function offersConversationEvents(contracts: Contracts | null | undefined): boolean {
  const feature = contracts?.features?.conversation_events;
  return !!feature && typeof feature === "object" && feature.sync_query === "include=conversations";
}

/** True when the service says what someone is doing per conversation (renew_presence's `conversation_id`). */
export function offersScopedActivity(contracts: Contracts | null | undefined): boolean {
  const feature = contracts?.features?.presence_activity;
  return !!feature && typeof feature === "object" && (feature as { scope?: unknown }).scope === "conversation";
}

/** Set by the sync loop from the contracts. */
export function setScopedActivity(next: boolean): void {
  if (scopedActivity === next) return;
  scopedActivity = next;
  for (const listener of listeners) listener();
}

export function scopedActivityOffered(): boolean {
  return scopedActivity;
}

/** Whether activity is said per conversation here (false while rendering on the server). */
export function useScopedActivity(): boolean {
  return useSyncExternalStore(subscribe, scopedActivityOffered, () => false);
}

/** Set by the sync loop: on once the contracts offer conversation events, off when it stops or the feed refuses them. */
export function setConversationEventsActive(next: boolean): void {
  if (active === next) return;
  active = next;
  for (const listener of listeners) listener();
}

export function conversationEventsActive(): boolean {
  return active;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Whether conversation events are reaching this tab now (false while rendering on the server). */
export function useConversationEvents(): boolean {
  return useSyncExternalStore(subscribe, conversationEventsActive, () => false);
}
