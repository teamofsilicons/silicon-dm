/**
 * The rows of a thread, from its messages (oldest first): day dividers ("Today", "Yesterday", "Monday, Oct 12"), the
 * "New messages" divider before the first unread, and messages grouped by sender within five minutes (the first and
 * last of a run shape the bubbles and carry the name and the avatar). Pure, so tests/chat-timeline.test.ts pins it.
 *
 * Messages still in the outbox (Waiting, Failed) come after the sent ones, as the newest, in the order they were
 * written; they group with your sent messages like any other.
 */

export interface TimelineEntry<T> {
  /** A stable key: the message code, or the outbox key of a pending send. */
  key: string;
  /** Who sent it (uuid when known, else the id): runs of one sender group together. */
  senderKey: string;
  /** When it was sent (ISO 8601). */
  at: string;
  value: T;
}

export type TimelineRow<T> =
  | { type: "day"; key: string; label: string; date: string }
  | { type: "unread"; key: "unread"; count: number }
  | { type: "entry"; key: string; entry: TimelineEntry<T>; first: boolean; last: boolean };

export interface TimelineOptions {
  /** The key of the first unread entry: the "New messages" divider goes right before it. */
  firstUnreadKey?: string | null;
  /** How many entries the divider stands for (shown in its words). */
  unreadCount?: number;
  /** Messages of one sender closer than this share a group. Default five minutes. */
  groupWindowMs?: number;
  /** "Today" is decided against this time. Default now. */
  now?: Date;
  /** The reader's time zone (tests pass one; the browser's own by default). */
  timeZone?: string;
}

const FIVE_MINUTES = 5 * 60_000;

// One formatter per time zone: making one costs far more than using it, and a long thread asks for every message's day.
const dayFormats = new Map<string, Intl.DateTimeFormat>();

/** The calendar day of a time in a time zone, as YYYY-MM-DD. */
export function dayOf(date: Date, timeZone?: string): string {
  let format = dayFormats.get(timeZone ?? "");
  if (!format) {
    format = new Intl.DateTimeFormat("en-CA", { year: "numeric", month: "2-digit", day: "2-digit", timeZone });
    dayFormats.set(timeZone ?? "", format);
  }
  const parts = format.formatToParts(date);
  const get = (type: string) => parts.find(part => part.type === type)?.value ?? "00";
  return `${get("year")}-${get("month")}-${get("day")}`;
}

/** Days between two YYYY-MM-DD dates (b - a). */
function daysBetween(a: string, b: string): number {
  return Math.round((Date.parse(`${b}T12:00:00Z`) - Date.parse(`${a}T12:00:00Z`)) / 86_400_000);
}

/** "Today", "Yesterday", "Monday, Oct 12", or "Monday, Oct 12, 2025" for another year. */
export function dayLabel(date: Date, options: { now?: Date; timeZone?: string } = {}): string {
  const now = options.now ?? new Date();
  const day = dayOf(date, options.timeZone);
  const today = dayOf(now, options.timeZone);
  const away = daysBetween(day, today);
  if (away === 0) return "Today";
  if (away === 1) return "Yesterday";
  const sameYear = day.slice(0, 4) === today.slice(0, 4);
  return new Intl.DateTimeFormat("en-US", { weekday: "long", month: "short", day: "numeric", year: sameYear ? undefined : "numeric", timeZone: options.timeZone }).format(date);
}

/** The rows of a thread. Entries must come oldest first. */
export function buildTimeline<T>(entries: Array<TimelineEntry<T>>, options: TimelineOptions = {}): Array<TimelineRow<T>> {
  const groupWindow = options.groupWindowMs ?? FIVE_MINUTES;
  const rows: Array<TimelineRow<T>> = [];
  const days = entries.map(entry => dayOf(new Date(entry.at), options.timeZone));
  let lastDay = "";
  for (let index = 0; index < entries.length; index++) {
    const entry = entries[index]!;
    const day = days[index]!;
    if (day !== lastDay) {
      rows.push({ type: "day", key: `day-${day}`, label: dayLabel(new Date(entry.at), options), date: day });
      lastDay = day;
    }
    if (options.firstUnreadKey && entry.key === options.firstUnreadKey) rows.push({ type: "unread", key: "unread", count: options.unreadCount ?? 0 });
    const joins = (other: number) => {
      const neighbour = entries[other];
      if (!neighbour || neighbour.senderKey !== entry.senderKey || days[other] !== day) return false;
      return Math.abs(Date.parse(neighbour.at) - Date.parse(entry.at)) <= groupWindow;
    };
    // The unread divider breaks a run: what is new starts its own group.
    const breaksBefore = !!options.firstUnreadKey && entry.key === options.firstUnreadKey;
    const breaksAfter = !!options.firstUnreadKey && entries[index + 1]?.key === options.firstUnreadKey;
    rows.push({ type: "entry", key: entry.key, entry, first: breaksBefore || !joins(index - 1), last: breaksAfter || !joins(index + 1) });
  }
  return rows;
}
