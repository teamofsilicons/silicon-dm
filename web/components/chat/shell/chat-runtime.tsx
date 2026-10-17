"use client";

/**
 * What runs for as long as the chat is open, once per tab: the sync loop (new messages and receipts), this tab's
 * presence lease, the outbox (sends that survive reloads), delivered and read receipts, the unread count in the tab's
 * title, and browser notifications when they are on. Renders nothing.
 */
import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useRouter } from "next/navigation";
import { conversationTitle, sameAccount } from "@/lib/chat/address";
import { summariesOf, totalUnread, type ConversationPages } from "@/lib/chat/cache";
import { useConversations } from "@/lib/chat/hooks";
import { chatKeys } from "@/lib/chat/keys";
import { maybeNotify } from "@/lib/chat/notifications";
import { attachOutbox } from "@/lib/chat/outbox-store";
import { startPresence } from "@/lib/chat/presence";
import { attachReceipts, noteDelivered } from "@/lib/chat/receipts";
import { startSync } from "@/lib/chat/sync";
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

  const { items } = useConversations(null);
  useUnreadTitle(totalUnread(items, account));
  return null;
}
