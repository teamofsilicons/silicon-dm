"use client";

/**
 * TanStack Query hooks for DM's server state. Your own conversations stay current through the sync loop
 * (lib/chat/sync.ts), so they are not polled; a custodian's view of a Silicon is not in your sync feed and is polled
 * instead. Every hook takes the custodian view (`view`, a Silicon's id) or null.
 *
 * Polling asks for the newest page only (useListSafetyNet, and a view's thread in useMessages): TanStack asks for every
 * loaded page again on a refetch, seven requests a minute for an account with 308 conversations, and a custodian's idle
 * view asked for all of its loaded thread pages every 10 s. The list's safety refresh is a timer of its own: TanStack's
 * `refetchInterval` starts over at every cache write, and the sync loop writes the list at every event, so in an active
 * account it never fired and a read on another device was never seen.
 */
import { useInfiniteQuery, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef } from "react";
import { ApiError } from "@/lib/errors";
import { getAllowList, getConversation, getGroup, getMe, listConversations, listInvitations, listMessages } from "./api";
import { mergeListHead, mergeNewestMessages, messagesOldestFirst, summariesOf, type ConversationPages, type MessagePages } from "./cache";
import { useConversationEvents } from "./features";
import { chatKeys } from "./keys";
import type { ConversationSummary, Me, Message, Page } from "./types";

const VIEW_POLL_MS = 10_000;
const SAFETY_REFRESH_MS = 60_000;
/** The whole list is asked for again this often (a conversation gone without an event, an account's new id further down). */
const FULL_REFRESH_MS = 10 * 60_000;
/** A tab back in view after this long asks for the list's first page at once (reads on other devices). */
const BACK_REFRESH_MS = 15_000;
const INVITATIONS_POLL_MS = 15_000;
/** A thread or conversation that failed to load because the service was away is asked for again this often. */
const RETRY_FAILED_MS = 10_000;

/** A failure that passes with time: no connection, the service away or busy. A refusal (403, 404) does not. */
function passes(error: unknown): boolean {
  const failure = ApiError.from(error);
  return (failure.status === 0 && failure.code === "network_error") || failure.status === 408 || failure.status === 429 || failure.status >= 500;
}

/** Asks again while a load failed for a reason that passes ("It keeps trying" is then what happens). */
function retryWhileAway(query: { state: { status: string; error: unknown } }): number | false {
  return query.state.status === "error" && passes(query.state.error) ? RETRY_FAILED_MS : false;
}

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
    // Kept current by the sync loop and useListSafetyNet (the first page only), not by refetching every page.
    refetchOnWindowFocus: false,
    refetchInterval: retryWhileAway,
    staleTime: view ? 5000 : 30_000,
    meta: { toast: false },
  });
  const items = useMemo(() => summariesOf(query.data), [query.data]);
  return { ...query, items };
}

/** The most conversations a search loads to look through (40 pages of 50). */
export const SEARCH_CONVERSATIONS_CAP = 2000;

/**
 * While `active` (a search typed in the sidebar or the palette), loads the rest of the conversation list page by page,
 * so what is searched is every conversation, not the 50 loaded first (the DM service has no search of its own). Says
 * whether it is still looking, and whether it stopped at SEARCH_CONVERSATIONS_CAP.
 */
export function useLoadEverything(view: string | null, active: boolean): { looking: boolean; capped: boolean } {
  const list = useConversations(view);
  const { hasNextPage, isFetchingNextPage, isError, fetchNextPage } = list;
  const loaded = list.items.length;
  const capped = loaded >= SEARCH_CONVERSATIONS_CAP && !!hasNextPage;
  const go = active && !!hasNextPage && !capped && !isError;
  useEffect(() => {
    if (go && !isFetchingNextPage) void fetchNextPage();
  }, [go, isFetchingNextPage, fetchNextPage, loaded]);
  return { looking: go, capped: active && capped };
}

/**
 * Loads more of the conversation list until `found` says it has what it needs or the list ends (at most
 * SEARCH_CONVERSATIONS_CAP rows): the next unread conversation further down, everything unread to mark read.
 */
export async function loadConversationsUntil(list: { items: ConversationSummary[]; hasNextPage: boolean; fetchNextPage: () => Promise<{ data?: ConversationPages; hasNextPage: boolean; isError: boolean }> }, found: (items: ConversationSummary[]) => boolean): Promise<ConversationSummary[]> {
  let items = list.items;
  let more = list.hasNextPage;
  while (more && !found(items) && items.length < SEARCH_CONVERSATIONS_CAP) {
    const result = await list.fetchNextPage();
    const next = summariesOf(result.data);
    // A page that failed, or brought nothing new: stop with what there is.
    if (result.isError || next.length <= items.length) break;
    items = next;
    more = result.hasNextPage;
  }
  return items;
}

/**
 * Asks for the conversation list's first page again and puts it over the loaded pages (cache.ts mergeListHead). Not
 * while the list is being fetched anyway; never throws.
 */
export async function refreshListHead(queryClient: QueryClient, view: string | null): Promise<void> {
  const key = chatKeys.conversations(view);
  if (!queryClient.getQueryData(key) || queryClient.isFetching({ queryKey: key, exact: true })) return;
  try {
    const page = await listConversations({ view });
    queryClient.setQueryData<ConversationPages>(key, current => (current ? mergeListHead(current, page) : current));
  } catch {
    // The next tick, the sync loop or a reload catches up.
  }
}

