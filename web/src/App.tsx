import {
  ErrorBoundary,
  createMemo,
  createEffect,
  createSignal,
  For,
  Match,
  onCleanup,
  onMount,
  Show,
  Switch,
} from "solid-js";
import {
  ApiError,
  api,
  getConfig,
  getSession,
  login,
  logout,
  queueMessage,
  retryOutbox,
  selectProfile,
  queryString,
  validateMessage,
  type ApiOptions,
} from "./api";
import type {
  AppConfig,
  Bundle,
  BundleDetail,
  Conversation,
  Draft,
  DraftInput,
  Page,
  Message,
  MessageCreate,
  OutboxEntry,
  Presence,
  RealtimeState,
  Session,
} from "./models";
import {
  cacheMessage,
  cachedMessages,
  getGeneration,
  listOutbox,
  mergeMessage,
  mergeStatus,
  removeCachedMessage,
  removeOutbox,
} from "./storage";
import { connectRealtime } from "./realtime";
import Composer from "./Composer";
import { MessageView } from "./MessageView";
import { AccountPanel, EnvironmentsPanel, OutboxPanel } from "./Panels";
import {
  Avatar,
  Brand,
  Busy,
  ConfirmHost,
  confirmAction,
  Empty,
  Icon,
  IconButton,
  JsonDetails,
  Modal,
  Notice,
  dateLabel,
  downloadJson,
  stamp,
} from "./ui";

const emptyContent = (): MessageCreate => ({ metadata: {} });
const contentOf = (message: Message): MessageCreate => ({
  text: message.text,
  metadata: message.metadata || {},
  attachments: message.attachments,
  voice: message.voice,
  voice_transcript: message.voice_transcript,
  gif: message.gif,
  reply_to_message_id: message.reply_to_message_id,
});
const draftContent = (value: Draft): MessageCreate => ({
  text: value.message_content || undefined,
  metadata: value.metadata || {},
  attachments: value.attachments,
  voice: value.voice,
  voice_transcript: value.voice_transcript,
  gif: value.gif,
  reply_to_message_id: value.reply_to_message_id,
});
const hasComposition = (value: MessageCreate): boolean =>
  !!(
    value.text ||
    value.attachments?.length ||
    value.voice ||
    value.voice_transcript ||
    value.gif ||
    value.reply_to_message_id ||
    Object.keys(value.metadata || {}).length
  );
const previewOf = (message?: Message | null) =>
  !message
    ? "Start the conversation"
    : message.deleted_at
      ? "Message deleted"
      : message.text?.slice(0, 100) ||
        (message.voice
          ? "Voice message"
          : message.gif
            ? "GIF"
            : message.bundle
              ? "Message bundle"
              : "Attachment");

