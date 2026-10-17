/**
 * Typed calls to the DM service from the browser, one function per route the website uses (openapi.yaml, plus the
 * 0.17 additions in the build spec §4). They go through the same-origin API proxy (`request` in lib/client/api.ts:
 * `/api` + the service's own path, so `/api/v1/me` is fetched at `/api/api/v1/me`), unwrap the `{type, data}`
 * envelope, and throw ApiError with the service's code, message and hint.
 *
 * `view` is a custodian's read-only view of one of their Silicons (`?for=si:x`); the service refuses writes in it.
 */
import { ApiError, errorFromResponse, networkError } from "@/lib/errors";
import { request, seg, type QueryValue, type RequestOptions } from "@/lib/client/api";
import { ensureFreshSession, markSignedOut } from "@/lib/client/session";
import type {
  AccountVoice,
  Activity,
  AllowList,
  BundleDetail,
  ConversationSummary,
  Draft,
  DraftInput,
  Gif,
  GroupDetails,
  Invitation,
  Me,
  Message,
  MessageCreate,
  Page,
  Presence,
  PresenceLease,
  ReceiptsResult,
  SyncPage,
  VoiceCatalog,
} from "./types";

/** DM's HTTP contract; the service answers 406 to any other explicit version. */
export const DM_CONTRACT = "4";
const HEADERS = { "x-dm-contract-version": DM_CONTRACT };

interface Envelope<T> {
  type: string;
  data: T;
}

type CallOptions = Omit<RequestOptions, "query"> & { query?: Record<string, QueryValue> };

/** One call to the service, returning the envelope's `data` (null for 204). */
export async function dm<T>(path: string, options: CallOptions = {}): Promise<T> {
  const reply = await request<Envelope<T> | null>(path, { ...options, headers: { ...HEADERS, ...options.headers } });
  return (reply?.data ?? null) as T;
}

const conversationPath = (address: string) => `/api/v1/conversations/${seg(address)}`;
const messagePath = (address: string, code: string) => `${conversationPath(address)}/messages/${seg(code)}`;
const forQuery = (view?: string | null) => (view ? { for: view } : undefined);

/* Accounts ---------------------------------------------------------------------------------------------------------- */

export const getMe = (signal?: AbortSignal) => dm<Me>("/api/v1/me", { signal });

/* Conversations ----------------------------------------------------------------------------------------------------- */

export function listConversations(options: { view?: string | null; cursor?: string | null; limit?: number; signal?: AbortSignal } = {}) {
  return dm<Page<ConversationSummary>>("/api/v1/conversations", {
    query: { limit: options.limit ?? 50, cursor: options.cursor ?? undefined, for: options.view ?? undefined },
    signal: options.signal,
  });
}

/** Starts (or finds) the one conversation of exactly these accounts and you. */
export function createConversation(participantIds: string[], idempotencyKey: string | true = true) {
  return dm<ConversationSummary>("/api/v1/conversations", {
    method: "POST",
    body: { type: "create_conversation", data: { participant_ids: participantIds } },
    idempotencyKey,
  });
}

/**
 * One conversation by address. The service lists conversations and has a GET for groups; a newer one also answers
 * GET /api/v1/conversations/{address} (spec §4.5). This tries that first and falls back to the group route, or to
 * paging the list, so it works against either.
 */
export async function getConversation(address: string, options: { view?: string | null; signal?: AbortSignal; pages?: number } = {}): Promise<ConversationSummary> {
  try {
    const found = await dm<ConversationSummary>(conversationPath(address), { query: forQuery(options.view), signal: options.signal });
    if (found && Array.isArray(found.participants)) return found;
  } catch (error) {
    // 404/405: an older service without the route (or no such conversation); 403 is a real answer.
    if (!ApiError.is(error) || (error.status !== 404 && error.status !== 405)) throw error;
  }
  if (address.startsWith("g:")) return getGroup(address, { view: options.view, signal: options.signal });
  let cursor: string | null = null;
  for (let page = 0; page < (options.pages ?? 10); page++) {
    const result: Page<ConversationSummary> = await listConversations({ view: options.view, cursor, limit: 100, signal: options.signal });
    const match = result.items.find(item => item.id === address || item.supersedes?.includes(address));
    if (match) return match;
    if (!result.next_cursor) break;
    cursor = result.next_cursor;
  }
  throw new ApiError({
    status: 404,
    code: "conversation_not_found",
    message: `No conversation ${address} is visible to this account${options.view ? ` while viewing as ${options.view}` : ""}.`,
    hint: "Check the address (c:ada::si:scout, g:name), or start the conversation with New.",
  });
}

