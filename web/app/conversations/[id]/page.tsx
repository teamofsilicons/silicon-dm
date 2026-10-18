/** A conversation's address before DM 0.17: it is /c/<address> now (with its ?for= and #message). */
import { redirect } from "next/navigation";
import { conversationHref, decodeAddress } from "@/lib/chat/address";

export default async function OldConversation({ params, searchParams }: { params: Promise<{ id: string }>; searchParams: Promise<{ for?: string }> }) {
  const [{ id }, query] = await Promise.all([params, searchParams]);
  redirect(conversationHref(decodeAddress(id), { view: typeof query.for === "string" ? query.for : null }));
}
