import type {
  Activity,
  AppConfig,
  Conversation,
  IamInfo,
  Message,
  MessageStatus,
  Page,
  ReceiptStatus,
  RealtimeState,
  Session,
  SyncPage,
} from "./models";
import { ApiError, api, pathSegment, queryString } from "./api";
import { localMessageKey, wireMessageId } from "./wire.ts";
import { watchTing, type TingStatus } from "./ting";
import {
  adoptGeneration,
  authenticated,
  beginSyncSnapshot,
  broadcastSource,
  broadcastUpdate,
  cachedMessage,
  commitSyncPage,
  completeReceipt,
  getDeviceId,
  getGeneration,
  getSyncCheckpoint,
  pendingReceipts,
  queueReceipt,
  scopeFor,
  StorageError,
  type HydratedSyncMessage,
  type StorageUpdate,
  type SyncCheckpoint,
} from "./storage";

type Callback<T extends unknown[]> = (...args: T) => void | Promise<void>;
export interface RealtimeHandlers {
  onMessage: Callback<[message: Message]>;
  onReceipt: Callback<[messageId: string, status: MessageStatus]>;
  onState: Callback<[state: RealtimeState]>;
  onReset: Callback<[generation: number | null]>;
  onReady?: Callback<[generation: number | null]>;
  onTing?: Callback<[status: TingStatus]>;
  onSnapshot?: Callback<[]>;
  onInaccessible?: Callback<[messageId: string]>;
  onError?: Callback<[error: Error]>;
}
export interface RealtimeConnection {
  close(): void;
  reconnect(): void;
  presence(activity: Activity | null, actorId?: string): void;
  receipt(
    conversationId: string,
    messageId: string,
    status: ReceiptStatus,
    actorId?: string,
  ): Promise<void>;
}
const sameContext = (
  session: Session,
  page: {
    testing_environment_id: string | null;
    testing_generation: number | null;
  },
  generation: number | null,
) =>
  (session.testing_environment_id ?? null) === page.testing_environment_id &&
  page.testing_generation === generation;

/** Ting supplies invalidation hints after checking its session context. DM HTTP authorization is
 * required for all content. No DM socket, Ting ACK or implicit DM read exists. */
