/**
 * Times, durations and sizes the way the chat shows them. A fixed locale (en-US) so everyone reads the same words;
 * the time zone is the reader's own (tests pass one).
 */

const LOCALE = "en-US";

function sameDay(a: Date, b: Date, timeZone?: string): boolean {
  const format = new Intl.DateTimeFormat("en-CA", { year: "numeric", month: "2-digit", day: "2-digit", timeZone });
  return format.format(a) === format.format(b);
}

/** A row's time: "14:05" today, "Yesterday", "Mon" this week, "Oct 12" this year, "Oct 12, 2025" before. */
export function rowTime(value: string | number | Date | null | undefined, now: Date = new Date(), timeZone?: string): string {
  if (value === null || value === undefined) return "";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  if (sameDay(date, now, timeZone)) return clockTime(date, timeZone);
  const yesterday = new Date(now.getTime() - 86_400_000);
  if (sameDay(date, yesterday, timeZone)) return "Yesterday";
  const days = (now.getTime() - date.getTime()) / 86_400_000;
  if (days > 0 && days < 6) return new Intl.DateTimeFormat(LOCALE, { weekday: "short", timeZone }).format(date);
  const sameYear = new Intl.DateTimeFormat(LOCALE, { year: "numeric", timeZone }).format(date) === new Intl.DateTimeFormat(LOCALE, { year: "numeric", timeZone }).format(now);
  return new Intl.DateTimeFormat(LOCALE, { month: "short", day: "numeric", year: sameYear ? undefined : "numeric", timeZone }).format(date);
}

/** "14:05", 24-hour, as the rest of the family's sites write times. */
export function clockTime(value: string | number | Date, timeZone?: string): string {
  return new Intl.DateTimeFormat(LOCALE, { hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZone }).format(new Date(value));
}

/** "Oct 17, 2026, 14:05" for tooltips and details. */
export function fullTime(value: string | number | Date, timeZone?: string): string {
  return new Intl.DateTimeFormat(LOCALE, { month: "short", day: "numeric", year: "numeric", hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZone }).format(new Date(value));
}

/** A length of audio as a clock: "0:07", "1:05", "1:02:09". */
export function clockDuration(milliseconds: number | null | undefined): string {
  if (milliseconds === null || milliseconds === undefined || !Number.isFinite(milliseconds) || milliseconds < 0) return "0:00";
  const total = Math.round(milliseconds / 1000);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  const two = (value: number) => String(value).padStart(2, "0");
  return hours ? `${hours}:${two(minutes)}:${two(seconds)}` : `${minutes}:${two(seconds)}`;
}

/** "last seen 5 minutes ago", "last seen yesterday at 14:05", "last seen Oct 12". */
export function lastSeen(value: string | null | undefined, now: Date = new Date(), timeZone?: string): string {
  if (!value) return "offline";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "offline";
  const minutes = Math.round((now.getTime() - date.getTime()) / 60_000);
  if (minutes < 1) return "last seen just now";
  if (minutes < 60) return `last seen ${minutes} minute${minutes === 1 ? "" : "s"} ago`;
  if (sameDay(date, now, timeZone)) return `last seen today at ${clockTime(date, timeZone)}`;
  if (sameDay(date, new Date(now.getTime() - 86_400_000), timeZone)) return `last seen yesterday at ${clockTime(date, timeZone)}`;
  return `last seen ${new Intl.DateTimeFormat(LOCALE, { month: "short", day: "numeric", timeZone }).format(date)}`;
}

/**
 * The most an unread count shows. The DM service stops counting a conversation's unread messages at 1,000 (counting is
 * a scan, and a Silicon that never marks anything read would otherwise pay for its whole history on every list), so
 * from 1,000 on a count is "999+": it means "a thousand or more", never an exact number.
 */
export const UNREAD_SHOWN_MAX = 999;

/** An unread count as a badge, a divider or the tab's title shows it: "7", "999+" from 1,000 on. */
export function unreadBadge(count: number): string {
  return count > UNREAD_SHOWN_MAX ? `${UNREAD_SHOWN_MAX}+` : String(Math.max(0, Math.round(count)));
}

/** 1536 → "1.5 KB". */
export function fileSize(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) return "";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value < 10 ? value.toFixed(1).replace(/\.0$/, "") : Math.round(value)} ${units[unit]}`;
}
