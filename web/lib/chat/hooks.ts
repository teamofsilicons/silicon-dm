"use client";

/**
 * TanStack Query hooks for DM's server state. Your own conversations stay current through the sync loop
 * (lib/chat/sync.ts), so they are not polled (a slow safety refresh only); a custodian's view of a Silicon is not in
 * your sync feed and is polled instead. Every hook takes the custodian view (`view`, a Silicon's id) or null.
 */
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useMemo } from "react";
import { getAllowList, getConversation, getGroup, getMe, listConversations, listInvitations, listMessages } from "./api";
import { messagesOldestFirst, summariesOf } from "./cache";
import { chatKeys } from "./keys";
import type { ConversationSummary, Me, Message, Page } from "./types";

const VIEW_POLL_MS = 10_000;
const SAFETY_REFRESH_MS = 60_000;
const INVITATIONS_POLL_MS = 15_000;

export function useMe(initial?: Me) {
  return useQuery({ queryKey: chatKeys.me, queryFn: ({ signal }) => getMe(signal), initialData: initial, staleTime: 5 * 60_000 });
}

/** The conversation list (pages of 50, newest activity first). */
export function useConversations(view: string | null, initial?: Page<ConversationSummary>) {
  const query = useInfiniteQuery({
    queryKey: chatKeys.conversations(view),
    queryFn: ({ pageParam, signal }) => listConversations({ view, cursor: pageParam, signal }),
    initialPageParam: null as string | null,
    getNextPageParam: last => last.next_cursor ?? undefined,
    initialData: initial && !view ? { pages: [initial], pageParams: [null] } : undefined,
    refetchInterval: view ? VIEW_POLL_MS : SAFETY_REFRESH_MS,
    staleTime: view ? 5000 : 30_000,
    meta: { toast: false },
  });
  const items = useMemo(() => summariesOf(query.data), [query.data]);
  return { ...query, items };
}

/** One conversation: from the loaded list when it is there, otherwise fetched (with the fallbacks in getConversation). */
export function useConversation(address: string, view: string | null) {
  // The same list query as the sidebar's (one cache entry, one fetch): a conversation in it needs no request.
  const list = useConversations(view);
  const fromList = useMemo(() => list.items.find(item => item.id === address || item.supersedes?.includes(address)), [list.items, address]);
  const waitForList = list.isPending && !list.data;
  const single = useQuery({
    queryKey: chatKeys.conversation(address, view),
    queryFn: ({ signal }) => getConversation(address, { view, signal }),
    enabled: !fromList && !waitForList,
    staleTime: 30_000,
    retry: false,
    meta: { toast: false },
  });
  return { conversation: fromList ?? single.data, error: fromList ? null : single.error, loading: !fromList && (waitForList || single.isPending) };
}

/** A conversation's messages: pages of 50, newest first; `messages` is every loaded one, oldest first. */
export function useMessages(address: string, view: string | null) {
  const query = useInfiniteQuery({
    queryKey: chatKeys.messages(address, view),
    queryFn: ({ pageParam, signal }) => listMessages(address, { view, cursor: pageParam, signal }),
    initialPageParam: null as string | null,
    getNextPageParam: last => last.next_cursor ?? undefined,
    // Your own conversations change through the sync loop; a custodian's view is polled.
    staleTime: view ? 5000 : Infinity,
    refetchOnWindowFocus: !!view,
    refetchInterval: view ? VIEW_POLL_MS : false,
    meta: { toast: false },
  });
  const messages = useMemo<Message[]>(() => messagesOldestFirst(query.data), [query.data]);
  return { ...query, messages };
}

/**
 * Group invitations waiting for you (or, in a custodian's view, for the Silicon). The service's sync feed carries
 * messages only, so invitations are asked for every 15 s while the tab is visible: a new one shows in the sidebar, and
 * one answered on another device makes the list refresh (components/chat/shell/chat-runtime.tsx).
 */
export function useInvitations(view: string | null) {
  return useQuery({
    queryKey: chatKeys.invitations(view),
    queryFn: ({ signal }) => listInvitations({ view, signal }),
    refetchInterval: INVITATIONS_POLL_MS,
    staleTime: 10_000,
    meta: { toast: false },
  });
}

/** A group's details (owners, members, pending invitations), for the details panel. */
export function useGroup(id: string | null, view: string | null) {
  return useQuery({
    queryKey: chatKeys.group(id ?? "", view),
    queryFn: ({ signal }) => getGroup(id!, { view, signal }),
    enabled: !!id,
    staleTime: 30_000,
    meta: { toast: false },
  });
}

export function useAllowList(silicon: string | null) {
  return useQuery({
    queryKey: chatKeys.allowList(silicon ?? ""),
    queryFn: ({ signal }) => getAllowList(silicon!, signal),
    enabled: !!silicon,
    staleTime: 30_000,
    meta: { toast: false },
  });
}
