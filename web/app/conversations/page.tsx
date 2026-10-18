/** The address of the conversation list before DM 0.17: it is /c now (a custodian's view keeps its ?for=). */
import { redirect } from "next/navigation";
import { inboxHref } from "@/lib/chat/address";

export default async function OldConversations({ searchParams }: { searchParams: Promise<{ for?: string }> }) {
  const query = await searchParams;
  redirect(inboxHref(typeof query.for === "string" ? query.for : null));
}
