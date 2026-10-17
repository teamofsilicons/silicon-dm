"use client";

/**
 * Who is using the website and how: the signed-in account (from the session and /api/v1/me), and the custodian view
 * (`?for=si:x`: a Carbon reading one of their Silicons' conversations, read only). Every chat component reads it with
 * useChat(). In a view, "mine" means the viewed Silicon's: its messages sit on the right, as it sees them.
 */
import { createContext, useContext, useMemo, type ReactNode } from "react";
import { useSearchParams } from "next/navigation";
import type { SessionAccount } from "@/lib/account";
import { useMe } from "@/lib/chat/hooks";
import type { AccountRef, Me } from "@/lib/chat/types";

export interface ChatContextValue {
  me: Me;
  /** The session's account (photo and name as Silicon Accounts has them). */
  account: SessionAccount;
  accountsUrl: string;
  /** The Silicon whose conversations are shown read only (`?for=`), or null for your own. */
  view: string | null;
  viewed: AccountRef | null;
  /** Whose messages are "mine" on screen: the viewed Silicon, or you. */
  perspective: AccountRef;
  readOnly: boolean;
}

const ChatContext = createContext<ChatContextValue | null>(null);

/** /api/v1/me as far as the session knows it, until the service answers (or when it could not be reached). */
export function meFromSession(account: SessionAccount): Me {
  return {
    account: { type: account.kind, id: account.id, uuid: account.uuid, display_name: account.display_name || undefined, pfp_url: account.pfp_url ?? undefined },
    custodian: null,
    custodian_of: [],
    scopes: [],
    via_app: null,
    delivery: { transport: "sync", ting_enabled: false, sync_path: "/api/v1/sync", sync_long_poll_seconds: 30 },
  };
}

export function ChatProvider({ account, accountsUrl, initialMe, children }: { account: SessionAccount; accountsUrl: string; initialMe: Me | null; children: ReactNode }) {
  const me = useMe(initialMe ?? undefined).data ?? initialMe ?? meFromSession(account);
  const params = useSearchParams();
  const requested = params.get("for")?.trim() || null;
  const value = useMemo<ChatContextValue>(() => {
    const viewed = requested ? me.custodian_of.find(silicon => silicon.id === requested || silicon.uuid === requested) ?? null : null;
    // A view of a Silicon this account does not look after is still asked of the service, which says why it refuses.
    const view = requested ? viewed?.id ?? requested : null;
    const perspective = viewed ?? (view ? { type: "silicon" as const, id: view } : me.account);
    return { me, account, accountsUrl, view, viewed, perspective, readOnly: !!view };
  }, [me, account, accountsUrl, requested]);
  return <ChatContext.Provider value={value}>{children}</ChatContext.Provider>;
}

export function useChat(): ChatContextValue {
  const value = useContext(ChatContext);
  if (!value) throw new Error("useChat() needs <ChatProvider> (components/chat/shell/chat-app.tsx) above it.");
  return value;
}
