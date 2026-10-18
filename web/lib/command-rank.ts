/**
 * The ⌘K palette's order (spec §6.7: conversations "fuzzy by name and id", actions, custodian views). Silicon UI's
 * palette filters by a plain substring and keeps the order it is given, so the first match Enter runs could be any
 * action whose words happen to contain the letters typed: "bo" ran Keyboard shortcuts, "wren" opened the read-only
 * View as si:wren instead of the conversation with Wren. Here a match is ranked by how well it names the entry:
 *
 *   0  the name itself                  "Wren" for Wren
 *   1  another name, exactly            its address; in a direct conversation the other account's id ("si:wren")
 *   2  the start of the name            "bo" for Bo Okafor
 *   3  the start of a word in the name  "okafor" for Bo Okafor, "wren" for View as si:wren
 *   4  the start of another name, or someone in the conversation exactly
 *   5  the start of someone's name or id in it, or of an action's extra words or description ("log" for "log out")
 *   6  inside the name
 *   7  inside anything else, or every word of the query somewhere
 *   8  a conversation's name letters in order, from a word's start ("lnch" for Launch week)
 *
 * On a tie a conversation comes first, then the order the entries were registered in. The groups stay whole and are
 * ordered by their best match, so the rows Enter and the arrow keys walk are the rows as they are drawn.
 *
 * With nothing typed the palette shows what is there to do: the open conversation's actions, a few recent
 * conversations (the one on screen left out), then the rest of the actions. Typing searches every conversation.
 *
 * Pure: tests/command-rank.test.ts.
 */

export interface RankEntry {
  id: string;
  label: string;
  description?: string;
  group?: string;
  keywords?: string[];
  /** Other names the entry answers to as itself: a conversation's address, the other account's id in a direct one. */
  names?: string[];
  /** The people in a conversation (ids and names, without the one looking). */
  members?: string[];
  /** A conversation: it leads a tie, its other names count as names, and the empty palette lists a few of them. */
  conversation?: boolean;
}

export interface RankOptions {
  /** How many conversations the empty palette lists (as "Recent conversations"). */
  recent?: number;
  /** The command id of the conversation on screen, left out of the recent ones. */
  current?: string | null;
}

export const RECENT_GROUP = "Recent conversations";
const CONTEXT_GROUP = "This conversation";
const DEFAULT_RECENT = 5;

/** Lower case without accents, so "jose" finds José and "ZOE" finds Zoë. */
export function normalizeForSearch(text: string): string {
  return text.normalize("NFKD").replace(/\p{M}/gu, "").toLowerCase();
}

const isWordChar = (char: string | undefined) => !!char && /[\p{L}\p{N}]/u.test(char);

/** 0 equal, 1 prefix, 2 at a word's start, 3 inside, null not there. Both sides already normalised. */
function fieldMatch(text: string, query: string): 0 | 1 | 2 | 3 | null {
  if (!text || !query) return null;
  if (text === query) return 0;
  if (text.startsWith(query)) return 1;
  let at = text.indexOf(query);
  if (at < 0) return null;
  while (at >= 0) {
    if (!isWordChar(text[at - 1])) return 2;
    at = text.indexOf(query, at + 1);
  }
  return 3;
}

/** The query's letters in order, the first of them at a word's start (spaces in the query are ignored). */
function inOrder(text: string, query: string): boolean {
  const letters = query.replace(/\s+/g, "");
  if (letters.length < 3) return false;
  for (let start = text.indexOf(letters[0]!); start >= 0; start = text.indexOf(letters[0]!, start + 1)) {
    if (isWordChar(text[start - 1])) continue;
    let at = start + 1;
    let found = 1;
    for (; found < letters.length && at < text.length; at++) if (text[at] === letters[found]) found++;
    if (found === letters.length) return true;
  }
  return false;
}

/** How well `entry` answers `query` (normalised), lower is better; null when it does not. */
export function rankOf(entry: RankEntry, query: string): number | null {
  const normalized = (list: Array<string | undefined> | undefined) => (list ?? []).filter((text): text is string => !!text).map(normalizeForSearch);
  const label = normalizeForSearch(entry.label);
  const names = normalized(entry.names);
  const members = normalized(entry.members);
  const extra = normalized([entry.description, ...(entry.keywords ?? [])]);
  const group = normalizeForSearch(entry.group ?? "");
  let best: number | null = null;
  const take = (rank: number) => {
    if (best === null || rank < best) best = rank;
  };

  const inLabel = fieldMatch(label, query);
  if (inLabel !== null) take([0, 2, 3, 6][inLabel]!);
  for (const name of names) {
    const match = fieldMatch(name, query);
    if (match !== null) take([1, 4, 4, 7][match]!);
  }
  for (const member of members) {
    const match = fieldMatch(member, query);
    if (match !== null) take([4, 5, 5, 7][match]!);
  }
  for (const text of extra) {
    const match = fieldMatch(text, query);
    if (match !== null) take(match === 3 ? 7 : 5);
  }
  if (best === null && fieldMatch(group, query) !== null) take(7);
  if (best === null) {
    // Every word somewhere ("bo design" for the design crit Bo is in).
    const words = query.split(/\s+/).filter(Boolean);
    const all = [label, ...names, ...members, ...extra, group].join(" ");
    if (words.length > 1 && words.every(word => all.includes(word))) take(7);
  }
  if (best === null && entry.conversation && inOrder(label, query)) take(8);
  return best;
}

/** Keeps each group's entries together, groups in the order they first appear (the palette draws them so). */
function groupsTogether<T extends RankEntry>(entries: T[]): T[] {
  const groups = new Map<string, T[]>();
  for (const entry of entries) {
    const key = entry.group ?? "";
    const list = groups.get(key);
    if (list) list.push(entry);
    else groups.set(key, [entry]);
  }
  return [...groups.values()].flat();
}

/**
 * The palette's entries for a query, best first and each group whole. With an empty query: the open conversation's
 * actions, then up to `recent` conversations under "Recent conversations", then the other actions in their order.
 */
export function rankCommands<T extends RankEntry>(entries: T[], query: string, options: RankOptions = {}): T[] {
  const wanted = normalizeForSearch(query.trim());
  if (!wanted) {
    const recent = entries
      .filter(entry => entry.conversation && entry.id !== options.current)
      .slice(0, options.recent ?? DEFAULT_RECENT)
      .map(entry => ({ ...entry, group: RECENT_GROUP }));
    const context = entries.filter(entry => !entry.conversation && entry.group === CONTEXT_GROUP);
    const rest = entries.filter(entry => !entry.conversation && entry.group !== CONTEXT_GROUP);
    return groupsTogether([...context, ...recent, ...rest]);
  }
  const ranked = entries
    .map((entry, index) => ({ entry, index, rank: rankOf(entry, wanted) }))
    .filter((item): item is { entry: T; index: number; rank: number } => item.rank !== null)
    .sort((a, b) => a.rank - b.rank || Number(!!b.entry.conversation) - Number(!!a.entry.conversation) || a.index - b.index);
  return groupsTogether(ranked.map(item => item.entry));
}
