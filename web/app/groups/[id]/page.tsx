/** A group's page before DM 0.17: the group is a conversation at /c/<g:id> now, its details beside it. */
import { redirect } from "next/navigation";
import { conversationHref, decodeAddress } from "@/lib/chat/address";

export default async function OldGroup({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  redirect(conversationHref(decodeAddress(id)));
}
