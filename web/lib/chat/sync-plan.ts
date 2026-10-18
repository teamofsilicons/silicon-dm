/**
 * The pure part of the sync loop (lib/chat/sync.ts): which messages a page of events asks the website to fetch, what
 * its conversation events ask of the list and the invitations, which events it has already applied, and how long to
 * wait after a failure. Events are references, not content (docs/reference/wire-format.md, Updates): one message edited
 * three times and read twice is fetched once; a group renamed and given a member in one page is asked for once.
 */
import type { SyncEvent } from "./types";

type MessageEventType = "message" | "message_status";

export interface MessageRef {
  conversation: string;
  code: string;
  /** What changed: a new, edited or deleted message, and/or its delivered/read state. */
  kinds: MessageEventType[];
  /** The events this fetch answers (marked applied once it succeeds). */
  eventIds: string[];
}

/** What a page's conversation events ask for (`include=conversations`, DM 0.17). */
export interface ConversationPlan {
  /** Conversations you are no longer in (you left, or were removed, as of the page's last word on them): drop them. */
  drop: string[];
  /** Of those, the ones someone else took you out of (`removed`), rather than ones you left. */
  removed: string[];
  /** Conversations that changed (renamed, described, members, owners, or a change this website does not know): ask again. */
  refresh: string[];
  /** A conversation joined the list (created with you, added, an invitation accepted): ask for the list again. */
  list: boolean;
  /** Your invitations changed (a new one, or one answered or withdrawn): ask for them again. */
  invitations: boolean;
  /** The events this plan answers (marked applied once it is carried out). */
  eventIds: string[];
}

const JOINED = new Set(["created", "added", "invitation_accepted"]);
const GONE = new Set(["left", "removed"]);
const INVITATIONS = new Set(["invited", "invitation_accepted", "invitation_declined", "invitation_cancelled"]);
/** Changes that need nothing beyond the invitations (an invitation is not a conversation in your list yet). */
const INVITATION_ONLY = new Set(["invited", "invitation_declined", "invitation_cancelled"]);

/**
 * What a page of conversation events asks for, in the order they happened: the last word on whether you are in a
 * conversation wins (removed, then added back: it stays), and a conversation you are out of is not asked for again.
 */
export function planConversationEvents(events: SyncEvent[], seen: SeenEvents): ConversationPlan {
  const plan: ConversationPlan = { drop: [], removed: [], refresh: [], list: false, invitations: false, eventIds: [] };
  const member = new Map<string, boolean>();
  const lastGone = new Map<string, string>();
  const changed = new Set<string>();
  for (const event of [...events].sort((a, b) => a.sequence - b.sequence)) {
    if (!event?.event_id || event.type !== "conversation" || !event.conversation_id || seen.has(event.event_id)) continue;
    plan.eventIds.push(event.event_id);
    const change = String(event.change ?? "");
    if (INVITATIONS.has(change)) plan.invitations = true;
    if (JOINED.has(change)) {
      member.set(event.conversation_id, true);
      plan.list = true;
    } else if (GONE.has(change)) {
      member.set(event.conversation_id, false);
      lastGone.set(event.conversation_id, change);
    } else if (!INVITATION_ONLY.has(change)) {
      changed.add(event.conversation_id);
    }
  }
  for (const [conversation, inIt] of member) {
    if (inIt) continue;
    plan.drop.push(conversation);
    if (lastGone.get(conversation) === "removed") plan.removed.push(conversation);
  }
  for (const conversation of changed) if (member.get(conversation) !== false) plan.refresh.push(conversation);
  return plan;
}

/** Event ids already applied, the most recent `limit` of them (a page is at most 100, so 2000 spans many pages). */
export class SeenEvents {
  private readonly ids = new Map<string, true>();
  constructor(private readonly limit = 2000) {}

  has(id: string): boolean {
    return this.ids.has(id);
  }

  add(id: string): void {
    if (this.ids.has(id)) this.ids.delete(id);
    this.ids.set(id, true);
    while (this.ids.size > this.limit) {
      const oldest = this.ids.keys().next().value;
      if (oldest === undefined) break;
      this.ids.delete(oldest);
    }
  }

  get size(): number {
    return this.ids.size;
  }
}

/** The messages to fetch for a page of events, each once, in the order they first appear; applied events are skipped. */
export function planEvents(events: SyncEvent[], seen: SeenEvents): MessageRef[] {
  const refs = new Map<string, MessageRef>();
  for (const event of events) {
    if (!event?.event_id || event.type === "conversation" || seen.has(event.event_id) || !event.conversation_id || !event.message_id) continue;
    const key = `${event.conversation_id}\u0000${event.message_id}`;
    const ref = refs.get(key) ?? { conversation: event.conversation_id, code: event.message_id, kinds: [], eventIds: [] };
    if (!ref.kinds.includes(event.type)) ref.kinds.push(event.type);
    ref.eventIds.push(event.event_id);
    refs.set(key, ref);
  }
  return [...refs.values()];
}

/** The wait after `failures` failed polls in a row: 1 s, 2 s, 4 s … up to 30 s, with up to 25 % jitter. */
export function syncDelay(failures: number, random: () => number = Math.random): number {
  const base = Math.min(30_000, 1000 * 2 ** Math.max(0, failures - 1));
  return Math.round(base * (1 + random() * 0.25));
}
