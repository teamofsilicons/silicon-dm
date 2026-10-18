"use client";

/** The people the signed-in Carbon talks to, for the account picker (contacts.ts), from the cached conversation list. */
import { useMemo } from "react";
import { useConversations } from "@/lib/chat/hooks";
import type { AccountRef } from "@/lib/chat/types";
import { useChat } from "../shell/chat-context";
import { knownAccounts } from "./contacts";

export function useKnownAccounts(): AccountRef[] {
  const { me } = useChat();
  // Your own list, also while viewing as one of your Silicons: you choose people as yourself.
  const { items } = useConversations(null);
  return useMemo(() => knownAccounts(items, me), [items, me]);
}
