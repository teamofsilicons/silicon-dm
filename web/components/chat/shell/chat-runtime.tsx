"use client";

/**
 * What runs for as long as the chat is open, once per tab: the sync loop (new messages and receipts), this tab's
 * presence lease, the outbox (sends that survive reloads), delivered and read receipts, the unread count in the tab's
 * title, browser notifications when they are on, and the conversation news that keeps the list complete (another tab's
 * new conversation, an invitation answered on another device). Renders nothing.
 */
import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useRouter } from "next/navigation";
import { conversationTitle, sameAccount } from "@/lib/chat/address";
import { summariesOf, totalUnread, type ConversationPages } from "@/lib/chat/cache";
import { useConversations, useInvitations } from "@/lib/chat/hooks";
import { chatKeys } from "@/lib/chat/keys";
import { listenForConversationNews } from "@/lib/chat/news";
import { maybeNotify } from "@/lib/chat/notifications";
import { attachOutbox } from "@/lib/chat/outbox-store";
import { startPresence } from "@/lib/chat/presence";
import { attachReceipts, noteDelivered } from "@/lib/chat/receipts";
import { refreshConversationsSoon, startSync } from "@/lib/chat/sync";
import { useUnreadTitle } from "@/lib/chat/title";
import type { Message } from "@/lib/chat/types";
import { useOpenAddress } from "../sidebar/conversations-pane";
import { useChat } from "./chat-context";

export function ChatRuntime() {
  const { me } = useChat();
  const queryClient = useQueryClient();
  const router = useRouter();
  const openAddress = useOpenAddress();
  const account = me.account;
  const accountKey = account.uuid ?? account.id;

  // The latest values for the sync loop's callback, which lives longer than one render.
  const latest = useRef({ openAddress, router });
  useEffect(() => {
    latest.current = { openAddress, router };
  });

  useEffect(() => attachOutbox({ queryClient, me: account }), [queryClient, accountKey]); // eslint-disable-line react-hooks/exhaustive-deps -- the account's key identifies it
  useEffect(() => attachReceipts({ queryClient, me: account }), [queryClient, accountKey]); // eslint-disable-line react-hooks/exhaustive-deps -- as above
  useEffect(() => startPresence(), []);
  useEffect(() => {
    const onIncoming = (message: Message, info: { fresh: boolean }) => {
      if (sameAccount(message.sender, account) || message.deleted_at) return;
      if (!message.delivered_at) noteDelivered(message.conversation_id, message["message-id"]);
      if (!info.fresh) return;
      const summary = summariesOf(queryClient.getQueryData<ConversationPages>(chatKeys.conversations())).find(item => item.id === message.conversation_id);
      const title = summary ? conversationTitle(summary, account) : message.sender.display_name || message.sender.id;
      maybeNotify(message, { title, openAddress: latest.current.openAddress, navigate: href => latest.current.router.push(href) });
    };
    return startSync({ queryClient, me: account, onIncoming });
  }, [queryClient, accountKey]); // eslint-disable-line react-hooks/exhaustive-deps -- as above

  // Another tab of this browser started, joined or left a conversation: refresh the list here too.
  useEffect(() => listenForConversationNews(queryClient, accountKey), [queryClient, accountKey]);

  // An invitation that goes without this tab answering it was accepted (or declined) somewhere else: a group may have
  // joined the list. The sync feed carries messages only, so the invitations (asked for every 15 s) are the signal.
  const invitations = useInvitations(null);
  const invited = useRef<Set<string> | null>(null);
  useEffect(() => {
    if (!invitations.data) return;
    const now = new Set(invitations.data.items.map(item => item.conversation_id));
    const before = invited.current;
    invited.current = now;
    if (before && [...before].some(id => !now.has(id))) refreshConversationsSoon(queryClient);
  }, [invitations.data, queryClient]);

  const { items } = useConversations(null);
  useUnreadTitle(totalUnread(items, account));
  return null;
}
