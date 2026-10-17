/**
 * The signed-in chat around every page under it: the session is checked here on the server (no session: the hosted
 * sign-in, coming back to this page), the account and the first page of conversations are read from the service so the
 * list paints with the page, then the providers (query client, toasts, squircles, the session keeper) and the chat
 * (components/chat/shell/chat-app.tsx: sidebar, the page, details, ⌘K, the keyboard). It stays mounted while you move
 * between conversations and the settings.
 */
import type { ReactNode } from "react";
import { Providers } from "@/components/foundation/providers";
import { ChatApp } from "@/components/chat/shell/chat-app";
import type { ConversationSummary, Me, Page } from "@/lib/chat/types";
import { requireSession, tryApiFetch } from "@/lib/server/rsc";
import { accountsOrigin } from "@/lib/server/site";

interface Envelope<T> {
  type: string;
  data: T;
}

export default async function WorkspaceLayout({ children }: { children: ReactNode }) {
  const session = await requireSession();
  const [me, conversations] = await Promise.all([
    tryApiFetch<Envelope<Me>>("/api/v1/me"),
    tryApiFetch<Envelope<Page<ConversationSummary>>>("/api/v1/conversations", { query: { limit: 50 } }),
  ]);
  return (
    <Providers session={{ signedIn: true, account: session.account, expiresAt: session.expiresAt }}>
      <ChatApp account={session.account} accountsUrl={accountsOrigin()} initialMe={me.data?.data ?? null} initialConversations={conversations.data?.data ?? null}>
        {children}
      </ChatApp>
    </Providers>
  );
}
