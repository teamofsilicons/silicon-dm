"use client";

/**
 * The conversation list with everything around it (spec §6.3): the mark and wordmark, "New", the search field (/
 * focuses it), invitations when there are any, the conversations (newest activity first, more as you scroll), and the
 * account at the bottom with "Viewing as" for custodians. The sidebar shows it on wide and medium screens; on phones
 * the inbox page shows it as the page itself (`variant="page"`).
 *
 * A search looks through every conversation: the rest of the list loads page by page while it is typed (the DM service
 * has no search of its own), and what it found is said to screen readers ("3 conversations", or none) by a status line
 * that is always there, so the words are heard when they change.
 */
import Link from "next/link";
import { useParams } from "next/navigation";
import { useEffect, useMemo, useRef, useState, useSyncExternalStore, type KeyboardEvent } from "react";
import { Eye, Mail } from "lucide-react";
import { SearchField } from "@/components/silicon-ui/search-field/search-field";
import { Wordmark, BRAND_NAME } from "@/components/foundation/brand/app-mark";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { Button } from "@/components/silicon-ui/button/button";
import { accountName, conversationTitle, decodeAddress, inboxHref, otherParticipants } from "@/lib/chat/address";
import { localDraftText, subscribeDrafts } from "@/lib/chat/draft-store";
import { SEARCH_CONVERSATIONS_CAP, useConversations, useInvitations, useLoadEverything } from "@/lib/chat/hooks";
import { useHydrated } from "@/lib/chat/hydrated";
import { latestPending, useOutbox } from "@/lib/chat/outbox-store";
import { useParticipantsPresence } from "@/lib/chat/presence";
import { rowPreview } from "@/lib/chat/preview";
import type { ConversationSummary } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { useShortcutKeys } from "@/lib/shortcuts/react";
import { openDialog } from "../dialogs/store";
import { useChat } from "../shell/chat-context";
import { useOnline } from "../thread/use-online";
import { AccountMenu } from "./account-menu";
import { ConversationRow } from "./conversation-row";
import { NewMenu } from "./new-menu";
import { ViewingAs } from "./viewing-as";
import styles from "./sidebar.module.css";

let draftsVersion = 0;
const subscribeDraftsVersion = (listener: () => void) => subscribeDrafts(() => {
  draftsVersion += 1;
  listener();
});

/** Words a search is checked against: the name, every id, and the last message's line. */
function haystack(conversation: ConversationSummary, title: string, preview: string): string {
  return [title, conversation.id, conversation.group?.name, ...conversation.participants.flatMap(account => [account.id, account.display_name]), preview].filter(Boolean).join(" ").toLowerCase();
}

/** '/' pressed where no search field shows (a phone in a conversation): the list's own page focuses its search on arrival. */
let searchFocusWanted = false;

export function requestSearchFocus(): void {
  searchFocusWanted = true;
}

/** The open conversation's address (from the URL), or null on the inbox and settings. */
export function useOpenAddress(): string | null {
  const params = useParams<{ address?: string }>();
  return params?.address ? decodeAddress(params.address) : null;
}

function useMinuteClock(): Date {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(new Date()), 60_000);
    return () => window.clearInterval(timer);
  }, []);
  return now;
}

/**
 * What a search found, for screen readers: mounted from the start and filled a moment after the typing stops, so one
 * sentence is read ("3 conversations", "No conversation matches", "Looking through older conversations"), not one per
 * key (WCAG 4.1.3).
 */
function SearchStatus({ searching, found, looking }: { searching: boolean; found: number; looking: boolean }) {
  const words = !searching ? "" : found ? `${found} ${found === 1 ? "conversation" : "conversations"}${looking ? ", looking through older ones" : ""}` : looking ? "Looking through older conversations" : "No conversation matches";
  const [said, setSaid] = useState("");
  useEffect(() => {
    const timer = window.setTimeout(() => setSaid(words), words ? 500 : 0);
    return () => window.clearTimeout(timer);
  }, [words]);
  return <span className="sr-only" role="status">{said}</span>;
}

