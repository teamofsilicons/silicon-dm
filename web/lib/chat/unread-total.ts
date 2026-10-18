/**
 * How many messages wait for you in every conversation, loaded or not: the "(n)" of "(n) DM".
 *
 * The list loads 50 conversations at a time, so counting its loaded rows missed unread messages further down (an old
 * conversation someone wrote in again is at the top; one with messages nobody read for weeks is not). A DM service that
 * counts them (0.17: the first page of GET /api/v1/conversations carries `unread: {conversations, messages}`) says the
 * total; this keeps the part of it that is in conversations not loaded (`rest`) and adds the loaded rows as they are
 * now, so the title follows every new message, read and refresh the rows follow, at once:
 *
 *   title = rest + Σ loaded rows,   rest = total − Σ rows the total already covered (counted when it arrived)
 *
 * A later page that loads takes its rows out of `rest` (the total covered them). Each fresh first page starts over. An
 * older service says no total: the title counts the loaded rows, as before. The service counts at most 1,000 (shown as
 * "999+", lib/chat/format.ts), so past that the title is a floor, never more than the truth.
 */
import { useSyncExternalStore } from "react";
import { totalUnread } from "./cache";
import type { AccountRef, ConversationSummary, Page } from "./types";

/** What the first page of your conversations says about all of them (DM 0.17). */
export interface UnreadTotal {
  /** Conversations with at least one unread message. */
  conversations: number;
  /** Unread messages in them, at most 1,000. */
  messages: number;
}

export type ListPage = Page<ConversationSummary> & { unread?: UnreadTotal | null };

let rest: number | null = null;
/** The rows the total already covers, with the count each had when it was counted. */
const counted = new Map<string, number>();
const listeners = new Set<() => void>();

/** A row's unread count as the service gave it (a service that totals also counts every row). */
const countOf = (row: ConversationSummary) => (typeof row.unread_count === "number" && row.unread_count > 0 ? row.unread_count : 0);

function set(next: number | null): void {
  if (next === rest) return;
  rest = next;
  for (const listener of listeners) listener();
}

/**
 * The first page of your list arrived (a fetch, the safety refresh, or the page the server rendered) with the service's
 * total. `known`: every row this tab had loaded besides it (they are in the total as well).
 */
export function noteListHead(page: ListPage, known: ConversationSummary[]): void {
  const total = page.unread?.messages;
  if (typeof total !== "number" || !Number.isFinite(total)) {
    counted.clear();
    set(null);
    return;
  }
  counted.clear();
  for (const row of known) counted.set(row.id, countOf(row));
  for (const row of page.items) counted.set(row.id, countOf(row));
  let covered = 0;
  for (const count of counted.values()) covered += count;
  set(Math.max(0, total - covered));
}

/** A later page of your list arrived: what it brings was in the total, so it leaves the part not loaded. */
export function noteListPage(page: ListPage): void {
  if (rest === null) return;
  let taken = 0;
  for (const row of page.items) {
    if (counted.has(row.id)) continue;
    const count = countOf(row);
    counted.set(row.id, count);
    taken += count;
  }
  if (taken) set(Math.max(0, rest - taken));
}

/** Forgets the total (another account, signing out). */
export function resetUnreadTotal(): void {
  counted.clear();
  set(null);
}

/** The unread messages in conversations not loaded, as the service last said; null without a total. */
export function unreadNotLoaded(): number | null {
  return rest;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Every unread message of yours: the service's total where it gives one, else the loaded rows' (the title's "(n)"). */
export function useUnreadTotal(loaded: ConversationSummary[], me: Pick<AccountRef, "id" | "uuid">): number {
  const notLoaded = useSyncExternalStore(subscribe, unreadNotLoaded, () => null);
  return (notLoaded ?? 0) + totalUnread(loaded, me);
}