/* Messages ---------------------------------------------------------------------------------------------------------- */

export function listMessages(address: string, options: { view?: string | null; cursor?: string | null; limit?: number; signal?: AbortSignal } = {}) {
  return dm<Page<Message>>(`${conversationPath(address)}/messages`, {
    query: { limit: options.limit ?? 50, cursor: options.cursor ?? undefined, for: options.view ?? undefined },
    signal: options.signal,
  });
}

export const getMessage = (address: string, code: string, options: { view?: string | null; signal?: AbortSignal } = {}) =>
  dm<Message>(messagePath(address, code), { query: forQuery(options.view), signal: options.signal });

/** Sends a message. The same Idempotency-Key on a retry returns the original message instead of a second one. */
export const sendMessage = (address: string, body: MessageCreate, idempotencyKey: string, signal?: AbortSignal) =>
  dm<Message>(`${conversationPath(address)}/messages`, { method: "POST", body: { type: "message.create", data: body }, idempotencyKey, signal });

/** Replaces your message's content (the previous content goes to its history). Send the complete content. */
export const editMessage = (address: string, code: string, body: MessageCreate, idempotencyKey: string | true = true) =>
  dm<Message>(messagePath(address, code), { method: "PATCH", body: { type: "message.updated", data: body }, idempotencyKey });

/** Turns a message into a tombstone (yours, or a Silicon's you look after). */
export const deleteMessage = (address: string, code: string, idempotencyKey: string | true = true) =>
  dm<Message>(messagePath(address, code), { method: "DELETE", idempotencyKey });

/** A delivered or read receipt for one message from this device (receipts never move backwards). */
export const recordReceipt = (address: string, code: string, status: "delivered" | "read", deviceId: string) =>
  dm<Message>(`${messagePath(address, code)}/receipts`, { method: "POST", body: { type: "receipt", data: { status, device_id: deviceId } } });

/**
 * Marks every message from others up to `upTo` (inclusive) read or delivered, in one call (spec §4.5). A service
 * without the route answers 404; the caller falls back to single receipts (lib/chat/receipts.ts).
 */
export const receiptsUpTo = (address: string, upTo: string, status: "read" | "delivered") =>
  dm<ReceiptsResult>(`${conversationPath(address)}/receipts`, { method: "POST", body: { type: status === "read" ? "receipts.read" : "receipts.delivered", data: { up_to: upTo } } });

export const getBundle = (address: string, bundle: string, view?: string | null) =>
  dm<BundleDetail>(`${conversationPath(address)}/bundles/${seg(bundle)}`, { query: forQuery(view) });

/* Drafts ------------------------------------------------------------------------------------------------------------ */

export const getDraft = (address: string, options: { view?: string | null; signal?: AbortSignal } = {}) =>
  dm<Draft>(`${conversationPath(address)}/draft`, { query: forQuery(options.view), signal: options.signal });

/** Saves your draft with compare-and-swap: `version` is the one you last read (0 when there is none yet). */
export const putDraft = (address: string, input: DraftInput, version: number) =>
  dm<Draft>(`${conversationPath(address)}/draft`, { method: "PUT", body: { type: "put_draft", data: input }, headers: { "If-Match": String(version) } });

export const deleteDraft = (address: string) => dm<null>(`${conversationPath(address)}/draft`, { method: "DELETE" });

/* Groups ------------------------------------------------------------------------------------------------------------ */

const groupPath = (id: string) => `/api/v1/groups/${seg(id)}`;

export const getGroup = (id: string, options: { view?: string | null; signal?: AbortSignal } = {}) =>
  dm<ConversationSummary & { group: GroupDetails }>(groupPath(id), { query: forQuery(options.view), signal: options.signal });

