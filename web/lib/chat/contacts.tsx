"use client";

import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { dm } from "./api";
import type { AccountRef, ConversationSummary, GroupDetails, Message } from "./types";
import { parseAccountId } from "../ids";

export interface Profile {
  uuid: string;
  id: string;
  kind: "carbon" | "silicon";
  display_name?: string;
  pfp_url?: string;
  status?: string;
}
export interface ContactFields {
  name: string;
  note: string;
  trust: number;
  photo_id: string | null;
  email: string;
  phone: string;
  birthday_month: number | null;
  birthday_day: number | null;
  birthday_year: number | null;
}
export interface Contact extends ContactFields {
  uuid: string;
  account: Profile;
  blocked: boolean;
  muted: boolean;
}
export interface ChatRequest {
  id: string;
  sender: AccountRef;
  created_at: string;
}
const empty: Contact[] = [];
export const contactsKey = (owner: string) => ["dm", "contacts", owner] as const;
export const requestsKey = (owner: string) => ["dm", "chat-requests", owner] as const;
export function useRequests(owner: string) {
  return useQuery({
    queryKey: requestsKey(owner),
    queryFn: ({ signal }) => dm<{ items: ChatRequest[] }>("/api/v1/chat-requests", { signal }),
    staleTime: 10_000,
    refetchInterval: 20_000,
    meta: { toast: false },
  });
}
async function loadContacts(signal: AbortSignal) {
  const contacts: Contact[] = [];
  for (;;) {
    const page = await dm<{ items: Contact[]; total: number }>("/api/v1/contacts", {
      query: { limit: 200, offset: contacts.length },
      signal,
    });
    contacts.push(...page.items);
    if (!page.items.length || contacts.length >= page.total) return contacts;
  }
}
export function refOf(p: Profile): AccountRef {
  return {
    uuid: p.uuid,
    id: p.id,
    type: p.kind,
    display_name: p.display_name || undefined,
    pfp_url: p.pfp_url?.startsWith("/api/v1/contact-photos/") ? `/api${p.pfp_url}` : p.pfp_url || undefined,
    deleted: p.status === "deleted",
  };
}
function savedRef(c: Contact): AccountRef {
  return {
    ...refOf(c.account),
    display_name: c.name,
    trust: c.trust,
    pfp_url: c.photo_id
      ? `/api/api/v1/contact-photos/${encodeURIComponent(c.photo_id)}`
      : c.account.pfp_url || undefined,
  };
}
const identity = (a: AccountRef) => a;
const AddressBook = createContext({
  items: empty,
  loading: false,
  error: null as Error | null,
  owner: "",
  project: identity,
});
export const useBook = () => useContext(AddressBook);

export function ContactProvider({ owner, children }: { owner: string; children: ReactNode }) {
  const query = useQuery({
    queryKey: contactsKey(owner),
    queryFn: ({ signal }) => loadContacts(signal),
    staleTime: 20_000,
    refetchInterval: 45_000,
    meta: { toast: false },
  });
  const items = query.data ?? empty;
  const project = useMemo(() => {
    const byId = new Map<string, AccountRef>();
    for (const c of items) {
      const ref = savedRef(c);
      byId.set(c.uuid, ref);
      if (ref.id) byId.set(ref.id, ref);
    }
    return (account: AccountRef) => {
      if (account.deleted) return account;
      const saved = byId.get(account.uuid ?? account.id);
      return saved ? { ...account, ...saved, pfp_url: saved.pfp_url ?? account.pfp_url } : account;
    };
  }, [items]);
  return (
    <AddressBook.Provider value={{ items, project, owner, loading: query.isPending, error: query.error }}>
      {children}
    </AddressBook.Provider>
  );
}

export function labeledMessage(m: Message, project: typeof identity): Message {
  return { ...m, sender: project(m.sender) };
}
export function labeledGroup(g: GroupDetails, project: typeof identity): GroupDetails {
  return {
    ...g,
    owners: g.owners.map(project),
    members: g.members.map(project),
    pending_invitations: g.pending_invitations.map(project),
  };
}
export function labeledChat(c: ConversationSummary, project: typeof identity): ConversationSummary {
  return {
    ...c,
    participants: c.participants.map(project),
    last_message: c.last_message ? labeledMessage(c.last_message, project) : null,
    group:
      c.group &&
      ("members" in c.group ? labeledGroup(c.group, project) : { ...c.group, owners: c.group.owners.map(project) }),
  };
}

/** Resolve complete IDs after typing settles, and keep the selected chip subscribed after Enter. */
export function useExactPerson(text: string, known?: AccountRef) {
  const { owner, project } = useBook();
  const [settled, setSettled] = useState("");
  const normalized = text.trim().toLowerCase();
  useEffect(() => {
    const timer = setTimeout(() => setSettled(normalized), 220);
    return () => clearTimeout(timer);
  }, [normalized]);
  const id = settled === normalized && parseAccountId(settled).ok ? settled : "";
  const query = useQuery({
    queryKey: ["dm", "profile", owner, id],
    queryFn: ({ signal }) => dm<Profile>("/api/v1/people/lookup", { query: { q: id }, signal }),
    enabled: !!id && !known?.trust,
    staleTime: 60_000,
    retry: false,
    meta: { toast: false },
  });
  const publicRef = query.data ? refOf(query.data) : undefined;
  return known?.trust
    ? known
    : publicRef
      ? project({ ...known, ...publicRef, pfp_url: publicRef.pfp_url ?? known?.pfp_url })
      : known;
}
