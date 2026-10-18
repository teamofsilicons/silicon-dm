/** /c: the inbox. On wide screens the list is in the sidebar and this says what to do; on phones it is the list. */
import type { Metadata } from "next";
import { InboxView } from "@/components/chat/shell/inbox-view";

export const metadata: Metadata = { title: "Messages" };

export default function InboxPage() {
  return <InboxView />;
}