/** Runs `tick` every `every` ms while the tab shows, and at once when it shows again after BACK_REFRESH_MS away. */
function useVisibleInterval(tick: () => void, every: number, enabled: boolean): void {
  useEffect(() => {
    if (!enabled) return;
    let hiddenAt = 0;
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") tick();
    }, every);
    const onVisibility = () => {
      if (document.visibilityState === "hidden") {
        hiddenAt = Date.now();
        return;
      }
      if (hiddenAt && Date.now() - hiddenAt >= BACK_REFRESH_MS) tick();
      hiddenAt = 0;
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [tick, every, enabled]);
}

/**
 * The list's safety net, run once by the chat shell: your list's first page every minute while the tab shows (and as
 * soon as it shows again after a while away: a read on another device is not in your feed), the whole list every ten
 * minutes; a custodian's view of a Silicon (not in your feed) every 10 s.
 */
export function useListSafetyNet(view: string | null): void {
  const queryClient = useQueryClient();
  const lastFull = useRef(0);
  const own = useCallback(() => {
    const now = Date.now();
    if (!lastFull.current) lastFull.current = now;
    if (now - lastFull.current >= FULL_REFRESH_MS) {
      lastFull.current = now;
      void queryClient.invalidateQueries({ queryKey: chatKeys.conversations(), exact: true });
      return;
    }
    void refreshListHead(queryClient, null);
  }, [queryClient]);
  const viewed = useCallback(() => void refreshListHead(queryClient, view), [queryClient, view]);
  useVisibleInterval(own, SAFETY_REFRESH_MS, true);
  useVisibleInterval(viewed, VIEW_POLL_MS, !!view);
}

/**
 * One conversation: from the loaded list when it is there, otherwise fetched (with the fallbacks in getConversation).
 * Only a row with exactly this address answers for it: a conversation kept for its history is named in its successor's
 * `supersedes`, but it is its own conversation (read only, `superseded_by` set), so it is fetched itself and shows as
 * such, with its own title, the history banner and no composer.
 */
export function useConversation(address: string, view: string | null) {
  // The same list query as the sidebar's (one cache entry, one fetch): a conversation in it needs no request.
  const list = useConversations(view);
  const fromList = useMemo(() => list.items.find(item => item.id === address), [list.items, address]);
  const waitForList = list.isPending && !list.data;
  const single = useQuery({
    queryKey: chatKeys.conversation(address, view),
    queryFn: ({ signal }) => getConversation(address, { view, signal }),
    enabled: !fromList && !waitForList,
    staleTime: 30_000,
    retry: false,
    refetchInterval: retryWhileAway,
    meta: { toast: false },
  });
  return { conversation: fromList ?? single.data, error: fromList ? null : single.error, loading: !fromList && (waitForList || single.isPending) };
}

/** A conversation's messages: pages of 50, newest first; `messages` is every loaded one, oldest first. */
export function useMessages(address: string, view: string | null) {
  const queryClient = useQueryClient();
  const query = useInfiniteQuery({
    queryKey: chatKeys.messages(address, view),
    queryFn: ({ pageParam, signal }) => listMessages(address, { view, cursor: pageParam, signal }),
    initialPageParam: null as string | null,
    getNextPageParam: last => last.next_cursor ?? undefined,
    // Your own conversations change through the sync loop; a custodian's view polls its newest page (below).
    staleTime: view ? 5000 : Infinity,
    refetchOnWindowFocus: false,
    // A thread that could not be loaded while the service was away keeps trying (its words say so).
    refetchInterval: retryWhileAway,
    meta: { toast: false },
  });
  // A custodian's view: the newest page every 10 s while the tab shows (older pages stay as they loaded).
  useEffect(() => {
    if (!view) return;
    const key = chatKeys.messages(address, view);
    const timer = window.setInterval(() => {
      if (document.visibilityState !== "visible" || !queryClient.getQueryData(key) || queryClient.isFetching({ queryKey: key, exact: true })) return;
      void listMessages(address, { view })
        .then(page => queryClient.setQueryData<MessagePages>(key, current => mergeNewestMessages(current, page)))
        .catch(() => undefined);
    }, VIEW_POLL_MS);
    return () => window.clearInterval(timer);
  }, [address, view, queryClient]);
  const messages = useMemo<Message[]>(() => messagesOldestFirst(query.data), [query.data]);
  return { ...query, messages };
}

/**
 * Group invitations waiting for you (or, in a custodian's view, for the Silicon). When the sync feed carries
 * conversation events (DM 0.17, lib/chat/features.ts) a new invitation, or one answered elsewhere, arrives through it
 * at once, and the invitations are only asked for again every minute as a safety net. An older service's feed carries
 * messages only, and a custodian's view is not in your feed: then they are asked for every 15 s, so a new one shows in
 * the sidebar and one answered on another device makes the list refresh (components/chat/shell/chat-runtime.tsx).
 */
export function useInvitations(view: string | null) {
  const events = useConversationEvents();
  return useQuery({
    queryKey: chatKeys.invitations(view),
    queryFn: ({ signal }) => listInvitations({ view, signal }),
    refetchInterval: events && !view ? SAFETY_REFRESH_MS : INVITATIONS_POLL_MS,
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
