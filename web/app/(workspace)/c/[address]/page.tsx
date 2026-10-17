/**
 * /c/<address>: one conversation (`c:ada::si:scout`, `g:product-design`, or a UUID for one kept for its history);
 * `#<code>` opens at a message, `?for=si:x` is a custodian's read-only view. Everything it shows comes from the
 * browser's cache and the service (components/chat/thread), so moving between conversations is instant.
 */
import type { Metadata } from "next";
import { ConversationView } from "@/components/chat/thread/conversation-view";
import { decodeAddress } from "@/lib/chat/address";

export async function generateMetadata({ params }: { params: Promise<{ address: string }> }): Promise<Metadata> {
  const { address } = await params;
  return { title: decodeAddress(address) };
}

export default async function ConversationPage({ params }: { params: Promise<{ address: string }> }) {
  const { address } = await params;
  const decoded = decodeAddress(address);
  // A new conversation starts fresh: its reading position, its first unread, its composer state.
  return <ConversationView key={decoded} address={decoded} />;
}
