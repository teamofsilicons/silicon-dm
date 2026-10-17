"use client";

/**
 * The conversation list with everything around it (spec §6.3): the mark and wordmark, "New", the search field (/
 * focuses it), invitations when there are any, the conversations (newest activity first, more as you scroll), and the
 * account at the bottom with "Viewing as" for custodians. The sidebar shows it on wide and medium screens; on phones
 * the inbox page shows it as the page itself (`variant="page"`).
 */
import Link from "next/link";
import { useParams } from "next/navigation";
import { useEffect, useMemo, useRef, useState, useSyncExternalStore, type KeyboardEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import { Eye, Mail } from "lucide-react";
import { SearchField } from "@/components/silicon-ui/search-field/search-field";
import { Wordmark, BRAND_NAME } from "@/components/foundation/brand/app-mark";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { Button } from "@/components/silicon-ui/button/button";
import { accountName, conversationTitle, decodeAddress, inboxHref, otherParticipants } from "@/lib/chat/address";
import { getPresence } from "@/lib/chat/api";
import { localDraftText, subscribeDrafts } from "@/lib/chat/draft-store";
import { useConversations, useInvitations } from "@/lib/chat/hooks";
import { useHydrated } from "@/lib/chat/hydrated";
import { chatKeys } from "@/lib/chat/keys";
import { latestPending, useOutbox } from "@/lib/chat/outbox-store";
import { rowPreview } from "@/lib/chat/preview";
import type { ConversationSummary } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { openDialog } from "../dialogs/store";
import { useChat } from "../shell/chat-context";
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
  useSyncExternalStore(subscribeDraftsVersion, () => draftsVersion, () => 0);

  const open = useMemo(() => list.items.find(item => item.id === openAddress) ?? null, [list.items, openAddress]);
  const openOther = open && !open.group ? otherParticipants(open, perspective) : [];
  const presenceId = openOther.length === 1 ? openOther[0]!.id : "";
  const presence = useQuery({ queryKey: chatKeys.presence(presenceId), queryFn: ({ signal }) => getPresence(presenceId, signal), enabled: false });

  const searching = search.trim().length > 0;
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
  const failure = list.error ? ApiError.from(list.error) : null;

  return (
    <div className={styles.pane} data-pane={variant}>
      <div className={styles.head}>
        {variant === "page" ? <h1 className="sr-only">Messages</h1> : null}
        <Link href="/c" className={styles.brand} data-sq="surface" aria-label={`${BRAND_NAME}, your conversations`}>
          <Wordmark size={20} />
        </Link>
        <NewMenu disabledReason={readOnly ? `You are viewing ${view}'s conversations; go back to yours to start one.` : null} />
      </div>

      <div className={styles.search} role="search" onKeyDown={event => { if (event.key === "ArrowDown") moveFocus(event); }}>
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

      {view ? (
        <div className={styles.viewing} role="status">
          <Eye size={16} strokeWidth={1.75} aria-hidden="true" />
          <span className={styles.viewingText}>
            <span>Viewing {viewed ? accountName(viewed) : view}</span>
            <span className={styles.viewingNote}>Read only. You see what {viewed ? accountName(viewed) : view} sees.</span>
          </span>
          <Link href={inboxHref(null)} className={styles.viewingBack}>Back to yours</Link>
        </div>
      ) : null}

      {pending ? (
        <button type="button" className={styles.invitations} data-sq="surface" onClick={() => openDialog("invitations")} aria-keyshortcuts="G I">
          <Mail size={18} strokeWidth={1.75} aria-hidden="true" />
          <span className={styles.invitationsLabel}>{pending === 1 ? "1 invitation" : `${pending} invitations`}</span>
          <span className={styles.badge} data-sq="surface" aria-hidden="true">{pending}</span>
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
                presence={conversation.id === openAddress ? presence.data : undefined}
                draft={!hydrated || view || conversation.id === openAddress ? undefined : localDraftText(me.account.uuid ?? me.account.id, conversation.id)}
                now={hydrated ? now : null}
              />
            ))}
            {hasNextPage && !searching ? <li ref={sentinel} className={styles.more} aria-hidden="true">{isFetchingNextPage ? "Loading more…" : ""}</li> : null}
          </ul>
        ) : searching ? (
          <div className={styles.listState}>
            <p className={styles.empty}>No conversation matches “{search.trim()}”.</p>
            <Button size="sm" variant="ghost" onClick={() => setSearch("")}>Clear the search</Button>
          </div>
        ) : (
          <div className={styles.listState}>
            <p className={styles.emptyTitle}>{view ? "No conversations yet" : "Nothing here yet"}</p>
            <p className={styles.empty}>{view ? `${view} has not talked with anyone yet.` : "Start a conversation with a Carbon or a Silicon you look after."}</p>
            {view ? null : <Button size="sm" variant="secondary" onClick={() => openDialog("new-conversation")} aria-keyshortcuts="N">New conversation</Button>}
          </div>
        )}
      </nav>

      <div className={styles.foot}>
        <AccountMenu />
        {me.custodian_of.length ? <ViewingAs /> : null}
      </div>
    </div>
  );
}