export const createGroup = (input: { name: string; description?: string; member_ids?: string[] }, idempotencyKey: string | true = true) =>
  dm<ConversationSummary & { group: GroupDetails }>("/api/v1/groups", { method: "POST", body: { type: "create_group", data: input }, idempotencyKey });

export const updateGroup = (id: string, input: { name: string; description?: string }, version: number) =>
  dm<GroupDetails>(groupPath(id), { method: "PATCH", body: { type: "update_group", data: input }, headers: { "If-Match": String(version) }, idempotencyKey: true });

type MemberChange = "invite_group_members" | "remove_group_members" | "add_group_owners" | "remove_group_owners";
const changeMembers = (id: string, resource: "members" | "owners", method: "POST" | "DELETE", type: MemberChange, memberIds: string[]) =>
  dm<GroupDetails>(`${groupPath(id)}/${resource}`, { method, body: { type, data: { member_ids: memberIds } }, idempotencyKey: true });

export const inviteMembers = (id: string, memberIds: string[]) => changeMembers(id, "members", "POST", "invite_group_members", memberIds);
export const removeMembers = (id: string, memberIds: string[]) => changeMembers(id, "members", "DELETE", "remove_group_members", memberIds);
export const addOwners = (id: string, memberIds: string[]) => changeMembers(id, "owners", "POST", "add_group_owners", memberIds);
export const removeOwners = (id: string, memberIds: string[]) => changeMembers(id, "owners", "DELETE", "remove_group_owners", memberIds);
export const acceptInvitation = (id: string) => dm<ConversationSummary>(`${groupPath(id)}/accept`, { method: "POST" });
export const declineInvitation = (id: string, view?: string | null) => dm<null>(`${groupPath(id)}/decline`, { method: "POST", query: forQuery(view) });
export const leaveGroup = (id: string) => dm<null>(`${groupPath(id)}/leave`, { method: "POST" });

export const listInvitations = (options: { view?: string | null; signal?: AbortSignal } = {}) =>
  dm<{ items: Invitation[] }>("/api/v1/invitations", { query: forQuery(options.view), signal: options.signal });

/* Presence ---------------------------------------------------------------------------------------------------------- */

/** Renews this device's presence lease, with what it is doing right now (or null). */
export const renewPresence = (deviceId: string, activity: Activity | null) =>
  dm<PresenceLease>(`/api/v1/presence/devices/${seg(deviceId)}`, { method: "PUT", body: { type: "renew_presence", data: { activity } } });

/**
 * Closes this device's lease. `keepalive` lets it finish while the page unloads (pagehide); such a request cannot
 * wait for a session refresh, so it goes straight to the proxy.
 */
export function closePresence(deviceId: string, keepalive = false): Promise<unknown> {
  if (keepalive) {
    return fetch(`/api/api/v1/presence/devices/${seg(deviceId)}`, { method: "DELETE", keepalive: true, credentials: "same-origin", headers: HEADERS }).catch(() => undefined);
  }
  return dm<null>(`/api/v1/presence/devices/${seg(deviceId)}`, { method: "DELETE" });
}

export const getPresence = (account: string, signal?: AbortSignal) => dm<Presence>(`/api/v1/presence/${seg(account)}`, { signal });

/* Sync -------------------------------------------------------------------------------------------------------------- */

export const syncPage = (options: { cursor?: string | null; wait?: number; reset?: boolean; limit?: number; signal?: AbortSignal }) =>
  dm<SyncPage>("/api/v1/sync", {
    query: { cursor: options.cursor ?? undefined, wait: options.wait, reset: options.reset ? true : undefined, limit: options.limit },
    signal: options.signal,
  });

/* GIFs -------------------------------------------------------------------------------------------------------------- */

export const trendingGifs = (signal?: AbortSignal) => dm<{ items: Gif[] }>("/api/v1/gifs/trending", { signal });
export const searchGifs = (query: string, signal?: AbortSignal) => dm<{ items: Gif[] }>("/api/v1/gifs/search", { query: { q: query }, signal });
export const recentGifs = (signal?: AbortSignal) => dm<{ items: Gif[] }>("/api/v1/gifs/recent", { signal });

/* Silicons' reach (allow-lists) ------------------------------------------------------------------------------------- */

