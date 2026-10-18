/**
 * /c/<address>: one conversation (`c:ada::si:scout`, `g:product-design`, or a UUID for one kept for its history);
 * `#<code>` opens at a message, `?for=si:x` is a custodian's read-only view. Everything it shows comes from the
 * browser's cache and the service (components/chat/thread), so moving between conversations is instant. Any other name
 * the service takes for a conversation (`si:scout`, `si:scout::c:ada`) moves to its own address
 * (components/chat/shell/conversation-route.tsx).
 */
import type { Metadata } from "next";
import { ConversationRoute } from "@/components/chat/shell/conversation-route";
import { decodeAddress } from "@/lib/chat/address";

export async function generateMetadata({ params }: { params: Promise<{ address: string }> }): Promise<Metadata> {
  const { address } = await params;
  return { title: decodeAddress(address) };
}

export default async function ConversationPage({ params }: { params: Promise<{ address: string }> }) {
  const { address } = await params;
  // Another name for it (`si:scout` for your conversation with Scout) moves to the conversation's own address there.
  return <ConversationRoute address={decodeAddress(address)} />;
}
