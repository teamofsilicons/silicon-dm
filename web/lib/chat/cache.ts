/**
 * Pure updates of the cached server state, so a sync event, a send or a receipt changes exactly what it should and
 * nothing is refetched that the website already knows. The React side calls these through `queryClient.setQueryData`
 * (lib/chat/sync.ts, lib/chat/outbox-store.ts, lib/chat/receipts.ts); tests/chat-cache.test.ts pins them down.
 *
 * Shapes: a conversation's messages are an infinite query whose pages come newest first, each page's items newest
 * first (as GET …/messages returns them). The conversation list is an infinite query of summaries, newest activity
 * first.
 */
import type { InfiniteData } from "@tanstack/react-query";
import { compareCodes } from "./codes";
import { sameAccount } from "./address";
import type { AccountRef, ConversationSummary, LastMessageStatus, Message, Page, ReceiptsResult } from "./types";

export type MessagePages = InfiniteData<Page<Message>, unknown>;
export type ConversationPages = InfiniteData<Page<ConversationSummary>, unknown>;

const code = (message: Message) => message["message-id"];
const time = (value: string | null | undefined) => (value ? Date.parse(value) || 0 : 0);

/**
 * The newer knowledge of one message from two snapshots. A known deletion is never undone by a delayed snapshot,
 * content follows the later edit, and receipts never move backwards (a status only ever advances).
 */
export function mergeMessage(current: Message, incoming: Message): Message {
  if (current.deleted_at && !incoming.deleted_at) {
    return { ...current, delivered_at: current.delivered_at ?? incoming.delivered_at, read_at: current.read_at ?? incoming.read_at };
  }
  const contentFromCurrent = !incoming.deleted_at && time(current.updated_at) > time(incoming.updated_at);
  const base = contentFromCurrent ? { ...incoming, ...pickContent(current) } : incoming;
  return { ...base, delivered_at: incoming.delivered_at ?? current.delivered_at, read_at: incoming.read_at ?? current.read_at };
}

function pickContent(message: Message): Partial<Message> {
  const { message: text, attachments, voice_transcript, reply, history, updated_at, attachment_details, voice, gif, speech, metadata } = message;
  return { message: text, attachments, voice_transcript, reply, history, updated_at, attachment_details, voice, gif, speech, metadata };
}

function sameMessage(a: Message, b: Message): boolean {
  return a === b || JSON.stringify(a) === JSON.stringify(b);
}

/** Every loaded message, oldest first, each code once. */
export function messagesOldestFirst(data: MessagePages | undefined): Message[] {
  if (!data) return [];
  const seen = new Set<string>();
  const out: Message[] = [];
  for (const page of data.pages) {
    for (const message of page.items) {
      if (seen.has(code(message))) continue;
      seen.add(code(message));
      out.push(message);
    }
  }
  return out.sort((a, b) => compareCodes(code(a), code(b)));
}

/** The message with this code among the loaded pages, if any. */
export function findMessage(data: MessagePages | undefined, messageCode: string): Message | undefined {
  for (const page of data?.pages ?? []) for (const message of page.items) if (code(message) === messageCode) return message;
  return undefined;
}

/**
 * Puts a message into a conversation's loaded pages: replaces it where it is, or inserts it in order. A message older
 * than everything loaded while older pages remain is left for those pages (it arrives when they load). Returns the
 * same object when nothing changed, so React Query keeps its referential identity.
 */
export function upsertMessage(data: MessagePages | undefined, message: Message): MessagePages | undefined {
  if (!data) return data;
  const pages = data.pages;
  for (let index = 0; index < pages.length; index++) {
    const page = pages[index]!;
    const at = page.items.findIndex(item => code(item) === code(message));
    if (at < 0) continue;
    const merged = mergeMessage(page.items[at]!, message);
    if (sameMessage(merged, page.items[at]!)) return data;
    const items = page.items.slice();
    items[at] = merged;
    return { ...data, pages: pages.map((entry, i) => (i === index ? { ...entry, items } : entry)) };
  }
  // Not loaded yet: find the page whose range it belongs to (pages and items are newest first).
  for (let index = 0; index < pages.length; index++) {
    const page = pages[index]!;
    const oldest = page.items[page.items.length - 1];
    const isLastPage = index === pages.length - 1;
    if (oldest && compareCodes(code(message), code(oldest)) < 0 && !isLastPage) continue;
    if (oldest && compareCodes(code(message), code(oldest)) < 0 && isLastPage && page.next_cursor) return data;
    const items = [...page.items, message].sort((a, b) => compareCodes(code(b), code(a)));
    return { ...data, pages: pages.map((entry, i) => (i === index ? { ...entry, items } : entry)) };
  }
  // No pages at all: start the first one.
  return { ...data, pages: [{ items: [message], next_cursor: null }], pageParams: data.pageParams.length ? data.pageParams : [null] };
}

/** The status a conversation row shows for its last message (`last_message_status`). */
export function statusOf(message: Pick<Message, "read_at" | "delivered_at">): Exclude<LastMessageStatus, null> {
  return message.read_at ? "read" : message.delivered_at ? "delivered" : "sent";
}

