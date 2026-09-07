import type {
  AppConfig,
  Attachment,
  Bundle,
  BundleCreate,
  BundleDetail,
  Conversation,
  CreateTestingEnvironment,
  Draft,
  DraftInput,
  Gif,
  LoginInput,
  Message,
  MessageCreate,
  OutboxEntry,
  Page,
  Presence,
  Profile,
  ReceiptStatus,
  Session,
  TestingEnvironment,
} from "./models";
import {
  authenticated,
  claimOutbox,
  completeOutbox,
  failOutbox,
  getDeviceId,
  getGeneration,
  listOutbox,
  queueMessage as persistMessage,
  removeOutbox,
  scopeFor,
  broadcastUpdate,
  StorageError,
} from "./storage";

export class ApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
    public readonly detail?: unknown,
    public readonly idempotencyKey?: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
  get retryable(): boolean {
    return (
      this.status === 0 ||
      [408, 425, 429].includes(this.status) ||
      this.status >= 500
    );
  }
}
export interface ApiOptions extends Omit<RequestInit, "body"> {
  body?: unknown;
  session?: Session;
  profileId?: string;
  generation?: number | null;
  idempotencyKey?: string;
  version?: number;
}
let selectedSession: Session = { authenticated: false, profiles: [] };
export function currentSession(): Session {
  return selectedSession;
}
export function setSession(session: Session): Session {
  selectedSession = session;
  return session;
}
export function pathSegment(value: string): string {
  if (
    !value ||
    value === "." ||
    value === ".." ||
    /[\u0000-\u001f\u007f]/.test(value)
  )
    throw new ApiError(
      422,
      "validation_error",
      "An identifier is empty, a relative path, or contains control characters.",
    );
  return encodeURIComponent(value);
}
export function queryString(
  values: Record<string, string | number | boolean | undefined | null>,
): string {
  const query = new URLSearchParams();
  for (const [name, value] of Object.entries(values))
    if (value !== undefined && value !== null && value !== "")
      query.set(name, String(value));
  return query.size ? `?${query}` : "";
}
export function newIdempotencyKey(): string {
  return crypto.randomUUID();
}
/** Public build-time gateway address. Credentials always stay in its HttpOnly cookie. */
export function gatewayOrigin(): string {
  const configured = import.meta.env.VITE_DM_GATEWAY_ORIGIN;
  if (configured === undefined || configured === "")
    return window.location.origin;
  try {
    const origin = new URL(configured);
    if (
      !["http:", "https:"].includes(origin.protocol) ||
      origin.username ||
      origin.password ||
      origin.pathname !== "/" ||
      origin.search ||
      origin.hash ||
      configured.trim() !== configured
    )
      throw new Error("Invalid gateway origin");
    return origin.origin;
  } catch {
    throw new ApiError(
      0,
      "gateway_configuration",
      "VITE_DM_GATEWAY_ORIGIN must be an absolute HTTP or HTTPS origin without a path, credentials, or query.",
    );
  }
}
function destination(path: string): string {
  if (
    !path.startsWith("/") ||
    path.startsWith("//") ||
    path.includes("\\") ||
    /[\r\n\0]/.test(path)
  )
    throw new ApiError(
      422,
      "validation_error",
      "API paths must start with / and cannot include an origin.",
    );
  return path.startsWith("/api/") ? path : `/api/dm${path}`;
}
/** One request only. Callers retry with the same mutation key; bodies are never silently replayed. */
export async function api<T>(
  path: string,
  options: ApiOptions = {},
): Promise<T> {
  const {
    body,
    session = selectedSession,
    profileId,
    generation,
    idempotencyKey,
    version,
    ...requestOptions
  } = options;
  const target = destination(path);
  const url = new URL(target, gatewayOrigin());
  const headers = new Headers(requestOptions.headers);
  headers.set("Accept", "application/json");
  const profile = profileId ?? session.profile_id;
  if (profile) headers.set("X-DM-Profile", profile);
  if (idempotencyKey) {
    if (!/^[\x21-\x7e]{8,255}$/.test(idempotencyKey))
      throw new ApiError(
        422,
        "validation_error",
        "The request key must be 8–255 visible ASCII characters.",
      );
    headers.set("Idempotency-Key", idempotencyKey);
  }
  if (version !== undefined) {
    if (!Number.isSafeInteger(version) || version < 0)
      throw new ApiError(
        422,
        "validation_error",
        "Version must be a nonnegative integer.",
      );
    headers.set("If-Match", String(version));
  }
  if (target.startsWith("/api/dm/") && session.testing_environment_id) {
    const fence =
      generation === undefined ? await getGeneration(session) : generation;
    if (
      fence == null &&
      !["GET", "HEAD", "OPTIONS"].includes(
        (requestOptions.method || "GET").toUpperCase(),
      )
    )
      throw new StorageError(
        "generation_unknown",
        "Connect to this testing environment before changing its data.",
      );
    if (fence !== undefined && fence !== null)
      headers.set("X-Testing-Environment-Generation", String(fence));
  }
  let encoded: string | undefined;
  if (body !== undefined) {
    encoded = JSON.stringify(body);
    headers.set("Content-Type", "application/json");
  }
  let response: Response;
  try {
    response = await fetch(url, {
      ...requestOptions,
      headers,
      body: encoded,
      credentials: "include",
      cache: "no-store",
      redirect: "error",
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new ApiError(
      0,
      "network_error",
      "The server could not be reached. Your queued request can be retried with the same key.",
      undefined,
      idempotencyKey,
    );
  }
  if (
    response.status === 401 &&
    (target.startsWith("/api/dm/") || target === "/api/refresh")
  )
    window.dispatchEvent(
      new CustomEvent("dm:unauthorized", { detail: { profile_id: profile } }),
    );
  if (response.status === 204) return undefined as T;
  let text = await response.text();
  let value: unknown;
  try {
    value = text ? JSON.parse(text) : undefined;
    text = "";
  } catch {
    throw new ApiError(
      response.status,
      "invalid_response",
      "The server returned an unexpected response.",
      undefined,
      idempotencyKey,
    );
  }
  if (!response.ok) {
    const data = value as
      | { error?: { code?: string; message?: string } }
      | undefined;
    const code =
      response.status === 409 &&
      data?.error?.message?.includes("testing environment generation changed")
        ? "environment_changed"
        : data?.error?.code || `http_${response.status}`;
    throw new ApiError(
      response.status,
      code,
      data?.error?.message || `Request failed (${response.status}).`,
      value,
      idempotencyKey,
    );
  }
  return value as T;
}
export function mutation<T>(
  method: string,
  path: string,
  body?: unknown,
  key = newIdempotencyKey(),
  version?: number,
): Promise<T> {
  return api<T>(path, { method, body, idempotencyKey: key, version });
}
export async function getSession(): Promise<Session> {
  return setSession(await api<Session>("/api/session"));
}
export function getConfig(): Promise<AppConfig> {
  return api("/api/config");
}
export async function login(input: LoginInput): Promise<Session> {
  const result = await api<Session>("/api/login", {
    method: "POST",
    body: input,
  });
  return result?.authenticated === undefined
    ? getSession()
    : setSession(result);
}
export async function logout(
  profileId = selectedSession.profile_id,
): Promise<Session> {
  return setSession(
    await api<Session>("/api/logout", {
      method: "POST",
      body: { profile_id: profileId },
      profileId,
    }),
  );
}
export async function selectProfile(profileId: string): Promise<Session> {
  return setSession(
    await api<Session>("/api/profiles/select", {
      method: "POST",
      body: { profile_id: profileId },
    }),
  );
}
export async function refreshSession(
  profileId = selectedSession.profile_id,
): Promise<Session> {
  return setSession(
    await api<Session>("/api/refresh", {
      method: "POST",
      body: { profile_id: profileId },
      profileId,
    }),
  );
}
export function sessionForProfile(profile: Profile): Session {
  return { ...profile, authenticated: true };
}
function countCharacters(value: string, limit: number): boolean {
  if (value.length <= limit) return true;
  let count = 0;
  for (const _character of value) if (++count > limit) return false;
  return true;
}
function httpsUrl(value: string): void {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new ApiError(
      422,
      "validation_error",
      "Attachment URLs must be valid HTTPS URLs.",
    );
  }
  if (url.protocol !== "https:" || url.username || url.password)
    throw new ApiError(
      422,
      "validation_error",
      "Attachment URLs must use HTTPS without embedded credentials.",
    );
}
function validateAttachment(value: Attachment): void {
  httpsUrl(value.permanent_url);
  if (
    value.size !== undefined &&
    (!Number.isSafeInteger(value.size) ||
      value.size < 0 ||
      value.size > 5 * 1024 ** 3)
  )
    throw new ApiError(
      422,
      "validation_error",
      "An attachment may be at most 5 GiB.",
    );
}
export function validateMessage(message: MessageCreate): void {
  if (
    !message.text &&
    !message.attachments?.length &&
    !message.voice &&
    !message.gif
  )
    throw new ApiError(
      422,
      "validation_error",
      "Add text, an attachment, voice, or a GIF.",
    );
  if (message.text === "")
    throw new ApiError(
      422,
      "validation_error",
      "Message text cannot be empty.",
    );
  if (message.text && !countCharacters(message.text, 100_000_000))
    throw new ApiError(
      422,
      "validation_error",
      "Message text exceeds 100,000,000 characters.",
    );
  if (message.voice_transcript != null && !message.voice)
    throw new ApiError(
      422,
      "validation_error",
      "A voice transcript requires a voice attachment.",
    );
  if (
    message.voice_transcript &&
    !countCharacters(message.voice_transcript, 100_000_000)
  )
    throw new ApiError(
      422,
      "validation_error",
      "Voice transcript exceeds 100,000,000 characters.",
    );
  if ((message.attachments?.length ?? 0) + (message.voice ? 1 : 0) > 100)
    throw new ApiError(
      422,
      "validation_error",
      "A message may contain at most 100 attachments including voice.",
    );
  message.attachments?.forEach(validateAttachment);
  if (message.voice) {
    validateAttachment(message.voice);
    const duration = message.voice.duration_milliseconds;
    if (
      duration === null ||
      !Number.isSafeInteger(duration) ||
      duration < 1 ||
      duration > 172_800_000
    )
      throw new ApiError(
        422,
        "validation_error",
        "Voice duration must be between 1 millisecond and 48 hours.",
      );
  }
  if (message.gif) {
    httpsUrl(message.gif.url);
    if (message.gif.preview_url) httpsUrl(message.gif.preview_url);
  }
  if (
    message.metadata !== undefined &&
    (message.metadata === null ||
      Array.isArray(message.metadata) ||
      typeof message.metadata !== "object")
  )
    throw new ApiError(
      422,
      "validation_error",
      "Metadata must be a JSON object.",
    );
}
const conversationPath = (id: string) => `/conversations/${pathSegment(id)}`;
const messagePath = (conversation: string, id: string) =>
  `${conversationPath(conversation)}/messages/${pathSegment(id)}`;
export const dm = {
  me: () => api<unknown>("/auth/me"),
  conversations: (cursor?: string | null, limit = 50) =>
    api<Page<Conversation>>(`/conversations${queryString({ cursor, limit })}`),
  createConversation: (participantIds: string[], key?: string) =>
    mutation<Conversation>(
      "POST",
      "/conversations",
      { participant_ids: [...new Set(participantIds)] },
      key,
    ),
  messages: (
    conversation: string,
    cursor?: string | null,
    includeBundled = false,
    limit = 50,
  ) =>
    api<Page<Message>>(
      `${conversationPath(conversation)}/messages${queryString({ cursor, include_bundled_members: includeBundled, limit })}`,
    ),
  message: (conversation: string, id: string) =>
    api<Message>(messagePath(conversation, id)),
  send: (conversation: string, body: MessageCreate, key?: string) => {
    validateMessage(body);
    return mutation<Message>(
      "POST",
      `${conversationPath(conversation)}/messages`,
      body,
      key,
    );
  },
  edit: (
    conversation: string,
    id: string,
    body: MessageCreate,
    version: number,
    key?: string,
  ) => {
    validateMessage(body);
    return mutation<Message>(
      "PATCH",
      messagePath(conversation, id),
      body,
      key,
      version,
    );
  },
  deleteMessage: (
    conversation: string,
    id: string,
    version: number,
    key?: string,
  ) =>
    mutation<Message>(
      "DELETE",
      messagePath(conversation, id),
      undefined,
      key,
      version,
    ),
  receipt: async (
    conversation: string,
    id: string,
    status: ReceiptStatus,
    session = selectedSession,
  ) =>
    api<Message>(`${messagePath(conversation, id)}/receipts`, {
      method: "POST",
      body: { status, device_id: await getDeviceId(session) },
      session,
    }),
  draft: (conversation: string) =>
    api<Draft>(`${conversationPath(conversation)}/draft`),
  saveDraft: (conversation: string, body: DraftInput, version: number) =>
    api<Draft>(`${conversationPath(conversation)}/draft`, {
      method: "PUT",
      body,
      version,
    }),
  deleteDraft: (conversation: string) =>
    api<void>(`${conversationPath(conversation)}/draft`, { method: "DELETE" }),
  createBundle: (conversation: string, body: BundleCreate, key?: string) => {
    validateMessage(body.display_message);
    return mutation<Bundle>(
      "POST",
      `${conversationPath(conversation)}/bundles`,
      body,
      key,
    );
  },
  bundle: (conversation: string, id: string) =>
    api<BundleDetail>(
      `${conversationPath(conversation)}/bundles/${pathSegment(id)}`,
    ),
  presence: (actorId: string) =>
    api<Presence>(`/presence/${pathSegment(actorId)}`),
  trendingGifs: () => api<{ items: Gif[] }>("/gifs/trending"),
  searchGifs: (q: string) => {
    if (!q.trim() || !countCharacters(q, 50))
      throw new ApiError(
        422,
        "validation_error",
        "GIF searches require 1–50 characters.",
      );
    return api<{ items: Gif[] }>(`/gifs/search${queryString({ q })}`);
  },
  recentGifs: () => api<{ items: Gif[] }>("/gifs/recent"),
  environments: (includeDeleted = false) =>
    api<{ items: TestingEnvironment[] }>(
      `/testing-environments${queryString({ include_deleted: includeDeleted })}`,
    ),
  environment: (id: string) =>
    api<TestingEnvironment>(`/testing-environments/${pathSegment(id)}`),
  createEnvironment: (body: CreateTestingEnvironment, key?: string) =>
    mutation<TestingEnvironment & { root_key: string }>(
      "POST",
      "/testing-environments",
      body,
      key,
    ),
  updateEnvironment: (
    id: string,
    body: { name?: string; description?: string },
    key?: string,
  ) =>
    mutation<TestingEnvironment>(
      "PATCH",
      `/testing-environments/${pathSegment(id)}`,
      body,
      key,
    ),
  environmentKey: (id: string) =>
    api<{ environment_id: string; root_key: string }>(
      `/testing-environments/${pathSegment(id)}/key`,
    ),
  rotateEnvironmentKey: (id: string, key?: string) =>
    mutation<TestingEnvironment & { root_key: string }>(
      "POST",
      `/testing-environments/${pathSegment(id)}/rotate-key`,
      undefined,
      key,
    ),
  restoreEnvironment: (id: string, key?: string) =>
    mutation<TestingEnvironment & { root_key: string }>(
      "POST",
      `/testing-environments/${pathSegment(id)}/restore`,
      undefined,
      key,
    ),
  deleteEnvironment: (id: string, key?: string) =>
    mutation<void>(
      "DELETE",
      `/testing-environments/${pathSegment(id)}`,
      undefined,
      key,
    ),
  cleanEnvironment: (id: string, key?: string) =>
    mutation<void>(
      "POST",
      `/testing-environments/${pathSegment(id)}/clean`,
      undefined,
      key,
    ),
};
export async function queueMessage(
  session: Session,
  conversationId: string,
  body: MessageCreate,
  key?: string,
): Promise<OutboxEntry> {
  validateMessage(body);
  authenticated(session);
  if (body.sender_id && body.sender_id !== session.actor.id)
    throw new ApiError(
      403,
      "forbidden",
      "The queued sender must match this profile.",
    );
  const entry = await persistMessage(session, conversationId, body, key);
  window.dispatchEvent(
    new CustomEvent("dm:outbox", { detail: { scope: scopeFor(session) } }),
  );
  broadcastUpdate({ scope: scopeFor(session), kind: "outbox" });
  return entry;
}
export interface RetriedMessage {
  message: Message;
  idempotency_key: string;
}
const flushing = new Map<string, Promise<RetriedMessage[]>>();
/** Explicit retry surface. Durable body/key stay unchanged across every attempt.
 * Results stop at a 16 MiB memory budget, always allowing one oversized message.
 * Remaining entries stay queued and can be flushed in the next call.
 */
export function retryOutbox(
  session: Session,
  id?: string,
): Promise<RetriedMessage[]> {
  const scope = scopeFor(session);
  const existing = flushing.get(scope);
  if (existing) return existing;
  const operation = (async () => {
    const messages: RetriedMessage[] = [];
    let returnedBytes = 0;
    const saved = await listOutbox(session);
    if (
      id &&
      saved.some((entry) => entry.id === id && entry.status === "fenced")
    )
      throw new StorageError(
        "environment_changed",
        "This queued request belongs to an older testing generation. Review it and create a new message.",
      );
    const entries = saved.filter(
      (entry) => (!id || entry.id === id) && entry.status !== "fenced",
    );
    for (const entry of entries) {
      const claimed = await claimOutbox(session, entry.id);
      if (!claimed) continue;
      try {
        const message = await api<Message>(
          `${conversationPath(entry.conversation_id)}/messages`,
          {
            method: "POST",
            body: claimed.body,
            idempotencyKey: entry.idempotency_key,
            session,
            generation: entry.generation,
          },
        );
        messages.push({
          message: await completeOutbox(session, entry, message),
          idempotency_key: entry.idempotency_key,
        });
        returnedBytes +=
          2048 +
          2 *
            ((message.text?.length ?? 0) +
              (message.voice_transcript?.length ?? 0) +
              JSON.stringify(message.metadata ?? {}).length);
        broadcastUpdate({ scope, kind: "message", message_id: message.id });
      } catch (error) {
        const code =
          error instanceof ApiError || error instanceof StorageError
            ? error.code
            : "request_failed";
        await failOutbox(
          session,
          entry.id,
          error instanceof Error ? error.message : "Request failed.",
          code,
        );
        if (
          error instanceof ApiError &&
          (error.status === 401 || error.retryable)
        )
          throw error;
        if (id) throw error;
      } finally {
        window.dispatchEvent(
          new CustomEvent("dm:outbox", { detail: { scope } }),
        );
        broadcastUpdate({ scope, kind: "outbox" });
      }
      if (returnedBytes >= 16 * 1024 * 1024) break;
    }
    return messages;
  })();
  flushing.set(scope, operation);
  void operation.finally(() => flushing.delete(scope)).catch(() => undefined);
  return operation;
}
export { listOutbox, removeOutbox };