export function ConversationsPane({ variant = "sidebar" }: { variant?: "sidebar" | "page" }) {
  const { me, view, viewed, perspective, readOnly } = useChat();
  const list = useConversations(view);
  const invitations = useInvitations(view);
  const openAddress = useOpenAddress();
  const outbox = useOutbox();
  const now = useMinuteClock();
  const hydrated = useHydrated();
  const [search, setSearch] = useState("");
  const listRef = useRef<HTMLUListElement>(null);
  const sentinel = useRef<HTMLLIElement>(null);
  const searchField = useRef<HTMLDivElement>(null);
  useSyncExternalStore(subscribeDraftsVersion, () => draftsVersion, () => 0);
  const newKeys = useShortcutKeys("conversation.new").aria;
  const invitationKeys = useShortcutKeys("go.invitations").aria;

  useEffect(() => {
    if (!searchFocusWanted) return;
    searchFocusWanted = false;
    searchField.current?.querySelector<HTMLInputElement>("[data-conversation-search]")?.focus();
  }, []);

  const open = useMemo(() => list.items.find(item => item.id === openAddress) ?? null, [list.items, openAddress]);
  const openOther = open && !open.group ? otherParticipants(open, perspective) : [];
  const presenceId = openOther.length === 1 ? openOther[0]!.id : "";
  // What the open thread last heard (it polls; the row only reads), with the same rules for activity.
  const presence = { data: useParticipantsPresence(presenceId ? [presenceId] : [], false).get(presenceId) };
  // Offline, what this tab last heard about someone's presence is stale: the row shows none (as the thread does).
  const online = useOnline();

  const searching = search.trim().length > 0;
  const more = useLoadEverything(view, searching);
  const rows = useMemo(() => {
    const words = search.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (!words.length) return list.items;
    return list.items.filter(conversation => {
      const title = conversationTitle(conversation, perspective);
      const text = haystack(conversation, title, rowPreview(conversation.last_message, { me: perspective, group: !!conversation.group }).text);
      return words.every(word => text.includes(word));
    });
  }, [list.items, search, perspective]);

  // More conversations load as the end of the list comes into view.
  const { hasNextPage, isFetchingNextPage, fetchNextPage } = list;
  useEffect(() => {
    const node = sentinel.current;
    if (!node || !hasNextPage) return;
    const observer = new IntersectionObserver(entries => {
      if (entries.some(entry => entry.isIntersecting) && !isFetchingNextPage) void fetchNextPage();
    }, { rootMargin: "240px" });
    observer.observe(node);
    return () => observer.disconnect();
  }, [hasNextPage, isFetchingNextPage, fetchNextPage]);

  // ↑ and ↓ move between rows, Home and End go to the ends; from the search field ↓ enters the list.
  const moveFocus = (event: KeyboardEvent<HTMLElement>) => {
    const links = Array.from(listRef.current?.querySelectorAll<HTMLAnchorElement>("[data-row]") ?? []);
    if (!links.length) return;
    const at = links.indexOf(document.activeElement as HTMLAnchorElement);
    let next = -1;
    if (event.key === "ArrowDown") next = at < 0 ? 0 : Math.min(links.length - 1, at + 1);
    else if (event.key === "ArrowUp") next = at <= 0 ? -2 : at - 1;
    else if (event.key === "Home" && at >= 0) next = 0;
    else if (event.key === "End" && at >= 0) next = links.length - 1;
    if (next === -1 || event.altKey || event.metaKey || event.ctrlKey) return;
    event.preventDefault();
    if (next === -2) listRef.current?.closest("[data-pane]")?.querySelector<HTMLInputElement>("[data-conversation-search]")?.focus();
    else links[next]!.focus();
  };

  const pending = invitations.data?.items.length ?? 0;
  // Only a list that never loaded shows its failure: a refresh that fails (the service restarting, a dropped
  // connection) keeps the conversations on screen, and the sync loop and the next refresh catch up.
  const failure = list.error && !list.data ? ApiError.from(list.error) : null;

  return (
    <div className={styles.pane} data-pane={variant}>
      <div className={styles.head}>
        {variant === "page" ? <h1 className="sr-only">Messages</h1> : null}
        <Link href="/c" className={styles.brand} data-sq="surface" aria-label={`${BRAND_NAME}, your conversations`}>
          <Wordmark size={20} />
        </Link>
        <NewMenu disabledReason={readOnly ? `You are viewing ${view}'s conversations; go back to yours to start one.` : null} />
      </div>

      <div ref={searchField} className={styles.search} role="search" onKeyDown={event => { if (event.key === "ArrowDown") moveFocus(event); }}>
        <SearchField
          label="Search conversations"
          placeholder="Search"
          value={search}
          onValueChange={setSearch}
          data-conversation-search=""
          autoComplete="off"
          spellCheck={false}
          onKeyDown={event => {
            if (event.key === "Escape" && search) {
              event.preventDefault();
              event.stopPropagation();
              setSearch("");
            }
          }}
        />
      </div>

      <SearchStatus searching={searching} found={rows.length} looking={more.looking} />
      {/* Mounted from the start, so switching to a Silicon's view says it is read only (the banner below is not read out). */}
      <span className="sr-only" role="status">{view ? `Viewing ${viewed ? accountName(viewed) : view}'s conversations, read only.` : ""}</span>

      {view ? (
        <div className={styles.viewing}>
          <Eye size={16} strokeWidth={1.75} aria-hidden="true" />
          <span className={styles.viewingText}>
            <span>Viewing {viewed ? accountName(viewed) : view}</span>
            <span className={styles.viewingNote}>Read only. You see what {viewed ? accountName(viewed) : view} sees.</span>
          </span>
          <Link href={inboxHref(null)} className={styles.viewingBack}>Back to yours</Link>
        </div>
      ) : null}

      {pending ? (
        // The count once, in the badge (as a row's unread count); its name is what it shows, then what the count means
        // ("Invitations 2 waiting": the visible words in order, WCAG 2.5.3).
        <button type="button" className={styles.invitations} data-sq="surface" onClick={() => openDialog("invitations")} aria-keyshortcuts={invitationKeys}>
          <Mail size={18} strokeWidth={1.75} aria-hidden="true" />
          <span className={styles.invitationsLabel}>Invitations</span>
          <span className={styles.badge} data-sq="surface">{pending}<span className="sr-only"> waiting</span></span>
        </button>
      ) : null}

      <nav className={styles.listWrap} aria-label={view ? `${view}'s conversations` : "Conversations"}>
        {failure ? (
          <div className={styles.listState}>
            <ErrorAlert error={failure} title="Conversations could not be loaded" action={<Button size="sm" variant="secondary" onClick={() => void list.refetch()}>Try again</Button>} />
          </div>
        ) : list.isPending ? (
          <ul role="list" className={styles.list} aria-busy="true" aria-label="Loading conversations">
            {Array.from({ length: 6 }, (_, index) => <li key={index} className={styles.skeletonRow} aria-hidden="true"><span data-sq="surface" /><span><i /><i /></span></li>)}
          </ul>
        ) : rows.length ? (
          <ul role="list" className={styles.list} ref={listRef} onKeyDown={moveFocus}>
            {rows.map(conversation => (
              <ConversationRow
                key={conversation.id}
                conversation={conversation}
                perspective={perspective}
                view={view}
                current={conversation.id === openAddress}
                pending={view ? undefined : latestPending(outbox, conversation.id)}
                presence={online && conversation.id === openAddress ? presence.data : undefined}
                draft={!hydrated || view || conversation.id === openAddress ? undefined : localDraftText(me.account.uuid ?? me.account.id, conversation.id)}
                now={hydrated ? now : null}
              />
            ))}
            {hasNextPage && !searching ? <li ref={sentinel} className={styles.more} aria-hidden="true">{isFetchingNextPage ? "Loading more…" : ""}</li> : null}
            {searching && more.looking ? <li className={styles.more} aria-hidden="true">Looking through older conversations…</li> : null}
          </ul>
        ) : searching ? (
          <div className={styles.listState}>
            <p className={styles.empty}>{more.looking ? "Looking through older conversations…" : `No conversation matches “${search.trim()}”.`}</p>
            {more.capped ? <p className={styles.empty}>Only the {SEARCH_CONVERSATIONS_CAP} most recent were searched.</p> : null}
            <Button size="sm" variant="ghost" className={styles.quietAction} onClick={() => setSearch("")}>Clear the search</Button>
          </div>
        ) : (
          <div className={styles.listState}>
            <p className={styles.emptyTitle}>{view ? "No conversations yet" : "Nothing here yet"}</p>
            <p className={styles.empty}>{view ? `${view} has not talked with anyone yet.` : "Start a conversation with a Carbon or a Silicon you look after."}</p>
            {view ? null : <Button size="sm" variant="secondary" onClick={() => openDialog("new-conversation")} aria-keyshortcuts={newKeys}>New conversation</Button>}
          </div>
        )}
      </nav>

      {/* data-toast-floor: toasts rise above this bar, never over it (shell/toast-place.tsx). */}
      <div className={styles.foot} data-toast-floor="">
        <AccountMenu />
        {me.custodian_of.length ? <ViewingAs /> : null}
      </div>
    </div>
  );
}
