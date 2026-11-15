import { notFound } from "next/navigation";
import { OpenChatInvitation } from "@/components/chat/dialogs/chat-invitation";

export const metadata = { title: "Chat invitation", robots: { index: false, follow: false } };

export default async function InvitationPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(id)) notFound();
  return <OpenChatInvitation id={id} />;
}
