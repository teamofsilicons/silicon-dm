"use client";

/**
 * The signed-in chat, mounted once by the workspace layout and kept while you move between conversations and the
 * settings: who you are (ChatProvider), the keyboard (ShortcutsProvider), the details panel's place (DetailsProvider),
 * what runs in the background (ChatRuntime: sync, presence, outbox, receipts, title, notifications), the global
 * shortcuts and the palette's entries, the frame (sidebar | page | details), the chat's dialogs, and where toasts sit
 * (ToastPlace: never over the composer).
 */
import { usePathname, useRouter, useSearchParams } from "next/navigation";
import { useEffect, useState, type ReactNode } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { CommandMenu } from "@/components/foundation/shell/command-menu";
import type { SessionAccount } from "@/lib/account";
import { chatKeys } from "@/lib/chat/keys";
import type { ConversationSummary, Me, Page } from "@/lib/chat/types";
import { ShortcutsProvider } from "@/lib/shortcuts/react";
import { ChatDialogs } from "../dialogs/chat-dialogs";
import { openDialog } from "../dialogs/store";
import { ConversationsPane } from "../sidebar/conversations-pane";
import { ChatProvider } from "./chat-context";
import { ChatRuntime } from "./chat-runtime";
import { DetailsHost, DetailsProvider } from "./details";
import { GlobalCommands } from "./global-commands";
import { ToastPlace } from "./toast-place";
import styles from "./shell.module.css";

export interface ChatAppProps {
  account: SessionAccount;
  accountsUrl: string;
  /** GET /api/v1/me as the server read it (null when the service could not answer). */
  initialMe: Me | null;
  /** The first page of your conversations as the server read it, so the list paints with the page. */
  initialConversations: Page<ConversationSummary> | null;
  children: ReactNode;
}

/** `/c?new=1` and `/c?new=group` open the dialogs, so they have addresses of their own. */
function DialogFromAddress() {
  const params = useSearchParams();
  const router = useRouter();
  const pathname = usePathname();
  const wanted = params.get("new");
  useEffect(() => {
    if (!wanted) return;
    openDialog(wanted === "group" ? "new-group" : "new-conversation");
    const rest = new URLSearchParams(params);
    rest.delete("new");
    router.replace(`${pathname}${rest.size ? `?${rest}` : ""}`, { scroll: false });
  }, [wanted, params, pathname, router]);
  return null;
}

function routeOf(pathname: string): "inbox" | "thread" | "settings" {
  if (pathname.startsWith("/settings")) return "settings";
  return pathname === "/c" || pathname === "/c/" ? "inbox" : "thread";
}

function ChatFrame({ children }: { children: ReactNode }) {
  const pathname = usePathname();
  return (
    <div className={styles.app} data-route={routeOf(pathname)}>
      <a className="skip-link" data-sq="surface" href="#main">Skip to content</a>
      <div className={styles.sidebar}>
        <ConversationsPane />
      </div>
      <main id="main" className={styles.main} tabIndex={-1}>
        {children}
      </main>
      <DetailsHost />
    </div>
  );
}

export function ChatApp({ account, accountsUrl, initialMe, initialConversations, children }: ChatAppProps) {
  const queryClient = useQueryClient();
  // Seed the caches once, before anything reads them, with what the server already fetched.
  useState(() => {
    if (initialMe && !queryClient.getQueryData(chatKeys.me)) queryClient.setQueryData(chatKeys.me, initialMe);
    if (initialConversations && !queryClient.getQueryData(chatKeys.conversations())) queryClient.setQueryData(chatKeys.conversations(), { pages: [initialConversations], pageParams: [null] });
    return true;
  });
  return (
    <ChatProvider account={account} accountsUrl={accountsUrl} initialMe={initialMe}>
      <ShortcutsProvider>
        <DetailsProvider>
          <ChatRuntime />
          <GlobalCommands />
          <ChatFrame>{children}</ChatFrame>
          <CommandMenu />
          <ChatDialogs />
          <DialogFromAddress />
          <ToastPlace />
        </DetailsProvider>
      </ShortcutsProvider>
    </ChatProvider>
  );
}
