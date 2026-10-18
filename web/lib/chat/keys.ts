/**
 * TanStack Query keys for DM's server state. Every key starts with "dm" so one invalidation refreshes everything (a
 * sync reset), and every key that depends on a custodian's view carries it ("" for your own), so your conversations
 * and a Silicon's never share a cache entry.
 */
const viewKey = (view: string | null | undefined) => view ?? "";

export const chatKeys = {
  all: ["dm"] as const,
  me: ["dm", "me"] as const,
  conversations: (view?: string | null) => ["dm", "conversations", viewKey(view)] as const,
  conversation: (address: string, view?: string | null) => ["dm", "conversation", address, viewKey(view)] as const,
  /** An infinite query: pages newest first, each page's items newest first (as the service returns them). */
  messages: (address: string, view?: string | null) => ["dm", "messages", address, viewKey(view)] as const,
  bundle: (address: string, bundle: string, view?: string | null) => ["dm", "bundle", address, bundle, viewKey(view)] as const,
  draft: (address: string) => ["dm", "draft", address] as const,
  /** An account's presence; its activity is about `conversation` when the service says activity per conversation. */
  presence: (account: string, conversation?: string | null) => ["dm", "presence", account, conversation ?? ""] as const,
  invitations: (view?: string | null) => ["dm", "invitations", viewKey(view)] as const,
  group: (id: string, view?: string | null) => ["dm", "group", id, viewKey(view)] as const,
  allowList: (silicon: string) => ["dm", "allow-list", silicon] as const,
  gifs: (kind: "trending" | "recent" | "search", query = "") => ["dm", "gifs", kind, query] as const,
  voices: ["dm", "voices"] as const,
  voice: (account: string) => ["dm", "voice", account] as const,
};