export function connectRealtime(
  session: Session,
  handlers: RealtimeHandlers,
): RealtimeConnection {
  authenticated(session);
  const actor = session.actor,
    organization = session.organization_id,
    scope = scopeFor(session);
  let stopped = false,
    terminal = false,
    ready = false,
    epoch = 0,
    dirty = false;
  let generation: number | null | undefined,
    deviceId = "";
  let abort = new AbortController();
  let running: Promise<void> | undefined,
    receiptFlush: Promise<void> | undefined;
  let ting: ReturnType<typeof watchTing> | undefined;
  let retry: ReturnType<typeof setTimeout> | undefined,
    attempts = 0;
  let activity: Activity | null = null,
    presenceRunning: Promise<void> | undefined;
  const valid = (run: number) => !stopped && !terminal && epoch === run;
  function report(error: unknown) {
    try {
      void Promise.resolve(
        handlers.onError?.(
          error instanceof Error ? error : new Error(String(error)),
        ),
      ).catch(() => {});
    } catch {
      /* View failure does not advance cursors. */
    }
  }
  function notify<T extends unknown[]>(
    callback: Callback<T> | undefined,
    ...args: T
  ) {
    try {
      void Promise.resolve(callback?.(...args)).catch(report);
    } catch (error) {
      report(error);
    }
  }
  const state = (value: RealtimeState) => notify(handlers.onState, value);
  function stop(error: Error) {
    terminal = true;
    ready = false;
    ++epoch;
    abort.abort();
    ting?.close();
    if (retry) clearTimeout(retry);
    retry = undefined;
    state(
      error instanceof ApiError && error.status === 401
        ? "unauthorized"
        : "error",
    );
    report(error);
  }
  function schedule() {
    if (stopped || terminal || retry) return;
    state(navigator.onLine ? "reconnecting" : "offline");
    if (!navigator.onLine) return;
    retry = setTimeout(
      () => {
        retry = undefined;
        requestSync();
      },
      Math.min(30_000, 500 * 2 ** Math.min(attempts++, 6)),
    );
  }
  function failed(error: unknown) {
    if (
      stopped ||
      terminal ||
      (error instanceof DOMException && error.name === "AbortError")
    )
      return;
    if (error instanceof ApiError && error.status === 401) {
      stop(error);
      return;
    }
    ready = false;
    if (
      error instanceof StorageError &&
      error.code !== "sync_conflict" &&
      error.code !== "environment_changed"
    ) {
      stop(error);
      return;
    }
    report(error);
    schedule();
  }
  function ensure(run: number) {
    if (!valid(run))
      throw new DOMException(
        "This profile operation was superseded.",
        "AbortError",
      );
  }
  async function request<T>(
    path: string,
    run: number,
    options: Parameters<typeof api>[1] = {},
  ): Promise<T> {
    ensure(run);
    try {
      const result = await api<T>(path, {
        generation,
        ...options,
        session,
        signal: abort.signal,
      });
      ensure(run);
      return result;
    } catch (error) {
      // A late rejection from an abandoned profile/connection cannot sign out
      // the replacement connection after an explicit reconnect.
      ensure(run);
      throw error;
    }
  }
  function validatePage(page: SyncPage, fence: number | null) {
    if (!sameContext(session, page, fence))
      throw new StorageError(
        "environment_changed",
        "DM sync returned another testing context.",
      );
    if (
      typeof page.cursor !== "string" ||
      !page.cursor ||
      !Array.isArray(page.events) ||
      typeof page.has_more !== "boolean" ||
      !Number.isSafeInteger(page.upper_sequence) ||
      page.upper_sequence < 0
    )
      throw new StorageError(
        "invalid_sync",
        "DM returned an invalid sync page.",
      );
    let previous = -1;
    for (const event of page.events) {
      if (
        !event ||
        typeof event.event_id !== "string" ||
        !event.event_id ||
        !Number.isSafeInteger(event.sequence) ||
        event.sequence <= previous ||
        event.sequence > page.upper_sequence ||
        !["message", "message_status"].includes(event.type) ||
        typeof event.conversation_id !== "string" ||
        !event.conversation_id ||
        typeof event.message_id !== "string" ||
        !event.message_id
      )
        throw new StorageError(
          "invalid_sync",
          "DM returned an invalid message reference.",
        );
      previous = event.sequence;
    }
  }
  async function committed(
    messages: Message[],
    run: number,
    fence: number | null,
  ) {
    ensure(run);
    for (const message of messages) {
      notify(handlers.onMessage, message);
      notify(handlers.onReceipt, message.id, message.status);
      broadcastUpdate({
        scope,
        kind: "message",
        message_id: message.id,
        generation: fence,
      });
    }
  }
  async function snapshot(
    run: number,
    fence: number | null,
  ): Promise<SyncCheckpoint> {
    const anchor = await request<SyncPage>("/sync?reset=true&limit=100", run);
    validatePage(anchor, fence);
    if (anchor.events.length || anchor.has_more)
      throw new StorageError(
        "invalid_sync",
        "DM reset returned an invalid history boundary.",
      );
    const checkpoint = await beginSyncSnapshot(session, fence);
    ensure(run);
    ready = false;
    notify(handlers.onSnapshot);
    broadcastUpdate({ scope, kind: "snapshot", generation: fence });
    let cursor: string | null = null;
    const seen = new Set<string>();
    do {
      const page: Page<Conversation> = await request<Page<Conversation>>(
        `/conversations${queryString({ cursor, limit: 100 })}`,
        run,
      );
      if (!Array.isArray(page.items))
        throw new StorageError(
          "invalid_sync",
          "DM returned invalid conversation history.",
        );
      for (const conversation of page.items) {
        let historyCursor: string | null = null;
        const historySeen = new Set<string>();
        do {
          let history: Page<Message>;
          try {
            history = await request<Page<Message>>(
              `/conversations/${pathSegment(conversation.id)}/messages${queryString({ cursor: historyCursor, limit: 100, include_bundled_members: true })}`,
              run,
            );
          } catch (error) {
            if (error instanceof ApiError && [403, 404].includes(error.status))
              break;
            throw error;
          }
          if (
            !Array.isArray(history.items) ||
            history.items.some(
              (message) => message.conversation_id !== conversation.id,
            )
          )
            throw new StorageError(
              "invalid_sync",
              "DM history returned another conversation.",
            );
          const messages = await commitSyncPage(
            session,
            fence,
            checkpoint,
            checkpoint,
            history.items.map((message) => ({ message, deliver: true })),
            deviceId,
          );
          await committed(messages, run, fence);
          historyCursor = history.next_cursor;
          if (historyCursor && historySeen.has(historyCursor))
            throw new StorageError(
              "invalid_sync",
              "DM history pagination did not advance.",
            );
          if (historyCursor) historySeen.add(historyCursor);
        } while (historyCursor);
      }
      cursor = page.next_cursor;
      if (cursor && seen.has(cursor))
        throw new StorageError(
          "invalid_sync",
          "DM conversation pagination did not advance.",
        );
      if (cursor) seen.add(cursor);
    } while (cursor);
    const next = { cursor: anchor.cursor };
    await commitSyncPage(session, fence, checkpoint, next, [], deviceId);
    ensure(run);
    return next;
  }
  async function reconcile(run: number) {
    const info = await request<IamInfo>("/iam", run, { generation: null });
    if (
      (session.testing_environment_id ?? null) !==
        info.testing_environment_id ||
      (session.testing_environment_id
        ? !Number.isSafeInteger(info.testing_generation) ||
          (info.testing_generation ?? 0) < 1
        : info.testing_generation !== null)
    )
      throw new StorageError(
        "invalid_generation",
        "DM discovery did not confirm this profile’s environment.",
      );
    const changed = await adoptGeneration(session, info.testing_generation);
    ensure(run);
    const hadGeneration = generation !== undefined;
    const firstReady =
      generation === undefined || generation !== info.testing_generation;
    generation = info.testing_generation;
    const fence = generation;
    if (firstReady && hadGeneration) {
      ting?.close();
      ting = undefined;
    }
    if (changed || (firstReady && hadGeneration))
      notify(handlers.onReset, fence);
    if (changed) broadcastUpdate({ scope, kind: "reset", generation: fence });
    deviceId ||= await getDeviceId(session);
    ensure(run);
    let checkpoint = await getSyncCheckpoint(session, fence);
    ensure(run);
    if (!checkpoint?.cursor || checkpoint.snapshot_id)
      checkpoint = await snapshot(run, fence);
    let resetUsed = false;
    const seen = new Set<string>();
    while (valid(run)) {
      let page: SyncPage;
      try {
        page = await request<SyncPage>(
          `/sync${queryString({ cursor: checkpoint.cursor, limit: 100 })}`,
          run,
        );
      } catch (error) {
        if (
          !resetUsed &&
          error instanceof ApiError &&
          error.code === "sync_reset_required"
        ) {
          resetUsed = true;
          checkpoint = await snapshot(run, fence);
          continue;
        }
        throw error;
      }
      validatePage(page, fence);
      if (
        page.has_more &&
        (page.cursor === checkpoint.cursor || seen.has(page.cursor))
      )
        throw new StorageError(
          "invalid_sync",
          "DM sync pagination did not advance.",
        );
      seen.add(page.cursor);
      const hydrated: HydratedSyncMessage[] = [],
        inaccessible: string[] = [];
      // Multiple immutable events can point to the same mutable message. One
      // authorized read per page gives that message one unambiguous outcome.
      const references = new Map<
        string,
        { conversation_id: string; message_id: string; deliver: boolean }
      >();
      for (const event of page.events) {
        const id = JSON.stringify([event.conversation_id, event.message_id]);
        const prior = references.get(id);
        references.set(id, {
          conversation_id: event.conversation_id,
          message_id: event.message_id,
          deliver: event.type === "message" || prior?.deliver === true,
        });
      }
      for (const event of references.values()) {
        try {
          const message = await request<Message>(
            `/conversations/${pathSegment(event.conversation_id)}/messages/${pathSegment(event.message_id)}`,
            run,
          );
          if (
            message.conversation_id !== event.conversation_id ||
            message.id !==
              localMessageKey(event.conversation_id, event.message_id)
          )
            throw new StorageError(
              "invalid_sync",
              "DM returned a message with another identity.",
            );
          hydrated.push({ message, deliver: event.deliver });
        } catch (error) {
          if (error instanceof ApiError && [403, 404].includes(error.status))
            inaccessible.push(
              localMessageKey(event.conversation_id, event.message_id),
            );
          else throw error;
        }
      }
      const next = { cursor: page.cursor };
      const messages = await commitSyncPage(
        session,
        fence,
        checkpoint,
        next,
        hydrated,
        deviceId,
        inaccessible,
      );
      await committed(messages, run, fence);
      for (const id of inaccessible) {
        notify(handlers.onInaccessible, id);
        broadcastUpdate({
          scope,
          kind: "inaccessible",
          message_id: id,
          generation: fence,
        });
      }
      checkpoint = next;
      if (!page.has_more) break;
    }
    ensure(run);
    attempts = 0;
    const becameReady = !ready || firstReady;
    ready = true;
    state("connected");
    if (becameReady) notify(handlers.onReady, fence);
    await flushReceipts();
    void renewPresence().catch(failed);
    if (!ting) {
      try {
        const config = await request<AppConfig>("/api/config", run);
        ting = watchTing(
          config.ting_browser_origin || "https://ting.teamofsilicons.com",
          actor,
          organization,
          {
            testing_environment_id: info.testing_environment_id,
            testing_generation: fence,
          },
          () => {
            if (valid(run) && generation === fence) requestSync();
          },
          (value) => {
            if (valid(run) && generation === fence)
              notify(handlers.onTing, value);
          },
        );
      } catch (error) {
        report(error);
      }
    }
  }
  function requestSync() {
    if (stopped || terminal || !navigator.onLine) return;
    dirty = true;
    if (running) return;
    const run = epoch;
    running = (async () => {
      while (dirty && valid(run)) {
        dirty = false;
        await reconcile(run);
      }
    })()
      .catch(failed)
      .finally(() => {
        running = undefined;
        if (dirty && valid(run)) requestSync();
      });
  }
  async function flushReceipts(): Promise<void> {
    if (receiptFlush) return receiptFlush;
    if (!ready || generation === undefined || stopped || terminal) return;
    const run = epoch,
      fence = generation;
    receiptFlush = (async () => {
      for (const receipt of await pendingReceipts(session)) {
        ensure(run);
        if (receipt.generation !== fence) continue;
        try {
          await request(
            `/conversations/${pathSegment(receipt.conversation_id)}/messages/${pathSegment(wireMessageId(receipt.message_id))}/receipts`,
            run,
            {
              method: "POST",
              generation: fence,
              body: { status: receipt.status, device_id: receipt.device_id },
            },
          );
        } catch (error) {
          // Deleted or no-longer-authorized targets cannot hold unrelated
          // receipts forever. This only removes local pending work; no receipt
          // success/read is synthesized for the rejected target.
          if (
            !(error instanceof ApiError) ||
            ![403, 404].includes(error.status)
          )
            throw error;
        }
        ensure(run);
        await completeReceipt(
          session,
          fence,
          receipt.message_id,
          receipt.status,
        );
      }
    })();
    try {
      await receiptFlush;
    } finally {
      receiptFlush = undefined;
    }
  }
  async function renewPresence() {
    if (
      !ready ||
      generation === undefined ||
      stopped ||
      terminal ||
      presenceRunning
    )
      return;
    const run = epoch;
    presenceRunning = request(
      `/presence/devices/${pathSegment(deviceId)}`,
      run,
      {
        method: "PUT",
        body: {
          activity: document.visibilityState === "hidden" ? null : activity,
        },
      },
    ).then(() => {});
    try {
      await presenceRunning;
    } finally {
      presenceRunning = undefined;
    }
  }
  function online() {
    requestSync();
    ting?.reconnect();
  }
  function offline() {
    ready = false;
    state("offline");
  }
  function visibility() {
    if (document.visibilityState === "visible") requestSync();
    void renewPresence().catch(failed);
  }
  function unauthorized(event: Event) {
    const profile = (event as CustomEvent<{ profile_id?: string }>).detail
      ?.profile_id;
    if (!profile || profile === session.profile_id)
      stop(
        new ApiError(
          401,
          "unauthorized",
          "Sign in again to resume this profile.",
        ),
      );
  }
  const channel =
    typeof BroadcastChannel === "undefined"
      ? undefined
      : new BroadcastChannel("silicon-dm-browser-updates");
  if (channel)
    channel.onmessage = (event) => {
      const update = event.data as StorageUpdate;
      if (
        !update ||
        update.scope !== scope ||
        update.source === broadcastSource ||
        stopped ||
        terminal
      )
        return;
      if (["message", "inaccessible", "snapshot"].includes(update.kind)) {
        const run = epoch,
          fence = generation;
        if (fence === undefined || update.generation !== fence) return;
        const current = () => valid(run) && generation === fence;
        void (async () => {
          const message =
            update.kind === "message" && update.message_id
              ? await cachedMessage(session, update.message_id, fence)
              : undefined;
          // Another tab can adopt the next generation before this tab receives
          // its reset hint. Check durable state as well as our connection epoch.
          if (
            !current() ||
            (await getGeneration(session)) !== fence ||
            !current()
          )
            return;
          if (update.kind === "message" && message)
            notify(handlers.onMessage, message);
          else if (update.kind === "inaccessible" && update.message_id)
            notify(handlers.onInaccessible, update.message_id);
          else if (update.kind === "snapshot") {
            ready = false;
            notify(handlers.onSnapshot);
            requestSync();
          }
        })().catch((error) => {
          if (
            current() &&
            !(
              error instanceof StorageError &&
              error.code === "environment_changed"
            )
          )
            report(error);
        });
      } else if (update.kind === "outbox")
        window.dispatchEvent(
          new CustomEvent("dm:outbox", { detail: { scope } }),
        );
      else if (update.kind === "reset") {
        ready = false;
        requestSync();
      }
    };
  const watchdog = setInterval(() => {
    if (document.visibilityState === "visible") requestSync();
  }, 30_000);
  window.addEventListener("online", online);
  window.addEventListener("offline", offline);
  window.addEventListener("dm:unauthorized", unauthorized);
  document.addEventListener("visibilitychange", visibility);
  state(navigator.onLine ? "connecting" : "offline");
  requestSync();
  return {
    close() {
      if (deviceId && generation !== undefined && ready)
        void api(`/presence/devices/${pathSegment(deviceId)}`, {
          session,
          generation,
          method: "DELETE",
          keepalive: true,
        }).catch(() => {});
      stopped = true;
      ready = false;
      ++epoch;
      abort.abort();
      ting?.close();
      if (retry) clearTimeout(retry);
      clearInterval(watchdog);
      channel?.close();
      window.removeEventListener("online", online);
      window.removeEventListener("offline", offline);
      window.removeEventListener("dm:unauthorized", unauthorized);
      document.removeEventListener("visibilitychange", visibility);
      state("closed");
    },
    reconnect() {
      if (stopped) return;
      terminal = false;
      ready = false;
      ++epoch;
      abort.abort();
      abort = new AbortController();
      if (retry) clearTimeout(retry);
      retry = undefined;
      attempts = 0;
      ting?.close();
      ting = undefined;
      const previous = running;
      void Promise.resolve(previous).finally(requestSync);
    },
    presence(value, actorId = actor.id) {
      if (actorId !== actor.id)
        throw new ApiError(
          403,
          "forbidden",
          "Presence must belong to this profile.",
        );
      activity = value;
      void renewPresence().catch(failed);
    },
    async receipt(conversationId, messageId, status, actorId = actor.id) {
      if (actorId !== actor.id)
        throw new ApiError(
          403,
          "forbidden",
          "Receipt must belong to this profile.",
        );
      const fence = generation ?? (await getGeneration(session));
      if (fence === undefined)
        throw new StorageError(
          "generation_unknown",
          "Load this testing environment before recording receipts.",
        );
      await queueReceipt(
        session,
        fence,
        await getDeviceId(session),
        conversationId,
        messageId,
        status,
      );
      await flushReceipts();
    },
  };
}
