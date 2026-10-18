"use client";

/**
 * What runs for as long as the chat is open, once per tab: the sync loop (new messages and receipts), this tab's
 * presence lease, the outbox (sends that survive reloads), delivered and read receipts, the unread count in the tab's
 * title, browser notifications when they are on, the conversation news that keeps the list complete (another tab's
 * new conversation or read, an invitation answered on another device), the list's safety refresh, and what this
 * browser keeps of the Carbon's words (another account's leftovers go; signing out removes them). Renders nothing.
 */
import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useRouter } from "next/navigation";
import { whenSignedOut } from "@/lib/client/session";
import { conversationTitle, inboxHref, sameAccount } from "@/lib/chat/address";
import { summariesOf, type ConversationPages } from "@/lib/chat/cache";
import { useConversations, useInvitations, useListSafetyNet } from "@/lib/chat/hooks";
import { chatKeys } from "@/lib/chat/keys";
import { forgetChatData, forgetOtherAccounts } from "@/lib/chat/local-data";
import { listenForConversationNews } from "@/lib/chat/news";
import { maybeNotify, setScreenConversation, trackScreen } from "@/lib/chat/notifications";
import { attachOutbox } from "@/lib/chat/outbox-store";
import { setPresenceConversation, startPresence } from "@/lib/chat/presence";
import { attachReceipts, noteDelivered } from "@/lib/chat/receipts";
import { refreshConversationsSoon, startSync, wakeSync, type GoneConversation } from "@/lib/chat/sync";
import { useUnreadTitle } from "@/lib/chat/title";
import { noteListHead, resetUnreadTotal, useUnreadTotal } from "@/lib/chat/unread-total";
import { notify } from "@/lib/notify";
import type { Message } from "@/lib/chat/types";
import { useOpenAddress } from "../sidebar/conversations-pane";
import { useChat } from "./chat-context";

export function ChatRuntime() {
  const { me, view } = useChat();
  const queryClient = useQueryClient();
  const router = useRouter();
  const openAddress = useOpenAddress();
  const account = me.account;
  const accountKey = account.uuid ?? account.id;

  // The latest values for the sync loop's callback, which lives longer than one render.
  const latest = useRef({ openAddress, router, view });
  useEffect(() => {
    latest.current = { openAddress, router, view };
  });

  // What another account left in this browser goes before anything reads it; signing out removes this one's too.
  useEffect(() => forgetOtherAccounts(accountKey), [accountKey]);
  useEffect(() => whenSignedOut(forgetChatData), []);
  useEffect(() => attachOutbox({ queryClient, me: account }), [queryClient, accountKey]); // eslint-disable-line react-hooks/exhaustive-deps -- the account's key identifies it
  useEffect(() => attachReceipts({ queryClient, me: account }), [queryClient, accountKey]); // eslint-disable-line react-hooks/exhaustive-deps -- as above
  useEffect(() => startPresence(), []);
  // What this tab is doing (typing, recording) is about the conversation on screen; a custodian's view is not yours.
  const screenAddress = view ? null : openAddress;
  useEffect(() => setPresenceConversation(screenAddress), [screenAddress]);
  // Which conversation this tab shows, for the other tabs' notifications.
  useEffect(() => trackScreen(accountKey), [accountKey]);
  useEffect(() => setScreenConversation(screenAddress), [screenAddress]);
  // Anything that reached the service means it is back: a sync loop waiting out its backoff tries again now.
  useEffect(() => queryClient.getQueryCache().subscribe(event => {
    if (event.type === "updated" && event.action.type === "success") wakeSync();
  }), [queryClient]);
  useListSafetyNet(view);
  useEffect(() => {
    const onIncoming = (message: Message, info: { fresh: boolean }) => {
      if (sameAccount(message.sender, account) || message.deleted_at) return;
      if (!message.delivered_at) noteDelivered(message.conversation_id, message["message-id"]);
      if (!info.fresh) return;
      const summary = summariesOf(queryClient.getQueryData<ConversationPages>(chatKeys.conversations())).find(item => item.id === message.conversation_id);
      const title = summary ? conversationTitle(summary, account) : message.sender.display_name || message.sender.id;
      maybeNotify(message, { title, openAddress: latest.current.openAddress, navigate: href => latest.current.router.push(href) });
    };
    // A conversation you are no longer in leaves the screen too when it is open here: its messages can no longer be read
    // or answered. Someone else taking you out is said in words; leaving it yourself (another device, another tab) is not.
    const onConversationGone = ({ id, name, removed }: GoneConversation) => {
      // In a custodian's view the open conversation is the Silicon's, which your own membership does not change.
      if (latest.current.openAddress !== id || latest.current.view) return;
      latest.current.router.push(inboxHref(null));
      if (removed) notify.info(`You are no longer in ${name}`, "One of its owners took you out of the group. An owner can invite you back.");
    };
    return startSync({ queryClient, me: account, onIncoming, onConversationGone });
  }, [queryClient, accountKey]); // eslint-disable-line react-hooks/exhaustive-deps -- as above

  // Another tab of this browser started, joined or left a conversation: refresh the list here too.
  useEffect(() => listenForConversationNews(queryClient, accountKey), [queryClient, accountKey]);

  // An invitation that goes without this tab answering it was accepted (or declined) somewhere else: a group may have
  // joined the list. A service with conversation events says so in the sync feed at once (lib/chat/sync.ts); an older
  // one carries messages only, and then the invitations (asked for every 15 s) are the signal.
  const invitations = useInvitations(null);
  const invited = useRef<Set<string> | null>(null);
  useEffect(() => {
    if (!invitations.data) return;
    const now = new Set(invitations.data.items.map(item => item.conversation_id));
    const before = invited.current;
    invited.current = now;
    if (before && [...before].some(id => !now.has(id))) refreshConversationsSoon(queryClient);
  }, [invitations.data, queryClient]);

  // The service's unread total for every conversation, loaded or not (DM 0.17): from the first page the server
  // rendered, then from each first page fetched (lib/chat/hooks.ts); another account starts over.
  useEffect(() => {
    const list = queryClient.getQueryData<ConversationPages>(chatKeys.conversations());
    if (list?.pages[0]) noteListHead(list.pages[0], summariesOf(list));
    return () => resetUnreadTotal();
  }, [queryClient, accountKey]);
  const { items } = useConversations(null);
  useUnreadTitle(useUnreadTotal(items, account));
  return null;
}
