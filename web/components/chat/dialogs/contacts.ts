/**
 * Who a Carbon can pick without typing an id: the people they already talk to (the other participants of their
 * conversations, most recent first) and the Silicons they look after. The account picker (account-picker.tsx) offers
 * these when its field is empty and filters them as the Carbon types a name or an id.
 *
 * Pure (tests/details-contacts.test.ts): the conversation list and /me in, accounts out.
 */
import type { AccountKind, AccountRef, ConversationSummary } from "@/lib/chat/types";
import { parseAccountId } from "@/lib/ids";

/** The key an account is known by: its permanent uuid when the service gave one, else its id. */
export const accountKey = (account: Pick<AccountRef, "id" | "uuid">): string => account.uuid ?? account.id;

/** True when `account` is one of `keys` (uuids or ids, compared case-insensitively for ids). */
export function isListed(account: Pick<AccountRef, "id" | "uuid">, keys: ReadonlySet<string>): boolean {
  return (!!account.uuid && keys.has(account.uuid)) || keys.has(account.id.toLowerCase());
}

/** A set of uuids and lowercase ids, for `isListed`. */
export function keySet(accounts: Array<Pick<AccountRef, "id" | "uuid"> | string>): Set<string> {
  const keys = new Set<string>();
  for (const account of accounts) {
    if (typeof account === "string") keys.add(account.toLowerCase());
    else {
      keys.add(account.id.toLowerCase());
      if (account.uuid) keys.add(account.uuid);
    }
  }
  return keys;
}

/**
 * The accounts this Carbon talks to: every other participant of their conversations in the list's order (newest
 * activity first), then the Silicons they look after that are not there yet. Deleted and older unlinked identities
 * are left out (nobody can start anything new with them).
 */
export function knownAccounts(conversations: ConversationSummary[], me: { account: Pick<AccountRef, "id" | "uuid">; custodian_of: AccountRef[] }): AccountRef[] {
  const seen = keySet([me.account]);
  const out: AccountRef[] = [];
  const take = (account: AccountRef) => {
    if (account.deleted || account.legacy || isListed(account, seen)) return;
    seen.add(account.id.toLowerCase());
    if (account.uuid) seen.add(account.uuid);
    out.push(account);
  };
  for (const conversation of conversations) for (const participant of conversation.participants) take(participant);
  for (const silicon of me.custodian_of) take(silicon);
  return out;
}

/**
 * The accounts that match what was typed: an id that starts with it first, then a name with a word that starts with
 * it, then anything that contains it. Ties keep the list's order (most recent first).
 */
export function matchAccounts(accounts: AccountRef[], query: string, limit = 6): AccountRef[] {
  const text = query.trim().toLowerCase();
  if (!text) return accounts.slice(0, limit);
  const scored: Array<{ account: AccountRef; score: number; index: number }> = [];
  accounts.forEach((account, index) => {
    const id = account.id.toLowerCase();
    const handle = id.replace(/^(c|si):/, "");
    const name = (account.display_name ?? "").toLowerCase();
    let score = 0;
    if (id === text) score = 5;
    else if (id.startsWith(text) || handle.startsWith(text)) score = 4;
    else if (name.startsWith(text)) score = 3;
    else if (name.split(/\s+/).some(word => word.startsWith(text))) score = 2;
    else if (id.includes(text) || name.includes(text)) score = 1;
    if (score) scored.push({ account, score, index });
  });
  return scored.sort((a, b) => b.score - a.score || a.index - b.index).slice(0, limit).map(entry => entry.account);
}

/** The kind an id names (`c:` a Carbon, `si:` a Silicon), for an account known only by what was typed. */
export function kindOfId(id: string): AccountKind {
  return id.startsWith("si:") ? "silicon" : "carbon";
}

/** What typed text means as an account: an exact id (normalised to lowercase), or why it is not one. */
export function typedAccount(text: string): { ok: true; account: AccountRef } | { ok: false; message: string } {
  const check = parseAccountId(text);
  if (!check.ok) return check;
  return { ok: true, account: { type: check.value.kind, id: check.value.id } };
}

/** The chosen ids a refusal names ("No Silicon Accounts account has the id c:bo now."), so their chips can show it too. */
export function idsNamedIn(text: string, accounts: Array<Pick<AccountRef, "id">>): string[] {
  return accounts.map(account => account.id).filter(id => new RegExp(`(^|[^a-z0-9:_-])${id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}($|[^a-z0-9_-])`, "i").test(text));
}
