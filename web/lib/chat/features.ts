/**
 * Whether this tab hears about conversations through the sync feed (DM 0.17 announces it in GET /api/v1/contracts as
 * `features.conversation_events`; the sync loop asks once and then reads `include=conversations`). While it does, a new
 * invitation, a group joined or left on another device, a rename and a change of members arrive within a second, so
 * the website stops asking for the invitations every 15 s and the group panel every 30 s; against an older service it
 * keeps asking.
 */
import { useSyncExternalStore } from "react";
import type { Contracts } from "./api";

let active = false;
const listeners = new Set<() => void>();

/** True when the service's contracts offer conversation events in the sync feed this website can read. */
export function offersConversationEvents(contracts: Contracts | null | undefined): boolean {
  const feature = contracts?.features?.conversation_events;
  return !!feature && typeof feature === "object" && feature.sync_query === "include=conversations";
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
