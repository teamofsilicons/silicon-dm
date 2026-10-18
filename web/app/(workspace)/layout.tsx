/**
 * The signed-in chat around every page under it: the session is checked here on the server (no session: the hosted
 * sign-in, coming back to this page), the account, the first page of conversations and the invitations are read from
 * the service so the list paints with the page, where it stays, then the providers (query client, toasts, squircles, the session keeper) and the chat
 * (components/chat/shell/chat-app.tsx: sidebar, the page, details, ⌘K, the keyboard). It stays mounted while you move
 * between conversations and the settings.
 */
import type { ReactNode } from "react";
import { Providers } from "@/components/foundation/providers";
import { ChatApp } from "@/components/chat/shell/chat-app";
import type { ConversationSummary, Invitation, Me, Page } from "@/lib/chat/types";
import { requireSession, tryApiFetch } from "@/lib/server/rsc";
import { accountsOrigin } from "@/lib/server/site";

interface Envelope<T> {
  type: string;
  data: T;
}

export default async function WorkspaceLayout({ children }: { children: ReactNode }) {
  const session = await requireSession();
  // The invitations too: their line sits above the list, and arriving after it moved every row down under a tap.
  const [me, conversations, invitations] = await Promise.all([
    tryApiFetch<Envelope<Me>>("/api/v1/me"),
    tryApiFetch<Envelope<Page<ConversationSummary>>>("/api/v1/conversations", { query: { limit: 50 } }),
    tryApiFetch<Envelope<{ items: Invitation[] }>>("/api/v1/invitations"),
  ]);
  return (
    <Providers session={{ signedIn: true, account: session.account, expiresAt: session.expiresAt }}>
      <ChatApp account={session.account} accountsUrl={accountsOrigin()} initialMe={me.data?.data ?? null} initialConversations={conversations.data?.data ?? null} initialInvitations={invitations.data?.data ?? null}>
        {children}
      </ChatApp>
    </Providers>
  );
}
