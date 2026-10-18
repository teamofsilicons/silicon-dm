/**
 * Conversation news between this browser's tabs. When a tab starts, joins or leaves a conversation, answers an
 * invitation or changes a group, the account's other tabs refresh their list and invitations at once
 * (BroadcastChannel). The DM service's sync feed carries messages, not these changes, so without this another tab would
 * learn of them only at its next refresh. Best effort: a browser without BroadcastChannel just waits for that refresh.
 *
 * Reads travel the same way: when one tab marks a conversation read, the others take the unread count the service
 * answered (the feed tells only the senders of a read, so the other tabs' badges and "(n) DM" stayed until a refresh).
 */
import type { QueryClient } from "@tanstack/react-query";
import { applyReadToSummaries, type ConversationPages } from "./cache";
import { chatKeys } from "./keys";
import { refreshConversationsSoon } from "./sync";
import type { ReceiptsResult } from "./types";

const channelName = (account: string) => `dm:conversations:v1:${account}`;
const CHANGED = "conversations.changed";
const READ = "conversation.read";

interface ReadNews {
  type: typeof READ;
  address: string;
  unread_count: number;
  last_read_message_id?: string | null;
}

/** Tells this account's other tabs that a conversation was read here, with the count the service answered. */
export function announceRead(account: string, address: string, result: Partial<ReceiptsResult> & { unread_count: number }): void {
  if (typeof BroadcastChannel === "undefined") return;
  try {
    const channel = new BroadcastChannel(channelName(account));
    const news: ReadNews = { type: READ, address, unread_count: result.unread_count, ...(result.last_read_message_id !== undefined ? { last_read_message_id: result.last_read_message_id } : {}) };
    channel.postMessage(news);
    channel.close();
  } catch {
    // The other tabs catch up at their next refresh.
  }
}

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

/**
 * Refreshes the list and invitations when another tab of this account says they changed, and takes the unread count of
 * a conversation read there. Returns a stop function.
 */
export function listenForConversationNews(queryClient: QueryClient, account: string): () => void {
  if (typeof BroadcastChannel === "undefined") return () => undefined;
  const channel = new BroadcastChannel(channelName(account));
  channel.onmessage = event => {
    const news = event.data as { type?: unknown } | null;
    if (news?.type === CHANGED) refreshConversationsSoon(queryClient);
    else if (news?.type === READ) {
      const read = news as ReadNews;
      if (typeof read.address !== "string" || typeof read.unread_count !== "number") return;
      queryClient.setQueryData<ConversationPages>(chatKeys.conversations(), data => applyReadToSummaries(data, read.address, read));
    }
  };
  return () => channel.close();
}
