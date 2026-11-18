"use client";

/** The people the signed-in Carbon talks to, for the account picker (contacts.ts), from the cached conversation list. */
import { useMemo } from "react";
import { useConversations } from "@/lib/chat/hooks";
import type { AccountRef } from "@/lib/chat/types";
import { useChat } from "../shell/chat-context";
import { useBook, refOf } from "@/lib/chat/contacts";
import { knownAccounts } from "./contacts";

export function useKnownAccounts(): AccountRef[] {
  const { me } = useChat();
  // Your own list, also while viewing as one of your Silicons: you choose people as yourself.
  const { items } = useConversations(null);
  const { items: contactsList, project } = useBook();
  return useMemo(() => {
    const saved = new Set(contactsList.map((c) => c.uuid));
    const contacts = contactsList
      .filter((c) => !c.blocked && c.account.status !== "deleted")
      .sort((a, b) => b.trust - a.trust || a.name.localeCompare(b.name))
      .map((c) => project(refOf(c.account)));
    return [...contacts, ...knownAccounts(items, me).filter((p) => !saved.has(p.uuid ?? p.id))];
  }, [items, me, contactsList, project]);
}
