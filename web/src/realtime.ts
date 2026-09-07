import type {
  Activity,
  Message,
  MessageStatus,
  ReceiptStatus,
  RealtimeState,
  ServerFrame,
  Session,
} from "./models";
import { ApiError, api, gatewayOrigin } from "./api";
import {
  adoptGeneration,
  authenticated,
  cacheMessage,
  commitDelivery,
  completeReceipt,
  getCursor,
  getDeviceId,
  getGeneration,
  pendingReceipts,
  queueReceipt,
  StorageError,
  scopeFor,
  cachedMessage,
  broadcastSource,
  broadcastUpdate,
  type StorageUpdate,
} from "./storage";

type Callback<T extends unknown[]> = (...args: T) => void | Promise<void>;
export interface RealtimeHandlers {
  onMessage: Callback<[message: Message]>;
  onReceipt: Callback<[messageId: string, status: MessageStatus]>;
  onState: Callback<[state: RealtimeState]>;
  onReset: Callback<[generation: number | null]>;
  onReady?: Callback<[generation: number | null]>;
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
/** Cookie-authenticated browser transport. Message persistence precedes transport ACKs. */
export function connectRealtime(
  session: Session,
  handlers: RealtimeHandlers,
): RealtimeConnection {
  authenticated(session);
  const actorId = session.actor.id;
  const profileId = session.profile_id;
  const scope = scopeFor(session);
  let socket: WebSocket | undefined;
  let stopped = false;
  let terminal = false;
  let connected = false;
  let attempt = 0;
  let connectionEpoch = 0;
  let reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  let generation: number | null | undefined;
  let deviceId = "";
  let processing = Promise.resolve();
  let pendingBytes = 0;
  let pendingFrames = 0;
  let lastReceivedAt = Date.now();
  const MAX_PENDING_BYTES = 128 * 1024 * 1024;
  const MAX_PENDING_FRAMES = 32;
  // The backend sends an application ping every 30s and closes after 120s
  // without a matching pong. Give the server the first chance to reap a dead
  // socket, and avoid tearing down healthy sockets while a background tab's
  // timers are throttled.
  const CLIENT_STALE_TIMEOUT_MS = 150_000;
  const sentReceipts = new Set<string>();
  let receiptFlush: Promise<void> | undefined;
  let initialReplay: { cursor: number; requestedAt: number } | undefined;
  let gapReplay:
    | {
        cursor: number;
        requestedAt: number;
        attempts: number;
        reported: boolean;
      }
    | undefined;
  function requestGapReplay(cursor: number, target = socket): void {
    if (!gapReplay || gapReplay.cursor !== cursor) {
      const pending =
        initialReplay?.cursor === cursor ? initialReplay : undefined;
      gapReplay = {
        cursor,
        requestedAt: pending?.requestedAt ?? 0,
        attempts: pending ? 1 : 0,
        reported: false,
      };
      initialReplay = undefined;
    }
    if (gapReplay.requestedAt && Date.now() - gapReplay.requestedAt < 60_000)
      return;
    if (gapReplay.attempts >= 3) {
      if (!gapReplay.reported) {
        gapReplay.reported = true;
        report(
          new StorageError(
            "delivery_gap",
            `Delivery ${cursor + 1} is still unavailable after replay. Newer events are safely stored, but are not acknowledged past this gap.`,
          ),
        );
      }
      return;
    }
    if (
      send(
        { type: "resume", actor_id: actorId, after_sequence: cursor },
        target,
      )
    ) {
      gapReplay.requestedAt = Date.now();
      gapReplay.attempts++;
      state("reconnecting");
    }
  }

  function notify<T extends unknown[]>(
    callback: Callback<T> | undefined,
    ...args: T
  ): void {
    if (!callback) return;
    try {
      void Promise.resolve(callback(...args)).catch(report);
    } catch (error) {
      report(error);
    }
  }
  function state(value: RealtimeState): void {
    notify(handlers.onState, value);
  }
  function report(error: unknown): void {
    try {
      void Promise.resolve(
        handlers.onError?.(
          error instanceof Error ? error : new Error(String(error)),
        ),
      ).catch(() => undefined);
    } catch {
      /* A view error cannot break durable transport processing. */
    }
  }
  function send(frame: unknown, target = socket): boolean {
    if (!target || target.readyState !== WebSocket.OPEN) return false;
    target.send(JSON.stringify(frame));
    return true;
  }
  function stopWithError(error: Error, unauthorized = false): void {
    terminal = true;
    connected = false;
    connectionEpoch++;
    if (reconnectTimer) clearTimeout(reconnectTimer);
    socket?.close(
      1000,
      unauthorized ? "session-ended" : "local-storage-or-protocol-error",
    );
    state(unauthorized ? "unauthorized" : "error");
    report(error);
  }
  async function flushReceipts(): Promise<void> {
    if (receiptFlush) return receiptFlush;
    receiptFlush = (async () => {
      if (!connected || generation === undefined) return;
      for (const receipt of await pendingReceipts(session)) {
        if (receipt.generation !== generation) continue;
        const marker = `${receipt.id}:${receipt.status}`;
        if (sentReceipts.has(marker)) continue;
        if (
          send({
            type: "receipt",
            actor_id: receipt.actor_id,
            device_id: receipt.device_id,
            conversation_id: receipt.conversation_id,
            message_id: receipt.message_id,
            status: receipt.status,
          })
        )
          sentReceipts.add(marker);
      }
    })();
    try {
      await receiptFlush;
    } finally {
      receiptFlush = undefined;
    }
  }
  async function processFrame(
    frame: ServerFrame,
    target: WebSocket,
    epoch: number,
  ): Promise<void> {
    if (epoch !== connectionEpoch || stopped) return;
    if (frame.type === "ready") {
      if (
        frame.protocol_version !== 2 ||
        frame.actors.length !== 1 ||
        frame.actors[0] !== actorId
      )
        throw new StorageError(
          "protocol_mismatch",
          "The server advertised an unexpected protocol or actor.",
        );
      const changed = await adoptGeneration(session, frame.testing_generation);
      generation = frame.testing_generation;
      if (changed) {
        notify(handlers.onReset, generation);
        broadcastUpdate({ scope, kind: "reset", generation });
      }
      if (epoch !== connectionEpoch || target.readyState !== WebSocket.OPEN)
        return;
      // The server's ACK is not evidence that this browser committed a message.
      // Resume only from our own durable cursor, even when the server is ahead.
      const cursor = await getCursor(session, actorId);
      if (epoch !== connectionEpoch || target.readyState !== WebSocket.OPEN)
        return;
      send(
        { type: "resume", actor_id: actorId, after_sequence: cursor },
        target,
      );
      // Ready already starts a replay request. A queued future event must not
      // immediately send a second Resume that resets the server's sent cursor.
      initialReplay = { cursor, requestedAt: Date.now() };
      connected = true;
      attempt = 0;
      state("connected");
      notify(handlers.onReady, generation);
      await flushReceipts();
      return;
    }
    if (frame.type === "error") {
      if (
        frame.recoverable &&
        frame.code === "validation_error" &&
        frame.message === "cannot ACK a sequence not emitted on this connection"
      ) {
        // Resume and ACK travel independently of buffered server frames. An ACK
        // for a previously emitted frame can arrive after Resume reset the
        // server cursor. Keep our durable cursor; subsequent replay frames ACK
        // their own positions again as the server catches up.
        return;
      }
      const error = new ApiError(
        frame.code === "unauthorized" ? 401 : 0,
        frame.code,
        frame.message,
      );
      if (
        !frame.recoverable ||
        frame.code === "unauthorized" ||
        frame.code === "forbidden"
      )
        stopWithError(error, frame.code === "unauthorized");
      else report(error);
      return;
    }
    if (generation === undefined || !connected)
      throw new StorageError(
        "missing_ready",
        "The server sent a delivery before initializing the stream.",
      );
    if (frame.type === "message" || frame.type === "receipt") {
      if (frame.actor_id !== actorId)
        throw new StorageError(
          "wrong_actor",
          "A delivery targeted a different profile.",
        );
      const result = await commitDelivery(session, generation, frame, deviceId);
      if (epoch !== connectionEpoch || stopped) return;
      if (result.gap) requestGapReplay(result.cursor, target);
      else {
        initialReplay = undefined;
        if (gapReplay) {
          gapReplay = undefined;
          state("connected");
        }
      }
      // A Resume may have reset the server's sent cursor. Bound each ACK by
      // this frame, rather than by the maximum seen before the replay reset.
      if (epoch === connectionEpoch)
        send(
          {
            type: "ack",
            actor_id: actorId,
            through_sequence: Math.min(result.cursor, frame.delivery_sequence),
          },
          target,
        );
      if (!result.duplicate) {
        if (frame.type === "message" && result.message) {
          notify(handlers.onMessage, result.message);
          broadcastUpdate({
            scope,
            kind: "message",
            message_id: result.message.id,
          });
        }
        if (frame.type === "receipt") {
          notify(
            handlers.onReceipt,
            frame.message_id,
            result.status ?? frame.status,
          );
          broadcastUpdate({
            scope,
            kind: "receipt",
            message_id: frame.message_id,
            status: result.status ?? frame.status,
          });
        }
      }
      await flushReceipts();
    } else if (frame.type === "receipt_recorded") {
      await completeReceipt(
        session,
        generation,
        frame.message_id,
        frame.status,
      );
    } else if (frame.type === "message_accepted") {
      notify(
        handlers.onMessage,
        await cacheMessage(session, frame.message, generation),
      );
    }
  }
  function scheduleReconnect(): void {
    if (stopped || terminal || reconnectTimer) return;
    if (!navigator.onLine) {
      state("offline");
      return;
    }
    state(attempt ? "reconnecting" : "connecting");
    const backoffAttempt = Math.min(attempt++, 6);
    const delay =
      Math.min(30_000, 500 * 2 ** backoffAttempt) +
      Math.floor(Math.random() * 250);
    reconnectTimer = setTimeout(() => {
      reconnectTimer = undefined;
      void open();
    }, delay);
  }
  async function open(): Promise<void> {
    if (stopped || terminal) return;
    try {
      const epoch = ++connectionEpoch;
      deviceId = await getDeviceId(session);
      const previousGeneration = await getGeneration(session);
      if (stopped || terminal || epoch !== connectionEpoch) return;
      const url = new URL("/api/ws", gatewayOrigin());
      url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
      url.searchParams.set("profile_id", profileId);
      url.searchParams.set("device_id", deviceId);
      if (previousGeneration != null)
        url.searchParams.set("testing_generation", String(previousGeneration));
      const target = new WebSocket(url);
      socket = target;
      generation = undefined;
      connected = false;
      sentReceipts.clear();
      gapReplay = undefined;
      initialReplay = undefined;
      target.onopen = () => {
        lastReceivedAt = Date.now();
      };
      target.onmessage = (event) => {
        if (epoch !== connectionEpoch || stopped || terminal) return;
        lastReceivedAt = Date.now();
        if (typeof event.data !== "string") {
          stopWithError(new Error("Unexpected binary WebSocket frame."));
          return;
        }
        // Bound queued decoded messages while IndexedDB is committing a large
        // payload. The durable server stream will replay anything not ACKed.
        const bytes = event.data.length * 2;
        let frame: ServerFrame | undefined;
        if (event.data.length < 4096) {
          try {
            frame = JSON.parse(event.data) as ServerFrame;
          } catch {
            stopWithError(new Error("Invalid realtime JSON."));
            return;
          }
          if (frame?.type === "ping") {
            send({ type: "pong", ping_id: frame.ping_id }, target);
            return;
          }
        }
        if (
          pendingFrames >= MAX_PENDING_FRAMES ||
          (pendingFrames > 0 && pendingBytes + bytes > MAX_PENDING_BYTES)
        ) {
          target.close(1013, "local-backpressure");
          return;
        }
        try {
          frame ??= JSON.parse(event.data) as ServerFrame;
        } catch {
          stopWithError(new Error("Invalid realtime JSON."));
          return;
        }
        if (!frame || typeof frame !== "object" || !("type" in frame)) {
          stopWithError(new Error("Invalid realtime frame."));
          return;
        }
        const parsedFrame = frame;
        pendingFrames++;
        pendingBytes += bytes;
        processing = processing
          .then(() => processFrame(parsedFrame, target, epoch))
          .catch((error) => {
            if (epoch === connectionEpoch && !stopped)
              stopWithError(
                error instanceof Error ? error : new Error(String(error)),
              );
          })
          .finally(() => {
            pendingFrames--;
            pendingBytes -= bytes;
          });
      };
      target.onerror = () => {
        /* close carries retry policy; browsers conceal upgrade response bodies. */
      };
      target.onclose = (event) => {
        if (epoch !== connectionEpoch || stopped || terminal) return;
        const recoveryEpoch = ++connectionEpoch;
        connected = false;
        // Reflect the transport transition immediately. The session probe
        // below can take a few seconds when the network is degraded; leaving
        // the UI in "Connected" during that window makes sends look stuck.
        state(navigator.onLine ? "reconnecting" : "offline");
        if (
          event.code === 4001 &&
          !event.reason.includes("testing-environment-changed")
        ) {
          stopWithError(
            new ApiError(
              401,
              "unauthorized",
              "This realtime session is no longer authorized.",
            ),
            true,
          );
          return;
        }
        void api<Session>("/api/session", { session })
          .then((current) => {
            if (recoveryEpoch !== connectionEpoch || stopped || terminal)
              return;
            if (
              !current.profiles?.some(
                (profile) => profile.profile_id === session.profile_id,
              )
            )
              stopWithError(
                new ApiError(
                  401,
                  "unauthorized",
                  "This profile has signed out.",
                ),
                true,
              );
            else scheduleReconnect();
          })
          .catch((error) => {
            if (recoveryEpoch !== connectionEpoch || stopped || terminal)
              return;
            if (error instanceof ApiError && error.status === 401)
              stopWithError(error, true);
            else scheduleReconnect();
          });
      };
    } catch (error) {
      stopWithError(error instanceof Error ? error : new Error(String(error)));
    }
  }
  function online(): void {
    if (
      !stopped &&
      !terminal &&
      (!socket || socket.readyState === WebSocket.CLOSED)
    )
      scheduleReconnect();
  }
  function offline(): void {
    connected = false;
    state("offline");
    socket?.close(1000, "offline");
  }
  function visibility(): void {
    if (document.visibilityState === "visible") {
      if (
        socket?.readyState === WebSocket.OPEN &&
        Date.now() - lastReceivedAt > CLIENT_STALE_TIMEOUT_MS
      )
        socket.close(1000, "stale-connection");
      else online();
    } else if (connected)
      send({ type: "presence", actor_id: actorId, activity: null });
  }
  function unauthorized(event: Event): void {
    const profile = (event as CustomEvent<{ profile_id?: string }>).detail
      ?.profile_id;
    if (!profile || profile === session.profile_id)
      stopWithError(
        new ApiError(
          401,
          "unauthorized",
          "Sign in again to resume this profile.",
        ),
        true,
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
        stopped
      )
        return;
      if (update.kind === "message" && typeof update.message_id === "string")
        void cachedMessage(session, update.message_id)
          .then((message) => {
            if (message) notify(handlers.onMessage, message);
          })
          .catch(report);
      else if (update.kind === "receipt" && update.message_id && update.status)
        notify(handlers.onReceipt, update.message_id, update.status);
      else if (update.kind === "outbox")
        window.dispatchEvent(
          new CustomEvent("dm:outbox", { detail: { scope } }),
        );
      else if (update.kind === "reset" && update.generation !== undefined) {
        notify(handlers.onReset, update.generation);
        connectionEpoch++;
        connected = false;
        terminal = false;
        socket?.close(1000, "environment-changed");
        void open();
      }
    };
  const watchdog = setInterval(() => {
    if (
      connected &&
      document.visibilityState === "visible" &&
      Date.now() - lastReceivedAt > CLIENT_STALE_TIMEOUT_MS
    )
      socket?.close(1000, "heartbeat-expired");
    else if (connected) {
      if (gapReplay) requestGapReplay(gapReplay.cursor);
      sentReceipts.clear();
      void flushReceipts().catch(stopWithError);
    }
  }, 30_000);
  window.addEventListener("online", online);
  window.addEventListener("offline", offline);
  window.addEventListener("dm:unauthorized", unauthorized);
  document.addEventListener("visibilitychange", visibility);
  state(navigator.onLine ? "connecting" : "offline");
  if (navigator.onLine) void open();
  return {
    close() {
      stopped = true;
      connected = false;
      connectionEpoch++;
      if (reconnectTimer) clearTimeout(reconnectTimer);
      clearInterval(watchdog);
      channel?.close();
      socket?.close(1000, "client-closed");
      window.removeEventListener("online", online);
      window.removeEventListener("offline", offline);
      window.removeEventListener("dm:unauthorized", unauthorized);
      document.removeEventListener("visibilitychange", visibility);
      state("closed");
    },
    reconnect() {
      if (stopped) return;
      terminal = false;
      attempt = 0;
      connectionEpoch++;
      socket?.close(1000, "reconnect");
      if (reconnectTimer) clearTimeout(reconnectTimer);
      reconnectTimer = undefined;
      void open();
    },
    presence(activity, requestedActor = actorId) {
      if (requestedActor !== actorId)
        throw new ApiError(
          403,
          "forbidden",
          "Presence must belong to this profile.",
        );
      if (connected) send({ type: "presence", actor_id: actorId, activity });
    },
    async receipt(conversationId, messageId, status, requestedActor = actorId) {
      if (requestedActor !== actorId)
        throw new ApiError(
          403,
          "forbidden",
          "Receipt must belong to this profile.",
        );
      const fence = generation ?? (await getGeneration(session));
      if (fence === undefined)
        throw new StorageError(
          "generation_unknown",
          "Connect before recording receipts for this testing environment.",
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
