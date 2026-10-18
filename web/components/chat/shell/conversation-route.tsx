"use client";

/**
 * /c/<address> as the page renders it: the conversation, at its own address. Every route of the DM service takes other
 * names for a conversation too (an account id for your direct conversation with it, `si:scout`; the two ids in either
 * order, `si:scout::c:ada`), and the docs say any of them opens it here. But everything the website keeps for a
 * conversation is keyed by its address as the service writes it (`c:ada::si:scout`): the sync loop puts new messages
 * there, the outbox's sends land there, receipts and the open row match it. A thread opened under another name never
 * heard of new messages, and a message sent from it vanished once sent. So the page moves to the conversation's own
 * address as soon as the service has said what it is, keeping `?for=` and the `#code` of a message link.
 *
 * A conversation kept for its history keeps its UUID (the service answers it as itself, `superseded_by` set).
 */
import { useRouter } from "next/navigation";
import { useEffect } from "react";
import { conversationHref } from "@/lib/chat/address";
import { useConversation } from "@/lib/chat/hooks";
import { ConversationView } from "../thread/conversation-view";
import { useChat } from "./chat-context";

function CanonicalAddress({ address }: { address: string }) {
  const { view } = useChat();
  const router = useRouter();
  const { conversation } = useConversation(address, view);
  const canonical = conversation && conversation.id && conversation.id !== address ? conversation.id : null;
  useEffect(() => {
    if (!canonical) return;
    router.replace(`${conversationHref(canonical, { view })}${window.location.hash}`, { scroll: false });
  }, [canonical, view, router]);
  return null;
}

export function ConversationRoute({ address }: { address: string }) {
  return (
    <>
      <CanonicalAddress address={address} />
      {/* A new conversation starts fresh: its reading position, its first unread, its composer state. */}
      <ConversationView key={address} address={address} />
    </>
  );
}