export default function App() {
  const [session, setSession] = createSignal<Session>({ authenticated: false }),
    [config, setConfig] = createSignal<AppConfig>(),
    [loading, setLoading] = createSignal(true),
    [error, setError] = createSignal<unknown>(),
    [add, setAdd] = createSignal<{ id?: string; key?: string }>();
  async function reload() {
    setSession(await getSession());
  }
  onMount(() => {
    void Promise.all([getConfig(), getSession()])
      .then(([c, s]) => {
        setConfig(c);
        setSession(s);
      })
      .catch(setError)
      .finally(() => setLoading(false));
  });
  async function signIn(
    slt: string,
    testing_environment_id?: string,
    testing_key?: string,
  ) {
    setSession(
      await login({
        slt,
        testing_environment_id: testing_environment_id || undefined,
        testing_key: testing_key || undefined,
      }),
    );
    setAdd();
    setError();
  }
  return (
    <ErrorBoundary
      fallback={(e, reset) => (
        <main class="boot">
          <Brand />
          <h1>Unable to load this view</h1>
          <Notice error={e} />
          <button class="button" onClick={reset}>
            Try again
          </button>
        </main>
      )}
    >
      <ConfirmHost />
      <Show
        when={!loading()}
        fallback={
          <main class="boot">
            <Brand />
            <Busy label="Opening DM…" />
          </main>
        }
      >
        <Show
          when={!error()}
          fallback={
            <main class="boot">
              <Brand />
              <Notice error={error()} />
              <button class="button" onClick={() => location.reload()}>
                Try again
              </button>
            </main>
          }
        >
          <Show
            when={session().authenticated && session().profile_id}
            keyed
            fallback={<SignIn config={config()} submit={signIn} />}
          >
            {(_profile: string) => (
              <Workspace
                session={session()}
                reload={reload}
                changed={setSession}
                add={(id, key) => setAdd({ id, key })}
              />
            )}
          </Show>
        </Show>
      </Show>
      <Show when={add()}>
        {(value) => (
          <Modal
            title="Add an account"
            subtitle="Sign in with IAM or connect a test identity."
            close={() => setAdd()}
          >
            <SignIn
              compact
              config={config()}
              initial={value()}
              submit={signIn}
            />
          </Modal>
        )}
      </Show>
    </ErrorBoundary>
  );
}
function SignIn(props: {
  compact?: boolean;
  config?: AppConfig;
  initial?: { id?: string; key?: string };
  submit: (slt: string, id?: string, key?: string) => Promise<void>;
}) {
  const iamLoginHref = () => {
    const url = new URL(
      props.config?.iam_login_url || "/auth/login",
      window.location.origin,
    );
    return url.href;
  };
  const [advanced, setAdvanced] = createSignal(!!props.initial?.id),
    [slt, setSlt] = createSignal(""),
    [testId, setTestId] = createSignal(props.initial?.id || ""),
    [testKey, setTestKey] = createSignal(props.initial?.key || ""),
    [busy, setBusy] = createSignal(false),
    [error, setError] = createSignal<unknown>();
  const form = () => (
    <div class="auth-card">
      <Show when={!props.compact}>
        <p class="eyebrow">SILICON DM</p>
        <h2>Welcome back.</h2>
        <p class="muted auth-intro">A shared space for your conversations.</p>
      </Show>
      <Notice error={error()} />
      <div class="stack">
        <a class="button primary full" href={iamLoginHref()}>
          <img class="button-mark" src="/brand/mark.svg" alt="" />
          Continue with Silicon IAM
          <Icon name="chevron" size={15} />
        </a>
        <p class="footnote">
          Sign in or create your identity securely with IAM.
        </p>
        <button
          class="text-button advanced-login-toggle"
          onClick={() => setAdvanced((x) => !x)}
        >
          {advanced() ? "Hide" : "Use"} a short-lived token or test account
          <Icon name="down" size={14} />
        </button>
        <Show when={advanced()}>
          <form
            class="stack advanced-login"
            onSubmit={async (e) => {
              e.preventDefault();
              setBusy(true);
              setError();
              try {
                await props.submit(slt(), testId(), testKey());
                setSlt("");
                setTestKey("");
              } catch (e) {
                setError(e);
              } finally {
                setBusy(false);
              }
            }}
          >
            <label>
              IAM short-lived token
              <input
                type="password"
                autocomplete="off"
                required
                value={slt()}
                onInput={(e) => setSlt(e.currentTarget.value)}
                placeholder="Paste your DM sign-in token"
              />
            </label>
            <label>
              DM test environment ID{" "}
              <span class="muted">Optional for production</span>
              <input
                value={testId()}
                onInput={(e) => setTestId(e.currentTarget.value)}
              />
            </label>
            <Show when={testId()}>
              <label>
                DM test environment key
                <input
                  type="password"
                  autocomplete="off"
                  required
                  value={testKey()}
                  onInput={(e) => setTestKey(e.currentTarget.value)}
                />
              </label>
            </Show>
            <button class="button" disabled={busy()} type="submit">
              {busy() ? "Signing in…" : "Connect account"}
            </button>
          </form>
        </Show>
      </div>
    </div>
  );
  return props.compact ? (
    form()
  ) : (
    <main class="auth-layout">
      <aside class="auth-brand">
        <Brand />
        <div class="auth-statement">
          <p class="eyebrow">CONVERSATIONS, TOGETHER</p>
          <h1>
            Good work starts
            <br />
            with a conversation.
          </h1>
          <p>
            Stay in touch with your people and your Silicons. A little less
            noise. A little more connection.
          </p>
          <div class="auth-art" aria-hidden="true">
            <div class="art-line">
              <span class="art-avatar" />
              <div>
                <i />
                <i />
              </div>
            </div>
            <div class="art-line right">
              <div>
                <i />
                <i />
              </div>
              <span class="art-avatar blue" />
            </div>
            <div class="art-line">
              <span class="art-avatar" />
              <div>
                <i />
              </div>
            </div>
          </div>
        </div>
        <footer class="auth-foot">
          <span class="status-dot" />
          Part of Team of Silicons
        </footer>
      </aside>
      <section class="auth-panel">
        {form()}
        <footer class="auth-footer">
          <span>Carbons + Silicons</span>
          <span>One conversation at a time.</span>
        </footer>
      </section>
    </main>
  );
}
function Workspace(props: {
  session: Session;
  reload: () => Promise<void>;
  changed: (s: Session) => void;
  add: (id?: string, key?: string) => void;
}) {
  const [page, setPage] = createSignal("inbox"),
    [conversations, setConversations] = createSignal<Conversation[]>([]),
    [conversationCursor, setConversationCursor] = createSignal<string | null>(
      null,
    ),
    [loading, setLoading] = createSignal(true),
    [filter, setFilter] = createSignal(""),
    [current, setCurrent] = createSignal<string>(),
    [messages, setMessages] = createSignal<Message[]>([]),
    [messageCursor, setMessageCursor] = createSignal<string | null>(null),
    [messageLoading, setMessageLoading] = createSignal(false),
    [error, setError] = createSignal<unknown>(),
    [notice, setNotice] = createSignal(""),
    [connection, setConnection] = createSignal<RealtimeState>("connecting"),
    [outbox, setOutbox] = createSignal<OutboxEntry[]>([]),
    [compose, setCompose] = createSignal<MessageCreate>(emptyContent()),
    [draft, setDraft] = createSignal<Draft>(),
    [draftConflict, setDraftConflict] = createSignal<Draft | "deleted">(),
    [editing, setEditing] = createSignal<Message>(),
    [replyLabel, setReplyLabel] = createSignal(""),
    [sending, setSending] = createSignal(false),
    [newConversation, setNewConversation] = createSignal(false),
    [detail, setDetail] = createSignal(false),
    [reference, setReference] = createSignal<Message>(),
    [bundle, setBundle] = createSignal<BundleDetail>(),
    [bundleEditor, setBundleEditor] = createSignal(false),
    [selection, setSelection] = createSignal<string[] | undefined>(),
    [includeMembers, setIncludeMembers] = createSignal(false),
    [presence, setPresence] = createSignal<Presence[]>([]),
    [mobileNav, setMobileNav] = createSignal(false),
    [dirty, setDirty] = createSignal(false),
    [recovered, setRecovered] = createSignal<
      { id: string; content: MessageCreate; label: string }[]
    >([]);
  let scrollArea: HTMLDivElement | undefined;
  let followLatest = true;
  let scrollConversation: string | undefined;
  const visibleMessages = createMemo(() =>
    messages().filter((m) => includeMembers() || m.bundle?.role !== "member"),
  );
  const visibleMessageById = createMemo(
    () => new Map(visibleMessages().map((message) => [message.id, message])),
  );
  const visibleMessageIds = createMemo(() => [...visibleMessageById().keys()]);
  createEffect(() => {
    const id = current();
    const items = visibleMessages();
    const changedConversation = scrollConversation !== id;
    scrollConversation = id;
    if (changedConversation) followLatest = true;
    if (followLatest && items.length)
      queueMicrotask(() => {
        if (scrollArea && current() === id)
          scrollArea.scrollTop = scrollArea.scrollHeight;
      });
  });
  // Keep in-flight requests tied to the identity that mounted this workspace.
  const workspaceSession = props.session;
  const request = <T,>(path: string, options: ApiOptions = {}) =>
    api<T>(path, { ...options, session: workspaceSession });
  const conversationPath = (id: string) =>
    `/conversations/${encodeURIComponent(id)}`;
  const messagePath = (conversation: string, id: string) =>
    `${conversationPath(conversation)}/messages/${encodeURIComponent(id)}`;
  const dm = {
    conversations: (cursor?: string | null) =>
      request<Page<Conversation>>(
        `/conversations${queryString({ cursor, limit: 50 })}`,
      ),
    messages: (
      id: string,
      cursor: string | null,
      members: boolean,
      generation: number | null,
    ) =>
      request<Page<Message>>(
        `${conversationPath(id)}/messages${queryString({ cursor, include_bundled_members: members, limit: 50 })}`,
        { generation },
      ),
    draft: (id: string) => request<Draft>(`${conversationPath(id)}/draft`),
    saveDraft: (id: string, body: DraftInput, version: number) =>
      request<Draft>(`${conversationPath(id)}/draft`, {
        method: "PUT",
        body,
        version,
      }),
    deleteDraft: (id: string) =>
      request<void>(`${conversationPath(id)}/draft`, { method: "DELETE" }),
    presence: (id: string) =>
      request<Presence>(`/presence/${encodeURIComponent(id)}`),
    message: (conversation: string, id: string) =>
      request<Message>(messagePath(conversation, id)),
    bundle: (conversation: string, id: string, generation: number | null) =>
      request<BundleDetail>(
        `${conversationPath(conversation)}/bundles/${encodeURIComponent(id)}`,
        { generation },
      ),
    edit: (
      conversation: string,
      message: Message,
      body: MessageCreate,
      key: string,
    ) =>
      request<Message>(messagePath(conversation, message.id), {
        method: "PATCH",
        body,
        version: message.version,
        idempotencyKey: key,
      }),
    deleteMessage: (message: Message, key: string) =>
      request<Message>(messagePath(message.conversation_id, message.id), {
        method: "DELETE",
        version: message.version,
        idempotencyKey: key,
      }),
  };
  const active = createMemo(() =>
    conversations().find((c) => c.id === current()),
  );
  const actor = () => props.session.actor!.id;
  const otherActors = (c: Conversation) =>
    c.participants.filter((p) => p.id !== actor());
  const title = (c: Conversation) =>
    otherActors(c)
      .map((p) => p.id)
      .join(", ");
  const filtered = createMemo(() =>
    conversations().filter((c) =>
      `${title(c)} ${c.last_message?.text?.slice(0, 100) || ""}`
        .toLowerCase()
        .includes(filter().toLowerCase()),
    ),
  );
  let alive = true,
    viewRevision = 0,
    compositionRevision = 0,
    environmentRevision = 0,
    conversationRequest = 0,
    messageRequest = 0;
  let cacheHydration: { revision: number; promise: Promise<void> } | undefined;
  let typingTimer: ReturnType<typeof setTimeout> | undefined;
  let connectionApi: ReturnType<typeof connectRealtime> | undefined;
  let sendKey: string = crypto.randomUUID(),
    editKey: string = crypto.randomUUID();
  let beforeEdit:
    | { content: MessageCreate; label: string; dirty: boolean; key: string }
    | undefined;
  let flushing: Promise<void> | undefined;
  let flushAgain = false;
  let optimisticSequence = Math.floor(Number.MAX_SAFE_INTEGER / 2);
  const validEnvironment = (revision: number) =>
    alive && revision === environmentRevision;
  function autoFlush() {
    void flush().catch((e) => {
      if (alive) setError(e);
    });
  }
  const deleteKeys = new Map<string, string>();
  const validView = (revision: number) => alive && revision === viewRevision;
  function replaceComposition(value: MessageCreate, modified = true) {
    compositionRevision++;
    setCompose(value);
    setDirty(modified);
    sendKey = crypto.randomUUID();
    editKey = crypto.randomUUID();
  }
  async function confirmLeaving(action: string): Promise<boolean> {
    if (sending()) {
      setNotice("Wait for the current message to finish saving.");
      return false;
    }
    const revision = viewRevision;
    const composition = compositionRevision;
    const pending = recovered();
    const confirmed =
      !(dirty() || editing() || pending.length) ||
      (await confirmAction(
        `${action}? Unsaved compositions in this account will be discarded. Save a draft or download recovered content first.`,
      ));
    return (
      confirmed &&
      validView(revision) &&
      !sending() &&
      composition === compositionRevision &&
      pending === recovered()
    );
  }
  async function addAccount(id?: string, key?: string) {
    if (await confirmLeaving("Change accounts")) props.add(id, key);
  }
  function merge(incoming: Message) {
    if (!alive) return;
    if (incoming.conversation_id === current())
      setMessages((previous) => {
        const found = previous.find((m) => m.id === incoming.id);
        const value = mergeMessage(found, incoming);
        return [
          ...previous.filter((m) => m.id !== incoming.id),
          ...(includeMembers() || value.bundle?.role !== "member"
            ? [value]
            : []),
        ].sort((a, b) => a.sequence - b.sequence);
      });
    setConversations((previous) =>
      previous
        .map((c) =>
          c.id === incoming.conversation_id &&
          (!c.last_message ||
            c.last_message.id === incoming.id ||
            incoming.sequence >= c.last_message.sequence)
            ? {
                ...c,
                last_message: mergeMessage(
                  c.last_message?.id === incoming.id
                    ? c.last_message
                    : undefined,
                  incoming,
                ),
                updated_at:
                  c.updated_at > incoming.created_at
                    ? c.updated_at
                    : incoming.created_at,
              }
            : c,
        )
        .sort((a, b) => b.updated_at.localeCompare(a.updated_at)),
    );
  }
  function optimisticId(idempotencyKey: string): string {
    return `optimistic-${idempotencyKey}`;
  }
  async function showOptimistic(
    conversationId: string,
    body: MessageCreate,
    idempotencyKey: string,
  ): Promise<void> {
    const generation = await getGeneration(workspaceSession);
    if (generation === undefined) return;
    const message: Message = {
      ...body,
      id: optimisticId(idempotencyKey),
      conversation_id: conversationId,
      sender: workspaceSession.actor!,
      sequence: optimisticSequence++,
      version: 1,
      status: "waiting",
      created_at: new Date().toISOString(),
    };
    await cacheMessage(workspaceSession, message, generation);
    merge(message);
  }
  async function removeOptimistic(idempotencyKey: string): Promise<void> {
    const id = optimisticId(idempotencyKey);
    await removeCachedMessage(workspaceSession, id);
    if (alive) {
      setMessages((previous) => previous.filter((message) => message.id !== id));
      setConversations((previous) =>
        previous.map((conversation) =>
          conversation.last_message?.id === id
            ? { ...conversation, last_message: null }
            : conversation,
        ),
      );
    }
  }
  async function failOptimistic(
    idempotencyKey: string,
    error: unknown,
  ): Promise<void> {
    const id = optimisticId(idempotencyKey);
    const message = messages().find((item) => item.id === id);
    if (!message) return;
    const failed: Message = {
      ...message,
      status: "failed",
      failure_reason: error instanceof Error ? error.message : "Message failed.",
    };
    const generation = await getGeneration(workspaceSession);
    if (generation !== undefined) await cacheMessage(workspaceSession, failed, generation);
    merge(failed);
  }
  async function refreshOutbox() {
    try {
      const value = await listOutbox(workspaceSession);
      if (alive) setOutbox(value);
    } catch (e) {
      if (alive) setError(e);
    }
  }
  async function flush(id?: string): Promise<void> {
    if (!alive) return;
    if (connection() === "unauthorized") {
      if (id)
        throw new ApiError(
          401,
          "unauthorized",
          "Sign in again before retrying this message.",
        );
      return;
    }
    if (!navigator.onLine) {
      if (id)
        throw new ApiError(
          0,
          "network_error",
          "Reconnect to the network before retrying this message.",
        );
      return;
    }
    if (flushing) {
      if (!id) flushAgain = true;
      await flushing;
      if (!id || !alive) return;
    }
    const operation = (async () => {
      // A single result is released before the next queued 100M-character body is read.
      const entries = (await listOutbox(workspaceSession)).filter(
        (entry) => !id || entry.id === id,
      );
      for (const entry of entries) {
        if (!alive || !navigator.onLine || connection() === "unauthorized")
          break;
        if (!id && entry.status === "fenced") continue;
        try {
          for (const result of await retryOutbox(workspaceSession, entry.id)) {
            await removeOptimistic(result.idempotency_key);
            merge(result.message);
          }
        } catch (e) {
          await failOptimistic(entry.idempotency_key, e);
          if (alive) setError(e);
          if (id) throw e;
          if (e instanceof ApiError && (e.status === 401 || e.retryable)) break;
        }
      }
    })();
    flushing = operation;
    try {
      await operation;
    } finally {
      if (flushing === operation) flushing = undefined;
      await refreshOutbox();
      if (flushAgain && alive) {
        flushAgain = false;
        autoFlush();
      }
    }
  }
  async function loadConversations(append = false) {
    const revision = environmentRevision,
      requestId = ++conversationRequest;
    setLoading(true);
    try {
      const result = await dm.conversations(
        append ? conversationCursor() : null,
      );
      if (!validEnvironment(revision) || requestId !== conversationRequest)
        return;
      setConversations((old) => {
        const merged = new Map(
          (append ? old : old.filter((c) => c.id === current())).map((c) => [
            c.id,
            c,
          ]),
        );
        for (const incoming of result.items) {
          const previous = old.find((c) => c.id === incoming.id);
          let last = incoming.last_message;
          if (
            previous?.last_message &&
            (!last || previous.last_message.sequence > last.sequence)
          )
            last = previous.last_message;
          else if (last && previous?.last_message?.id === last.id)
            last = mergeMessage(previous.last_message, last);
          merged.set(incoming.id, {
            ...incoming,
            last_message: last,
            updated_at:
              previous && previous.updated_at > incoming.updated_at
                ? previous.updated_at
                : incoming.updated_at,
          });
        }
        return [...merged.values()].sort((a, b) =>
          b.updated_at.localeCompare(a.updated_at),
        );
      });
      setConversationCursor(result.next_cursor);
    } catch (e) {
      if (validEnvironment(revision) && requestId === conversationRequest)
        setError(e);
    } finally {
      if (validEnvironment(revision) && requestId === conversationRequest)
        setLoading(false);
    }
  }
  async function loadMessages(append = false) {
    const id = current();
    if (!id) return;
    const revision = viewRevision,
      requestId = ++messageRequest,
      members = includeMembers();
    setMessageLoading(true);
    try {
      if (cacheHydration?.revision === revision) await cacheHydration.promise;
      if (!validView(revision) || requestId !== messageRequest) return;
      const initialIds = new Set(messages().map((message) => message.id));
      const generation = await getGeneration(workspaceSession);
      if (!validView(revision) || generation === undefined) return;
      const result = await dm.messages(
        id,
        append ? messageCursor() : null,
        members,
        generation,
      );
      const observedGeneration = await getGeneration(workspaceSession);
      if (
        !validView(revision) ||
        requestId !== messageRequest ||
        generation !== observedGeneration
      )
        return;
      setMessages((old) => {
        const existing = new Map(old.map((message) => [message.id, message]));
        const merged = new Map(
          (append
            ? old
            : old.filter(
                (message) =>
                  !initialIds.has(message.id) ||
                  message.id.startsWith("optimistic-"),
              )
          ).map((message) => [message.id, message]),
        );
        for (const message of result.items)
          merged.set(
            message.id,
            mergeMessage(existing.get(message.id), message),
          );
        return [...merged.values()]
          .filter((message) => members || message.bundle?.role !== "member")
          .sort((a, b) => a.sequence - b.sequence);
      });
      setMessageCursor(result.next_cursor);
      for (const message of result.items) {
        if (!validView(revision) || requestId !== messageRequest) break;
        await cacheMessage(workspaceSession, message, generation);
        if (
          validView(revision) &&
          message.sender.id !== actor() &&
          !message.deleted_at
        )
          await connectionApi?.receipt(id, message.id, "delivered");
      }
    } catch (e) {
      if (validView(revision) && requestId === messageRequest) setError(e);
    } finally {
      if (validView(revision) && requestId === messageRequest)
        setMessageLoading(false);
    }
  }
  async function openConversation(id: string) {
    if (current() === id) {
      setPage("inbox");
      return;
    }
    if (
      (sending() || dirty() || editing()) &&
      !(await confirmLeaving("Switch conversations"))
    )
      return;
    viewRevision++;
    messageRequest++;
    setCurrent(id);
    setPage("inbox");
    setMessages([]);
    setMessageCursor(null);
    replaceComposition(emptyContent(), false);
    setReplyLabel("");
    beforeEdit = undefined;
    setDraft();
    setDraftConflict();
    setEditing();
    setSelection();
    setIncludeMembers(false);
    setPresence([]);
    setReference();
    setBundle();
    setDetail(false);
    setBundleEditor(false);
    setError();
    setNotice("");
    const revision = viewRevision,
      initialComposition = compositionRevision;
    cacheHydration = {
      revision,
      promise: cachedMessages(workspaceSession, id)
        .then((ms) => {
          if (validView(revision) && !messages().length)
            setMessages(
              ms.filter((message) => message.bundle?.role !== "member"),
            );
        })
        .catch((e) => {
          if (validView(revision)) setError(e);
        }),
    };
    void loadMessages();
    try {
      const saved = await dm.draft(id);
      if (validView(revision)) {
        if ((draft()?.version ?? 0) > saved.version) return;
        setDraft(saved);
        if (compositionRevision === initialComposition)
          replaceComposition(draftContent(saved), false);
        else {
          setDraftConflict(saved);
          setNotice(
            "A saved draft arrived. Your newer local composition was preserved.",
          );
        }
      }
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 404) && validView(revision))
        setError(e);
    }
    if (validView(revision)) void loadPresence();
  }
  async function loadPresence() {
    const c = active();
    if (!c) return;
    const revision = viewRevision;
    const results = await Promise.allSettled(
      otherActors(c).map((p) => dm.presence(p.id)),
    );
    if (validView(revision) && current() === c.id)
      setPresence(
        results.flatMap((r) => (r.status === "fulfilled" ? [r.value] : [])),
      );
  }
  function recoverComposition() {
    const pending = [] as {
      id: string;
      content: MessageCreate;
      label: string;
    }[];
    if (hasComposition(compose()))
      pending.push({
        id: crypto.randomUUID(),
        content: compose(),
        label: editing() ? "Unfinished edit" : "Unsent composition",
      });
    if (beforeEdit && hasComposition(beforeEdit.content))
      pending.push({
        id: crypto.randomUUID(),
        content: beforeEdit.content,
        label: "Composition from before editing",
      });
    setRecovered((previous) => [...previous, ...pending]);
    replaceComposition(emptyContent(), false);
    beforeEdit = undefined;
    setEditing();
    setReplyLabel("");
  }
  async function restoreRecovered(id: string) {
    if (!current() || sending()) return;
    const revision = viewRevision,
      composition = compositionRevision;
    if (
      (dirty() || editing()) &&
      !(await confirmAction(
        "Replace the current composition with this recovered content?",
      ))
    )
      return;
    if (
      !validView(revision) ||
      composition !== compositionRevision ||
      sending()
    )
      return;
    const entry = recovered().find((item) => item.id === id);
    if (!entry) return;
    const {
      sender_id: _sender,
      reply_to_message_id: _reply,
      ...content
    } = entry.content;
    replaceComposition(content);
    beforeEdit = undefined;
    setEditing();
    setReplyLabel("");
    setRecovered((previous) => previous.filter((item) => item.id !== id));
    setPage("inbox");
    setNotice(
      "Recovered content is ready for review. Any old reply or edit reference was removed.",
    );
  }
  onMount(() => {
    if (!workspaceSession.testing_environment_id) void loadConversations();
    void refreshOutbox();
    connectionApi = connectRealtime(workspaceSession, {
      onMessage: (message) => {
        merge(message);
        if (
          alive &&
          !conversations().some((c) => c.id === message.conversation_id)
        )
          void loadConversations();
      },
      onReceipt: (id, status) => {
        if (!alive) return;
        setMessages((ms) =>
          ms.map((m) =>
            m.id === id ? { ...m, status: mergeStatus(m.status, status) } : m,
          ),
        );
        setConversations((cs) =>
          cs.map((c) =>
            c.last_message?.id === id
              ? {
                  ...c,
                  last_message: {
                    ...c.last_message,
                    status: mergeStatus(c.last_message.status, status),
                  },
                }
              : c,
          ),
        );
      },
      onState: (state) => {
        if (alive) setConnection(state);
      },
      onReady: () => {
        if (!alive) return;
        setError((previous) =>
          previous instanceof ApiError && previous.status === 0
            ? undefined
            : previous,
        );
        void loadConversations();
        if (current()) void loadMessages();
        autoFlush();
      },
      onReset: () => {
        if (!alive) return;
        viewRevision++;
        environmentRevision++;
        conversationRequest++;
        messageRequest++;
        recoverComposition();
        setCurrent();
        setMessages([]);
        setConversations([]);
        setConversationCursor(null);
        setMessageCursor(null);
        setDraft();
        setDraftConflict();
        setSelection();
        setReference();
        setBundle();
        setBundleEditor(false);
        setDetail(false);
        setPresence([]);
        setNotice(
          "This test environment changed. Review recovered compositions and fenced outbox entries before sending again.",
        );
        void refreshOutbox();
        void loadConversations();
      },
      onError: (e) => {
        if (alive) setError(e);
      },
    });
    const timer = setInterval(() => {
      void refreshOutbox();
      if (
        outbox().some(
          (entry) =>
            entry.status === "sending" &&
            Date.now() - entry.updated_at >= 130_000,
        )
      )
        autoFlush();
    }, 2500);
    const presenceTimer = setInterval(() => {
      if (document.visibilityState === "visible") void loadPresence();
    }, 20000);
    const online = () => {
      autoFlush();
    };
    const changedOutbox = () => {
      void refreshOutbox();
    };
    const beforeUnload = (event: BeforeUnloadEvent) => {
      if (dirty() || editing() || recovered().length) {
        event.preventDefault();
        event.returnValue = "";
      }
    };
    window.addEventListener("online", online);
    window.addEventListener("dm:outbox", changedOutbox);
    window.addEventListener("beforeunload", beforeUnload);
    onCleanup(() => {
      alive = false;
      viewRevision++;
      clearInterval(timer);
      clearInterval(presenceTimer);
      clearTimeout(typingTimer);
      connectionApi?.close();
      window.removeEventListener("online", online);
      window.removeEventListener("dm:outbox", changedOutbox);
      window.removeEventListener("beforeunload", beforeUnload);
    });
  });
  function changeContent(value: MessageCreate) {
    replaceComposition(value);
    connectionApi?.presence("typing");
    clearTimeout(typingTimer);
    typingTimer = setTimeout(() => connectionApi?.presence(null), 4000);
  }
  async function send() {
    const id = current();
    if (!id || sending()) return;
    const revision = viewRevision,
      localRevision = compositionRevision,
      target = editing();
    const body = {
      ...compose(),
      text: compose().text || undefined,
      metadata: compose().metadata || {},
    };
    validateMessage(body);
    setSending(true);
    setError();
    try {
      if (target) {
        const message = await dm.edit(id, target, body, editKey);
        merge(message);
        if (validView(revision) && localRevision === compositionRevision) {
          cancelEdit();
          setNotice("Message updated.");
        }
      } else {
        await queueMessage(workspaceSession, id, body, sendKey);
        await showOptimistic(id, body, sendKey);
        if (validView(revision) && localRevision === compositionRevision) {
          replaceComposition(emptyContent(), false);
          setReplyLabel("");
          setNotice("Message saved to your outbox.");
        }
        if (alive) {
          await refreshOutbox();
          autoFlush();
        }
      }
      if (validView(revision)) connectionApi?.presence(null);
    } finally {
      if (alive) setSending(false);
    }
  }
  async function saveDraft() {
    const id = current();
    if (!id || editing()) return;
    const revision = viewRevision,
      localRevision = compositionRevision;
    const { text, sender_id: _s, ...rest } = compose();
    try {
      const saved = await dm.saveDraft(
        id,
        { ...rest, message_content: text || undefined },
        draft()?.version || 0,
      );
      if (validView(revision)) {
        setDraft(saved);
        setDraftConflict();
        if (localRevision === compositionRevision) setDirty(false);
        setNotice("Draft saved across your devices.");
      }
    } catch (e) {
      if (e instanceof ApiError && e.status === 409 && validView(revision)) {
        const remote = e.detail as Partial<Draft> | undefined;
        if (remote?.version && remote.conversation_id === id)
          setDraftConflict(remote as Draft);
        else
          try {
            const latest = await dm.draft(id);
            if (validView(revision)) setDraftConflict(latest);
          } catch (loadError) {
            if (
              loadError instanceof ApiError &&
              loadError.status === 404 &&
              validView(revision)
            )
              setDraftConflict("deleted");
          }
      }
      throw e;
    }
  }
  async function clearDraft() {
    const id = current();
    if (!id) return;
    const revision = viewRevision;
    await dm.deleteDraft(id);
    if (validView(revision)) {
      setDraft();
      setDraftConflict();
      if (beforeEdit) beforeEdit.dirty = hasComposition(beforeEdit.content);
      setDirty(hasComposition(compose()));
      setNotice("Saved draft deleted. Your local composition is unchanged.");
    }
  }
  async function beginEdit(message: Message) {
    if (sending()) return;
    const revision = viewRevision,
      composition = compositionRevision;
    if (
      editing() &&
      !(await confirmAction(
        "Discard this unfinished edit and edit another message?",
      ))
    )
      return;
    if (
      !validView(revision) ||
      composition !== compositionRevision ||
      sending()
    )
      return;
    beforeEdit ??= {
      content: compose(),
      label: replyLabel(),
      dirty: dirty(),
      key: sendKey,
    };
    setEditing(message);
    replaceComposition(contentOf(message));
    setReplyLabel("");
  }
  function cancelEdit() {
    if (editing()) {
      const previous = beforeEdit;
      beforeEdit = undefined;
      setEditing();
      replaceComposition(
        previous?.content || emptyContent(),
        previous?.dirty || false,
      );
      setReplyLabel(previous?.label || "");
      if (previous) sendKey = previous.key;
    } else {
      replaceComposition({ ...compose(), reply_to_message_id: undefined });
      setReplyLabel("");
    }
  }
  function beginReply(message: Message) {
    if (editing()) cancelEdit();
    changeContent({ ...compose(), reply_to_message_id: message.id });
    setReplyLabel(message.sender.id);
  }
  async function deleteMessage(message: Message) {
    const revision = viewRevision;
    if (
      !(await confirmAction(
        "Delete this message for every participant? A deletion marker will remain.",
      ))
    )
      return;
    if (!validView(revision)) return;
    const identifier = `${message.id}:${message.version}`;
    const key = deleteKeys.get(identifier) || crypto.randomUUID();
    deleteKeys.set(identifier, key);
    try {
      merge(await dm.deleteMessage(message, key));
    } catch (e) {
      if (alive) setError(e);
    }
  }
  async function showReference(id: string) {
    const conversation = current();
    if (!conversation) return;
    const revision = viewRevision;
    try {
      const value = await dm.message(conversation, id);
      if (validView(revision)) setReference(value);
    } catch (e) {
      if (validView(revision)) setError(e);
    }
  }
  async function rememberBundle(
    value: Bundle,
    generation: number | null,
    revision: number,
    selected: Message[] = [],
  ): Promise<boolean> {
    if (!validView(revision)) return false;
    const originals = new Map(
      [...selected, ...(value.original_messages ?? [])].map((message) => [
        message.id,
        message,
      ]),
    );
    const incoming = [
      value.display_message,
      ...value.original_message_ids.flatMap((id) => {
        const message = originals.get(id);
        return message
          ? [{ ...message, bundle: { id: value.id, role: "member" as const } }]
          : [];
      }),
    ];
    const updated: Message[] = [];
    for (const message of incoming) {
      if (!validView(revision)) return false;
      let stored = await cacheMessage(workspaceSession, message, generation);
      // Membership is immutable once assigned, even when the cached content
      // has a newer revision than the selection captured before creation.
      if (message.bundle && !stored.bundle)
        stored = await cacheMessage(
          workspaceSession,
          { ...stored, bundle: message.bundle },
          generation,
        );
      updated.push(stored);
    }
    if (
      !validView(revision) ||
      generation !== (await getGeneration(workspaceSession)) ||
      !validView(revision)
    )
      return false;
    for (const message of updated) merge(message);
    return true;
  }
  async function showBundle(message: Message) {
    if (!message.bundle) return;
    const revision = viewRevision;
    try {
      const generation = await getGeneration(workspaceSession);
      if (!validView(revision) || generation === undefined) return;
      const value = await dm.bundle(
        message.conversation_id,
        message.bundle.id,
        generation,
      );
      if (await rememberBundle(value, generation, revision)) setBundle(value);
    } catch (e) {
      if (validView(revision)) setError(e);
    }
  }
  const selectedMessages = () =>
    messages().filter((m) => selection()?.includes(m.id));
  const profiles = () => props.session.profiles || [];
  const own = (m: Message) => m.sender.id === actor();
  return (
    <div
      class={`workspace ${current() && page() === "inbox" ? "chat-open" : ""} ${mobileNav() ? "nav-open" : ""}`}
    >
      <aside class="sidebar">
        <div class="sidebar-brand">
          <Brand />
          <button
            class="mobile-only icon-button"
            aria-label="Close navigation"
            onClick={() => setMobileNav(false)}
          >
            <Icon name="close" />
          </button>
        </div>
        <div class="workspace-selector">
          <label>
            WORKSPACE
            <select
              aria-label="Switch account"
              value={props.session.profile_id}
              onChange={async (e) => {
                const select = e.currentTarget,
                  profile = select.value;
                if (profile === workspaceSession.profile_id) return;
                if (!(await confirmLeaving("Switch accounts"))) {
                  select.value = workspaceSession.profile_id!;
                  return;
                }
                try {
                  const selected = await selectProfile(profile);
                  if (alive) props.changed(selected);
                } catch (e) {
                  select.value = workspaceSession.profile_id!;
                  if (alive) setError(e);
                }
              }}
            >
              <For each={profiles()}>
                {(p) => (
                  <option value={p.profile_id}>
                    {p.organization_id} · {p.actor.id}
                    {p.testing_environment_id ? " · Test" : ""}
                  </option>
                )}
              </For>
            </select>
          </label>
          <span
            class={`environment-label ${props.session.testing_environment_id ? "testing" : ""}`}
          >
            <span class="status-dot" />
            {props.session.testing_environment_id
              ? "Testing environment"
              : "Production"}
          </span>
        </div>
        <nav aria-label="Main navigation">
          <For
            each={[
              { id: "inbox", icon: "inbox", title: "Conversations" },
              { id: "outbox", icon: "queue", title: "Outbox" },
              {
                id: "environments",
                icon: "flask",
                title: "Testing environments",
              },
              { id: "account", icon: "user", title: "Account" },
            ]}
          >
            {(item) => (
              <button
                class={page() === item.id ? "selected" : ""}
                onClick={() => {
                  setPage(item.id);
                  setMobileNav(false);
                }}
              >
                <Icon name={item.icon} />
                <span>{item.title}</span>
                <Show when={item.id === "outbox" && outbox().length}>
                  <span class="nav-count">{outbox().length}</span>
                </Show>
              </button>
            )}
          </For>
        </nav>
        <div class="sidebar-bottom">
          <div class="sidebar-note">
            <Icon name="book" />
            <div>
              <strong>Built for both kinds of mind.</strong>
              <span>Carbons + Silicons</span>
            </div>
          </div>
          <button class="account-link" onClick={() => setPage("account")}>
            <Avatar
              name={actor()}
              silicon={props.session.actor?.type === "silicon"}
            />
            <span>
              <strong>{actor()}</strong>
              <small>{props.session.organization_id}</small>
            </span>
            <Icon name="settings" size={16} />
          </button>
        </div>
      </aside>
      <div class="workspace-main">
        <header class="topbar">
          <div class="breadcrumb">
            <button
              class="mobile-only icon-button"
              aria-label="Open navigation"
              onClick={() => setMobileNav(true)}
            >
              <Icon name="more" />
            </button>
            <span>{props.session.organization_id}</span>
            <span>/</span>
            <strong>
              {page() === "inbox"
                ? "Conversations"
                : page() === "environments"
                  ? "Testing environments"
                  : page() === "outbox"
                    ? "Outbox"
                    : "Account"}
            </strong>
          </div>
          <div class={`connection connection-${connection()}`} role="status">
            <span class="status-dot" />
            {connection() === "connected"
              ? "Connected"
              : connection() === "unauthorized"
                ? "Sign-in required"
                : connection().replace(/^./, (s) => s.toUpperCase())}
            <Show when={connection() === "unauthorized"}>
              <button class="text-button" onClick={() => addAccount()}>
                Sign in
              </button>
            </Show>
          </div>
        </header>
        <Show when={recovered().length}>
          <div class="global-notice">
            <div class="notice error">
              <div>
                <strong>
                  Unsent content was preserved after this environment changed.
                </strong>
                <p>
                  Select a conversation, then review a composition before
                  sending it again.
                </p>
                <For each={recovered()}>
                  {(entry) => (
                    <div class="actions">
                      <span>{entry.label}</span>
                      <button
                        class="button small"
                        disabled={!current()}
                        onClick={() => restoreRecovered(entry.id)}
                      >
                        Use in selected conversation
                      </button>
                      <button
                        class="button small"
                        onClick={() =>
                          downloadJson(
                            entry.content,
                            "dm-recovered-composition.json",
                          )
                        }
                      >
                        Download content
                      </button>
                      <button
                        class="text-button"
                        onClick={async () => {
                          const revision = environmentRevision;
                          if (
                            (await confirmAction(
                              "Discard this recovered composition?",
                            )) &&
                            validEnvironment(revision)
                          )
                            setRecovered((previous) =>
                              previous.filter((item) => item.id !== entry.id),
                            );
                        }}
                      >
                        Discard
                      </button>
                    </div>
                  )}
                </For>
              </div>
            </div>
          </div>
        </Show>
        <Show when={page() !== "inbox"}>
          <div class="global-notice">
            <Notice error={error()} dismiss={() => setError()} />
          </div>
        </Show>
        <Switch>
          <Match when={page() === "inbox"}>
            <div class="inbox-layout">
              <section class="conversation-pane" aria-label="Conversations">
                <div class="conversation-title">
                  <h1>Conversations</h1>
                  <IconButton
                    icon="plus"
                    label="New conversation"
                    onClick={() => setNewConversation(true)}
                  />
                </div>
                <label class="conversation-search">
                  <Icon name="search" size={16} />
                  <input
                    placeholder="Filter conversations"
                    aria-label="Filter loaded conversations"
                    value={filter()}
                    onInput={(e) => setFilter(e.currentTarget.value)}
                  />
                </label>
                <div class="conversation-list">
                  <Show
                    when={!loading() || conversations().length}
                    fallback={<Busy />}
                  >
                    <For each={filtered()}>
                      {(c) => (
                        <button
                          class={`conversation-item ${current() === c.id ? "selected" : ""}`}
                          onClick={() => void openConversation(c.id)}
                        >
                          <Avatar
                            name={title(c)}
                            silicon={
                              otherActors(c).length === 1 &&
                              otherActors(c)[0].type === "silicon"
                            }
                          />
                          <span class="conversation-copy">
                            <span class="conversation-line">
                              <strong>{title(c)}</strong>
                              <time>{stamp(c.last_message?.created_at)}</time>
                            </span>
                            <span class="conversation-preview">
                              {previewOf(c.last_message)}
                            </span>
                            <span class="conversation-kind">
                              {otherActors(c).length > 1
                                ? `${c.participants.length} participants`
                                : otherActors(c)[0]?.type === "silicon"
                                  ? "Silicon"
                                  : "Carbon"}
                            </span>
                          </span>
                        </button>
                      )}
                    </For>
                    <Show when={!filtered().length}>
                      <div class="list-empty">
                        <p>
                          {filter()
                            ? "No matching conversations."
                            : "Your conversations will appear here."}
                        </p>
                        <Show when={!filter()}>
                          <button
                            class="text-button"
                            onClick={() => setNewConversation(true)}
                          >
                            Start a conversation
                          </button>
                        </Show>
                      </div>
                    </Show>
                  </Show>
                  <Show when={conversationCursor()}>
                    <button
                      class="load-more"
                      disabled={loading()}
                      onClick={() => void loadConversations(true)}
                    >
                      {loading() ? "Loading…" : "Load more conversations"}
                    </button>
                  </Show>
                </div>
                <footer class="conversation-footer">
                  <span>{conversations().length} loaded</span>
                  <IconButton
                    icon="refresh"
                    label="Refresh conversations"
                    onClick={() => void loadConversations()}
                    disabled={loading()}
                  />
                </footer>
              </section>
              <section class="chat-pane" aria-label="Conversation">
                <Notice error={error()} dismiss={() => setError()} />
                <Show
                  when={active()}
                  fallback={
                    <Empty title="Pick up the conversation">
                      <p>
                        Choose a conversation or start a new one.
                        <br />
                        Your people and Silicons are one message away.
                      </p>
                      <button
                        class="button"
                        onClick={() => setNewConversation(true)}
                      >
                        <Icon name="plus" />
                        New conversation
                      </button>
                    </Empty>
                  }
                >
                  {(c) => (
                    <>
                      <header class="chat-header">
                        <button
                          class="mobile-only icon-button"
                          aria-label="Back to conversations"
                          onClick={() => {
                            setCurrent();
                            viewRevision++;
                          }}
                        >
                          <Icon name="back" />
                        </button>
                        <Avatar
                          name={title(c())}
                          silicon={
                            otherActors(c()).length === 1 &&
                            otherActors(c())[0].type === "silicon"
                          }
                        />
                        <div class="chat-heading">
                          <h2>{title(c())}</h2>
                          <p>
                            <Show
                              when={presence().some(
                                (p) => p.availability === "online",
                              )}
                              fallback={
                                <span>
                                  {c().participants.length} participants
                                </span>
                              }
                            >
                              <span class="online-dot" />
                              {
                                presence().filter(
                                  (p) => p.availability === "online",
                                ).length
                              }{" "}
                              online
                            </Show>
                            <Show when={presence().some((p) => p.activity)}>
                              <span>
                                {" "}
                                ·{" "}
                                {presence()
                                  .filter((p) => p.activity)
                                  .map(
                                    (p) =>
                                      `${p.actor_id} ${p.activity?.replaceAll("_", " ")}`,
                                  )
                                  .join(", ")}
                              </span>
                            </Show>
                          </p>
                        </div>
                        <div class="chat-header-actions">
                          <Show when={props.session.actor?.type === "silicon"}>
                            <IconButton
                              icon="bundle"
                              label="Select messages to bundle"
                              active={!!selection()}
                              onClick={() =>
                                setSelection((s) => (s ? undefined : []))
                              }
                            />
                          </Show>
                          <IconButton
                            icon="refresh"
                            label="Refresh conversation"
                            onClick={() => void loadMessages()}
                            disabled={messageLoading()}
                          />
                          <IconButton
                            icon="info"
                            label="Conversation details"
                            onClick={() => {
                              setDetail(true);
                              void loadPresence();
                            }}
                          />
                        </div>
                      </header>
                      <Show when={selection()}>
                        <div class="selection-bar">
                          <span>{selection()!.length} selected</span>
                          <button
                            class="button small"
                            disabled={!selection()!.length}
                            onClick={() => setBundleEditor(true)}
                          >
                            Create bundle
                          </button>
                          <button
                            class="text-button"
                            onClick={() => setSelection()}
                          >
                            Cancel
                          </button>
                        </div>
                      </Show>
                      <div
                        class="messages-scroll"
                        aria-live="polite"
                        ref={scrollArea}
                        onScroll={(e) => {
                          const el = e.currentTarget;
                          followLatest =
                            el.scrollHeight - el.clientHeight - el.scrollTop <
                            100;
                        }}
                      >
                        <Show when={messageCursor()}>
                          <button
                            class="load-more"
                            disabled={messageLoading()}
                            onClick={() => void loadMessages(true)}
                          >
                            {messageLoading()
                              ? "Loading…"
                              : "Load earlier messages"}
                          </button>
                        </Show>
                        <Show when={messageLoading() && !messages().length}>
                          <Busy />
                        </Show>
                        <Show when={!messageLoading() && !messages().length}>
                          <Empty icon="send" title="Say hello">
                            <p>This is the start of your conversation.</p>
                          </Empty>
                        </Show>
                        <div class="message-timeline">
                          <For each={visibleMessageIds()}>
                            {(id, index) => {
                              const message = createMemo<Message>(
                                (previous) =>
                                  visibleMessageById().get(id) ?? previous,
                                visibleMessageById().get(id)!,
                              );
                              return (
                                <>
                                  <Show
                                    when={
                                      index() === 0 ||
                                      visibleMessages()[
                                        index() - 1
                                      ]?.created_at.slice(0, 10) !==
                                        message().created_at.slice(0, 10)
                                    }
                                  >
                                    <div class="day-divider">
                                      <span>
                                        {dateLabel(message().created_at)}
                                      </span>
                                    </div>
                                  </Show>
                                  <MessageView
                                    message={message()}
                                    own={own(message())}
                                    selecting={!!selection()}
                                    selected={selection()?.includes(
                                      message().id,
                                    )}
                                    toggle={() =>
                                      setSelection((s) =>
                                        s?.includes(message().id)
                                          ? s.filter(
                                              (id) => id !== message().id,
                                            )
                                          : [...(s || []), message().id].slice(
                                              0,
                                              100,
                                            ),
                                      )
                                    }
                                    reply={() => beginReply(message())}
                                    edit={() => beginEdit(message())}
                                    remove={() => void deleteMessage(message())}
                                    bundle={() => void showBundle(message())}
                                    reference={(id) => void showReference(id)}
                                    read={() =>
                                      connectionApi?.receipt(
                                        message().conversation_id,
                                        message().id,
                                        "read",
                                      )
                                    }
                                  />
                                </>
                              );
                            }}
                          </For>
                        </div>
                      </div>
                      <Show when={notice()}>
                        <div class="chat-notice" role="status">
                          <Icon name="check" size={14} />
                          {notice()}
                          <button
                            class="text-button"
                            onClick={() => setNotice("")}
                          >
                            Dismiss
                          </button>
                        </div>
                      </Show>
                      <Show when={draftConflict()}>
                        <div class="notice error">
                          <div>
                            <strong>
                              Your draft changed on another device.
                            </strong>
                            <p>
                              Your composition is preserved. Choose which
                              version to continue with.
                            </p>
                            <div class="actions">
                              <button
                                class="button small"
                                onClick={() => {
                                  const remote = draftConflict();
                                  if (remote && remote !== "deleted") {
                                    setDraft(remote);
                                    replaceComposition(
                                      draftContent(remote),
                                      false,
                                    );
                                  } else {
                                    setDraft();
                                    replaceComposition(emptyContent(), false);
                                  }
                                  beforeEdit = undefined;
                                  setEditing();
                                  setReplyLabel("");
                                  setDraftConflict();
                                }}
                              >
                                Use saved version
                              </button>
                              <button
                                class="button small"
                                onClick={() => {
                                  const remote = draftConflict();
                                  setDraft(
                                    remote && remote !== "deleted"
                                      ? remote
                                      : undefined,
                                  );
                                  setDraftConflict();
                                  setDirty(true);
                                  setNotice(
                                    "Local composition kept. Save draft to replace the current version.",
                                  );
                                }}
                              >
                                Keep my composition
                              </button>
                            </div>
                          </div>
                        </div>
                      </Show>
                      <Composer
                        value={compose()}
                        change={changeContent}
                        busy={sending()}
                        send={send}
                        save={saveDraft}
                        clearDraft={clearDraft}
                        editing={!!editing()}
                        replyLabel={replyLabel()}
                        cancelContext={cancelEdit}
                        draftLabel={
                          draft()
                            ? `Saved draft · ${stamp(draft()!.updated_at)}`
                            : undefined
                        }
                      />
                    </>
                  )}
                </Show>
              </section>
            </div>
          </Match>
          <Match when={page() === "outbox"}>
            <OutboxPanel
              entries={outbox()}
              retry={flush}
              remove={async (id) => {
                const entry = outbox().find((item) => item.id === id);
                await removeOutbox(workspaceSession, id);
                if (entry) await removeOptimistic(entry.idempotency_key);
                await refreshOutbox();
              }}
            />
          </Match>
          <Match when={page() === "account"}>
            <AccountPanel
              session={props.session}
              add={() => addAccount()}
              logout={async () => {
                if (await confirmLeaving("Sign out")) {
                  const remaining = await logout(workspaceSession.profile_id);
                  if (alive) props.changed(remaining);
                }
              }}
              activity={(value) => connectionApi?.presence(value)}
              reload={props.reload}
            />
          </Match>
          <Match when={page() === "environments"}>
            <EnvironmentsPanel session={props.session} signIn={addAccount} />
          </Match>
        </Switch>
      </div>
      <Show when={newConversation()}>
        <NewConversation
          session={workspaceSession}
          close={() => setNewConversation(false)}
          done={(conversation) => {
            setConversations((cs) => [
              conversation,
              ...cs.filter((c) => c.id !== conversation.id),
            ]);
            setNewConversation(false);
            void openConversation(conversation.id);
          }}
        />
      </Show>
      <Show when={detail() && active()}>
        <Modal title="Conversation details" close={() => setDetail(false)}>
          <div class="stack">
            <p class="muted">{active()!.participants.length} participants</p>
            <For each={active()!.participants}>
              {(p) => (
                <div class="participant">
                  <Avatar name={p.id} silicon={p.type === "silicon"} />
                  <div>
                    <strong>
                      {p.id}
                      {p.id === actor() ? " (you)" : ""}
                    </strong>
                    <small>
                      {p.type} ·{" "}
                      {presence().find((x) => x.actor_id === p.id)
                        ?.availability ||
                        (p.id === actor() && connection() === "connected"
                          ? "online"
                          : "offline")}
                    </small>
                  </div>
                </div>
              )}
            </For>
            <label class="check-label">
              <input
                type="checkbox"
                checked={includeMembers()}
                onChange={(e) => {
                  setIncludeMembers(e.currentTarget.checked);
                  void loadMessages();
                }}
              />
              Show original messages inside bundles
            </label>
            <JsonDetails
              value={{
                id: active()!.id,
                organization: active()!.org_id,
                created_at: active()!.created_at,
              }}
              title="Conversation identifiers"
            />
          </div>
        </Modal>
      </Show>
      <Show when={reference()}>
        {(m) => (
          <Modal title="Replied message" close={() => setReference()} wide>
            <MessageView message={m()} own={own(m())} />
          </Modal>
        )}
      </Show>
      <Show when={bundle()}>
        {(b) => (
          <Modal
            title="Message bundle"
            subtitle={`${b().original_message_ids.length} original messages · preserved in order`}
            close={() => setBundle()}
            wide
          >
            <MessageView
              message={b().display_message}
              own={own(b().display_message)}
            />
            <div class="section-label">ORIGINAL MESSAGES</div>
            <For each={b().original_messages ?? []}>
              {(m) => <MessageView message={m} own={own(m)} />}
            </For>
          </Modal>
        )}
      </Show>
      <Show
        when={bundleEditor() ? { revision: viewRevision } : undefined}
        keyed
      >
        {({ revision }) => {
          return (
            <BundleEditor
              session={workspaceSession}
              messages={selectedMessages()}
              conversation={current()!}
              close={() => setBundleEditor(false)}
              done={async (value, generation, selected) => {
                if (
                  !(await rememberBundle(value, generation, revision, selected))
                )
                  return;
                setBundleEditor(false);
                setSelection();
                void loadMessages();
              }}
            />
          );
        }}
      </Show>
    </div>
  );
}
function NewConversation(props: {
  session: Session;
  close: () => void;
  done: (c: Conversation) => void;
}) {
  const [participants, setParticipants] = createSignal(""),
    [busy, setBusy] = createSignal(false),
    [error, setError] = createSignal<unknown>();
  let key = crypto.randomUUID(),
    alive = true;
  onCleanup(() => {
    alive = false;
  });
  return (
    <Modal
      title="New conversation"
      subtitle="Invite Carbons and Silicons from your organization."
      close={props.close}
    >
      <form
        class="stack"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setError();
          try {
            const conversation = await api<Conversation>("/conversations", {
              method: "POST",
              session: props.session,
              idempotencyKey: key,
              body: {
                participant_ids: [
                  ...new Set(
                    participants()
                      .split(/[\s,]+/)
                      .filter(Boolean),
                  ),
                ],
              },
            });
            if (alive) props.done(conversation);
          } catch (e) {
            if (alive) setError(e);
          } finally {
            if (alive) setBusy(false);
          }
        }}
      >
        <Notice error={error()} />
        <label>
          Participant IDs
          <textarea
            required
            rows={4}
            placeholder={"alex\nassistant:your-org"}
            value={participants()}
            onInput={(e) => {
              setParticipants(e.currentTarget.value);
              key = crypto.randomUUID();
            }}
          />
        </label>
        <p class="footnote">
          Separate IDs with commas or new lines. You are included automatically.
          Participants need to have signed into DM or been shared with DM by
          IAM.
        </p>
        <div class="form-actions">
          <button class="button" type="button" onClick={props.close}>
            Cancel
          </button>
          <button class="button primary" type="submit" disabled={busy()}>
            {busy() ? "Creating…" : "Start conversation"}
          </button>
        </div>
      </form>
    </Modal>
  );
}
function BundleEditor(props: {
  session: Session;
  messages: Message[];
  conversation: string;
  close: () => void;
  done: (
    value: Bundle,
    generation: number | null,
    selected: Message[],
  ) => Promise<void>;
}) {
  const [content, setContent] = createSignal<MessageCreate>(emptyContent()),
    [busy, setBusy] = createSignal(false);
  let key = crypto.randomUUID(),
    alive = true;
  onCleanup(() => {
    alive = false;
  });
  return (
    <Modal
      title="Create a message bundle"
      subtitle={`${props.messages.length} original messages will remain intact.`}
      close={props.close}
      wide
    >
      <div class="bundle-preview">
        <For each={props.messages}>
          {(m) => (
            <p>
              <strong>{m.sender.id}</strong> {previewOf(m)}
            </p>
          )}
        </For>
      </div>
      <Composer
        value={content()}
        change={(v) => {
          setContent(v);
          key = crypto.randomUUID();
        }}
        busy={busy()}
        send={async () => {
          setBusy(true);
          try {
            const display = { ...content(), text: content().text || undefined };
            validateMessage(display);
            const selected = [...props.messages];
            const conversation = props.conversation;
            const idempotencyKey = key;
            const generation = await getGeneration(props.session);
            if (!alive) return;
            if (generation === undefined)
              throw new Error("Connect before creating a message bundle.");
            const result = await api<Bundle>(
              `/conversations/${encodeURIComponent(conversation)}/bundles`,
              {
                method: "POST",
                session: props.session,
                idempotencyKey,
                generation,
                body: {
                  message_ids: selected.map((message) => message.id),
                  display_message: display,
                },
              },
            );
            if (alive) await props.done(result, generation, selected);
          } finally {
            if (alive) setBusy(false);
          }
        }}
      />
    </Modal>
  );
}