export const getAllowList = (silicon: string, signal?: AbortSignal) => dm<AllowList>("/api/v1/allow-list", { query: { for: silicon }, signal });
export const allowAccounts = (silicon: string, accountIds: string[]) =>
  dm<AllowList>("/api/v1/allow-list", { method: "POST", query: { for: silicon }, body: { type: "allow_list_add", data: { account_ids: accountIds } } });
export const disallowAccounts = (silicon: string, accountIds: string[]) =>
  dm<AllowList>("/api/v1/allow-list", { method: "DELETE", query: { for: silicon }, body: { type: "allow_list_remove", data: { account_ids: accountIds } } });

/* Reports ----------------------------------------------------------------------------------------------------------- */

export const submitReport = (message: string, clientVersion: string, pr?: string | null) =>
  dm<{ id: string; submitted: true; notification: string; repository: string }>("/api/v1/reports", {
    method: "POST",
    body: { type: "report", data: { message, client_version: clientVersion, ...(pr ? { pr } : {}) } },
    idempotencyKey: true,
  });

/* Voices and speech (spec §4.2–4.4) --------------------------------------------------------------------------------- */

export const listVoices = (signal?: AbortSignal) => dm<VoiceCatalog>("/api/v1/voices", { signal });
export const getVoice = (account: string, signal?: AbortSignal) => dm<AccountVoice>(`/api/v1/voices/${seg(account)}`, { signal });
export const setVoice = (account: string, voice: string) =>
  dm<AccountVoice>(`/api/v1/voices/${seg(account)}`, { method: "PUT", body: { type: "voice.set", data: { voice } }, idempotencyKey: true });
export const resetVoice = (account: string) => dm<AccountVoice>(`/api/v1/voices/${seg(account)}`, { method: "DELETE" });

/** Where the browser fetches a message's speech (WAV) through the proxy; Range requests work against it. */
export const messageSpeechUrl = (address: string, code: string, view?: string | null) =>
  `/api${messagePath(address, code)}/speech${view ? `?for=${encodeURIComponent(view)}` : ""}`;

/** A binary answer (audio) with the same error handling as JSON calls. */
async function binary(path: string, init: RequestInit & { method: string }): Promise<{ blob: Blob; headers: Headers }> {
  await ensureFreshSession();
  let response: Response;
  try {
    response = await fetch(`/api${path}`, { credentials: "same-origin", cache: "no-store", ...init, headers: { ...HEADERS, ...(init.headers as Record<string, string> | undefined) } });
  } catch (error) {
    throw networkError(error, init.method, path, "this site");
  }
  if (!response.ok) {
    const text = await response.text().catch(() => "");
    let body: unknown = null;
    try {
      body = text ? JSON.parse(text) : null;
    } catch {
      body = null;
    }
    const error = errorFromResponse(response, body, init.method, path);
    if (error.status === 401) markSignedOut();
    throw error;
  }
  return { blob: await response.blob(), headers: response.headers };
}

/** A message's speech as WAV: a speech message in its pinned voice, any other text in the sender's voice ("Listen"). */
export const fetchMessageSpeech = (address: string, code: string, options: { view?: string | null; signal?: AbortSignal } = {}) =>
  binary(`${messagePath(address, code)}/speech${options.view ? `?for=${encodeURIComponent(options.view)}` : ""}`, { method: "GET", signal: options.signal });

/** Renders a script in a voice (previews and samples). */
export const renderSpeech = (script: string, voice: string, signal?: AbortSignal) =>
  binary("/api/v1/speech", { method: "POST", body: JSON.stringify({ type: "speech.render", data: { script, voice } }), headers: { "Content-Type": "application/json", Accept: "audio/wav" }, signal });

export interface Transcription {
  transcript: string;
  language?: string;
  duration_milliseconds?: number;
  confidence?: number;
}

/** Transcribes recorded audio (spec §4.4). An empty transcript means no speech was heard. */
export const transcribe = (audio: Blob, options: { contentType?: string; language?: string; signal?: AbortSignal } = {}) =>
  dm<Transcription>("/api/v1/speech/transcriptions", {
    method: "POST",
    raw: audio,
    contentType: options.contentType ?? (audio.type || "audio/webm"),
    query: { language: options.language },
    signal: options.signal,
  });
