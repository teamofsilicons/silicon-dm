/**
 * Conversation addresses as people and Silicons use them, and the website's own addresses for them.
 *
 * A direct conversation is its two accounts' current ids joined by `::` (`c:ada::si:scout`), a group is its id
 * (`g:product-design`), and a conversation kept only for its history is its UUID (docs/wire-format.md). The website
 * shows a conversation at `/c/<address>` with the colons left readable (`/c/c:ada::si:scout`), a message at
 * `/c/<address>#<code>`, and a custodian's read-only view of a Silicon's conversation with `?for=si:x`.
 */
import type { AccountKind, AccountRef, ConversationSummary } from "./types";

/** Encodes an address for a path segment, keeping `:` and `@` readable (both are valid in a path segment). */
export function encodeAddress(address: string): string {
  return encodeURIComponent(address).replace(/%3A/gi, ":").replace(/%40/g, "@");
}

/** The address in a `/c/[address]` segment, as Next hands it over (encoded or not). */
export function decodeAddress(segment: string): string {
  try {
    return decodeURIComponent(segment);
  } catch {
    return segment;
  }
}

/** `/c/<address>`, with the custodian view and a message code when given. */
export function conversationHref(address: string, options: { view?: string | null; code?: string | null } = {}): string {
  const query = options.view ? `?for=${encodeAddress(options.view)}` : "";
  const hash = options.code ? `#${options.code}` : "";
  return `/c/${encodeAddress(address)}${query}${hash}`;
}

/** `/c`, or `/c?for=si:x` for a custodian's view. */
export function inboxHref(view?: string | null): string {
  return view ? `/c?for=${encodeAddress(view)}` : "/c";
}

/** The full link to a message on this site, for "Copy link". */
export function messageUrl(origin: string, address: string, code: string, view?: string | null): string {
  return `${origin}${conversationHref(address, { view, code })}`;
}

export const isGroupAddress = (address: string) => address.startsWith("g:");

/** The two ids of a direct address (`c:ada::si:scout` → [`c:ada`, `si:scout`]), or null for anything else. */
export function directParticipants(address: string): [string, string] | null {
  const parts = address.split("::");
  if (parts.length !== 2 || !parts.every(part => /^(c|si):[a-z0-9_-]+$/.test(part))) return null;
  return [parts[0]!, parts[1]!];
}

/** True when `account` is the same account as `other` (uuid when both have one, else the current id). */
export function sameAccount(account: Pick<AccountRef, "id" | "uuid"> | null | undefined, other: Pick<AccountRef, "id" | "uuid"> | null | undefined): boolean {
  if (!account || !other) return false;
  if (account.uuid && other.uuid) return account.uuid === other.uuid;
  return account.id === other.id;
}

/** Everyone in the conversation but `me`. */
export function otherParticipants(conversation: Pick<ConversationSummary, "participants">, me: Pick<AccountRef, "id" | "uuid">): AccountRef[] {
  const others = conversation.participants.filter(participant => !sameAccount(participant, me));
  return others.length ? others : conversation.participants;
}

/** What an account is called on screen: its display name, else its id. */
export function accountName(account: Pick<AccountRef, "id" | "display_name" | "deleted">): string {
  if (account.deleted) return "Deleted account";
  return account.display_name?.trim() || account.id;
}

/** The first word of an account's name ("Ada" for "Ada Lovelace", "scout" for si:scout). */
export function firstName(account: Pick<AccountRef, "id" | "display_name" | "deleted">): string {
  const name = accountName(account);
  if (name === account.id) return account.id.replace(/^(c|si):/, "");
  return name.split(/\s+/)[0] ?? name;
}

/** A conversation's name: the group's name, or the other people in it. */
export function conversationTitle(conversation: Pick<ConversationSummary, "participants" | "group" | "id">, me: Pick<AccountRef, "id" | "uuid">): string {
  if (conversation.group?.name) return conversation.group.name;
  const others = otherParticipants(conversation, me);
  if (!others.length) return conversation.id;
  return others.map(accountName).join(", ");
}

export type ConversationKind = AccountKind | "group";

/** "group" for a group, else the kind of the one other account (a Silicon or a Carbon). */
export function conversationKind(conversation: Pick<ConversationSummary, "participants" | "group">, me: Pick<AccountRef, "id" | "uuid">): ConversationKind {
  if (conversation.group) return "group";
  const others = otherParticipants(conversation, me);
  if (others.length !== 1) return "group";
  return others[0]!.type;
}

/** The kind in words, as the thread header and the rows say it. */
export function kindWord(kind: ConversationKind): string {
  return kind === "group" ? "Group" : kind === "silicon" ? "Silicon" : "Carbon";
}

/** The ISI a message was routed to (`deliberate@si:cos` → `deliberate`), or null. */
export function isiOf(recipientId: string | null | undefined): string | null {
  if (!recipientId) return null;
  const at = recipientId.indexOf("@");
  if (at <= 0) return null;
  return recipientId.slice(0, at);
}

/** Shell-safe single quoting for the CLI commands the website shows. */
export function shellQuote(value: string): string {
  if (/^[A-Za-z0-9_:@./=-]+$/.test(value)) return value;
  return `'${value.replace(/'/g, `'\\''`)}'`;
}
