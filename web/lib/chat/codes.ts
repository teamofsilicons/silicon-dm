/**
 * Message codes are conversation-local, lowercase base36 and never reused: `000` … `zzz`, then `1000` and up
 * (docs/wire-format.md). A longer code is always newer, and codes of one length sort as text ('0'-'9' before 'a'-'z'),
 * so ordering needs no parsing. Bundle codes follow the same rule from `001`.
 */

const CODE = /^(?:[0-9a-z]{3}|[1-9a-z][0-9a-z]{3,12})$/;

/** True for a message code such as `00a` or `1000`. */
export function isMessageCode(value: string): boolean {
  return CODE.test(value);
}

/** Negative when `a` is older than `b`, positive when newer, 0 when they are the same message. */
export function compareCodes(a: string, b: string): number {
  if (a.length !== b.length) return a.length - b.length;
  return a < b ? -1 : a > b ? 1 : 0;
}

/** The newer of two codes (either may be missing). */
export function newerCode(a: string | null | undefined, b: string | null | undefined): string | null {
  if (!a) return b ?? null;
  if (!b) return a;
  return compareCodes(a, b) >= 0 ? a : b;
}

/** True when `code` is after `than` (anything is after nothing). */
export function isAfter(code: string, than: string | null | undefined): boolean {
  return !than || compareCodes(code, than) > 0;
}
