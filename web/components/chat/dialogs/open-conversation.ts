"use client";

/**
 * Opening a conversation from a dialog or the details panel, as fast as it can be: the service's answer goes straight
 * into the cache (the thread paints from it, no second request), the list refreshes behind it, and once the
 * conversation is on screen its message field has focus, so the next key is the first word.
 */
import { useRouter } from "next/navigation";
import { useCallback } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { conversationHref, decodeAddress } from "@/lib/chat/address";
import { createConversation } from "@/lib/chat/api";
import { chatKeys } from "@/lib/chat/keys";
import type { ConversationSummary } from "@/lib/chat/types";

/** Focuses the message field of the conversation at `href` once it is there (gives up after a few seconds). */
export function focusComposerWhenOpen(href: string): void {
  if (typeof window === "undefined") return;
  const path = decodeAddress(href.split(/[?#]/)[0] ?? href);
  const until = Date.now() + 4000;
  const tick = () => {
    const here = decodeAddress(window.location.pathname) === path;
    const field = here ? document.querySelector<HTMLTextAreaElement>("main textarea[data-composer-field]") : null;
    if (field && !field.disabled) {
      field.focus({ preventScroll: true });
      return;
    }
    if (Date.now() < until) window.setTimeout(tick, 60);
  };
  window.setTimeout(tick, 0);
}

/** Puts a conversation the service just returned into the cache and opens it. */
export function useOpenConversation(): (conversation: ConversationSummary) => void {
  const router = useRouter();
  const queryClient = useQueryClient();
  return useCallback((conversation: ConversationSummary) => {
    if (Array.isArray(conversation.participants)) queryClient.setQueryData(chatKeys.conversation(conversation.id, null), conversation);
    void queryClient.invalidateQueries({ queryKey: chatKeys.conversations(null) });
    const href = conversationHref(conversation.id);
    router.push(href);
    focusComposerWhenOpen(href);
  }, [queryClient, router]);
}

/** Starts (or finds) the conversation with these accounts and opens it; throws the service's error. */
export function useStartConversation(): (accountIds: string[], idempotencyKey?: string) => Promise<ConversationSummary> {
  const open = useOpenConversation();
  return useCallback(async (accountIds: string[], idempotencyKey?: string) => {
    const conversation = await createConversation(accountIds, idempotencyKey ?? crypto.randomUUID());
    open(conversation);
    return conversation;
  }, [open]);
}
