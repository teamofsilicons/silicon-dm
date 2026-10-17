/**
 * Conversation news between this browser's tabs. When a tab starts, joins or leaves a conversation, answers an
 * invitation or changes a group, the account's other tabs refresh their list and invitations at once
 * (BroadcastChannel). The DM service's sync feed carries messages, not these changes, so without this another tab would
 * learn of them only at its next refresh. Best effort: a browser without BroadcastChannel just waits for that refresh.
 */
import type { QueryClient } from "@tanstack/react-query";
import { refreshConversationsSoon } from "./sync";

const channelName = (account: string) => `dm:conversations:v1:${account}`;
const CHANGED = "conversations.changed";

/** Tells this account's other tabs that its conversations or invitations changed here. */
export function announceConversationsChanged(account: string): void {
  if (typeof BroadcastChannel === "undefined") return;
  try {
    const channel = new BroadcastChannel(channelName(account));
    channel.postMessage({ type: CHANGED });
    channel.close();
  } catch {
    // Nothing to do: the other tabs catch up at their next refresh.
  }
}

/** Refreshes the list and invitations when another tab of this account says they changed. Returns a stop function. */
export function listenForConversationNews(queryClient: QueryClient, account: string): () => void {
  if (typeof BroadcastChannel === "undefined") return () => undefined;
  const channel = new BroadcastChannel(channelName(account));
  channel.onmessage = event => {
    if ((event.data as { type?: unknown } | null)?.type === CHANGED) refreshConversationsSoon(queryClient);
  };
  return () => channel.close();
}