/** The newest activity a message stands for (its deletion, its last edit, or its sending). */
const activityAt = (message: Message) => Math.max(time(message.created_at), time(message.updated_at), time(message.deleted_at));

export interface SummaryUpdate {
  data: ConversationPages | undefined;
  /** The conversation was in the loaded list. When false, refetch the list: it is a new conversation. */
  found: boolean;
  /** The message is newer than anything the row knew (it may count as unread, or notify). */
  fresh: boolean;
}

/**
 * A message (new, edited, deleted, or with new receipts) applied to the conversation list: the row's last message and
 * status, its unread count when the message is new and from someone else, and its place (newest activity first).
 */
export function applyMessageToSummaries(data: ConversationPages | undefined, message: Message, me: Pick<AccountRef, "id" | "uuid">): SummaryUpdate {
  if (!data) return { data, found: false, fresh: false };
  let pageIndex = -1;
  let itemIndex = -1;
  data.pages.forEach((page, p) => {
    const i = page.items.findIndex(item => item.id === message.conversation_id);
    if (i >= 0 && pageIndex < 0) {
      pageIndex = p;
      itemIndex = i;
    }
  });
  if (pageIndex < 0) return { data, found: false, fresh: false };
  const summary = data.pages[pageIndex]!.items[itemIndex]!;
  const last = summary.last_message;
  const fresh = !last || compareCodes(code(message), code(last)) > 0;
  const isLast = !!last && code(last) === code(message);
  if (!fresh && !isLast) return { data, found: true, fresh: false };
  const nextLast = isLast && last ? mergeMessage(last, message) : message;
  const fromOther = !sameAccount(message.sender, me);
  const unread = summary.unread_count === undefined ? undefined : summary.unread_count + (fresh && fromOther && !message.deleted_at ? 1 : 0);
  const updatedAt = new Date(Math.max(time(summary.updated_at), activityAt(nextLast))).toISOString();
  const next: ConversationSummary = { ...summary, last_message: nextLast, last_message_status: statusOf(nextLast), updated_at: updatedAt, ...(unread === undefined ? {} : { unread_count: unread }) };
  // Move the row to where its activity now sorts among the first page.
  const pages = data.pages.map(page => ({ ...page, items: page.items.filter(item => item.id !== summary.id) }));
  const first = pages[0]!.items;
  let at = first.findIndex(item => time(item.updated_at) < time(next.updated_at));
  if (at < 0) at = first.length;
  pages[0] = { ...pages[0]!, items: [...first.slice(0, at), next, ...first.slice(at)] };
  return { data: { ...data, pages }, found: true, fresh };
}

/** A conversation's row after a read receipt: the unread count and read position the service answered. */
export function applyReadToSummaries(data: ConversationPages | undefined, address: string, result: Partial<ReceiptsResult> & { unread_count: number }): ConversationPages | undefined {
  if (!data) return data;
  let changed = false;
  const pages = data.pages.map(page => ({
    ...page,
    items: page.items.map(item => {
      if (item.id !== address) return item;
      if (item.unread_count === result.unread_count && (result.last_read_message_id === undefined || item.last_read_message_id === result.last_read_message_id)) return item;
      changed = true;
      return { ...item, unread_count: result.unread_count, ...(result.last_read_message_id !== undefined ? { last_read_message_id: result.last_read_message_id } : {}) };
    }),
  }));
  return changed ? { ...data, pages } : data;
}

/** Every loaded conversation row, in list order, each once. */
export function summariesOf(data: ConversationPages | undefined): ConversationSummary[] {
  const seen = new Set<string>();
  const out: ConversationSummary[] = [];
  for (const page of data?.pages ?? []) for (const item of page.items) if (!seen.has(item.id)) {
    seen.add(item.id);
    out.push(item);
  }
  return out;
}

/**
 * How many messages wait for you in a conversation. A newer service counts them (`unread_count`); an older one does
 * not, so a last message from someone else that nobody has read counts as one.
 */
export function unreadOf(summary: ConversationSummary, me: Pick<AccountRef, "id" | "uuid">): number {
  if (typeof summary.unread_count === "number") return summary.unread_count;
  const last = summary.last_message;
  if (!last || last.deleted_at || sameAccount(last.sender, me)) return 0;
  return last.read_at ? 0 : 1;
}

/** Unread messages across the loaded conversations (the document title's "(n) DM"). */
export function totalUnread(summaries: ConversationSummary[], me: Pick<AccountRef, "id" | "uuid">): number {
  return summaries.reduce((sum, summary) => sum + unreadOf(summary, me), 0);
}

/** Puts this exact snapshot in place of the loaded one (undoing an optimistic change); unloaded messages are left alone. */
export function replaceMessage(data: MessagePages | undefined, message: Message): MessagePages | undefined {
  if (!data) return data;
  let changed = false;
  const pages = data.pages.map(page => {
    const at = page.items.findIndex(item => code(item) === code(message));
    if (at < 0) return page;
    changed = true;
    const items = page.items.slice();
    items[at] = message;
    return { ...page, items };
  });
  return changed ? { ...data, pages } : data;
}
