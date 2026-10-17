/**
 * The pure part of the sync loop (lib/chat/sync.ts): which messages a page of events asks the website to fetch, which
 * events it has already applied, and how long to wait after a failure. Events are references, not content
 * (docs/reference/wire-format.md, Updates): one message edited three times and read twice is fetched once.
 */
import type { SyncEvent } from "./types";

export interface MessageRef {
  conversation: string;
  code: string;
  /** What changed: a new, edited or deleted message, and/or its delivered/read state. */
  kinds: Array<SyncEvent["type"]>;
  /** The events this fetch answers (marked applied once it succeeds). */
  eventIds: string[];
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
    if (!event?.event_id || seen.has(event.event_id) || !event.conversation_id || !event.message_id) continue;
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
