import type {
  AuthenticatedSession,
  DurableDelivery,
  Message,
  MessageCreate,
  MessageStatus,
  OutboxEntry,
  ReceiptStatus,
  Session,
} from "./models";

const DATABASE = "silicon-dm-browser";
const STORES = [
  "metadata",
  "events",
  "cursors",
  "messages",
  "outbox",
  "outboxBodies",
  "receipts",
] as const;
type StoreName = (typeof STORES)[number];
let opened: Promise<IDBDatabase> | undefined;
export class StorageError extends Error {
  constructor(
    public readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "StorageError";
  }
}
export function authenticated(
  session: Session,
): asserts session is AuthenticatedSession {
  if (
    !session.authenticated ||
    !session.actor ||
    !session.organization_id ||
    !session.profile_id
  )
    throw new StorageError(
      "unauthorized",
      "Sign in before using this profile.",
    );
}
export function scopeFor(session: Session): string {
  authenticated(session);
  return JSON.stringify([
    session.organization_id,
    session.actor.type,
    session.actor.id,
    session.testing_environment_id || "production",
  ]);
}
function key(scope: string, suffix: string): string {
  return `${scope}:${suffix}`;
}
function database(): Promise<IDBDatabase> {
  if (!opened)
    opened = new Promise((resolve, reject) => {
      if (!globalThis.indexedDB) {
        reject(
          new StorageError(
            "storage_unavailable",
            "This browser cannot persist messages.",
          ),
        );
        return;
      }
      const request = indexedDB.open(DATABASE, 4);
      request.onupgradeneeded = () => {
        for (const name of STORES) {
          if (request.result.objectStoreNames.contains(name)) continue;
          const store = request.result.createObjectStore(name, {
            keyPath: "id",
          });
          if (name !== "metadata")
            store.createIndex("scope", "scope", { unique: false });
        }
        const events = request.transaction!.objectStore("events");
        if (!events.indexNames.contains("actor_sequence"))
          events.createIndex(
            "actor_sequence",
            ["scope", "actor_id", "sequence"],
            { unique: true },
          );
        const messages = request.transaction!.objectStore("messages");
        if (!messages.indexNames.contains("conversation_sequence"))
          messages.createIndex("conversation_sequence", [
            "scope",
            "conversation_id",
            "message.sequence",
          ]);
      };
      request.onerror = () => {
        opened = undefined;
        reject(request.error);
      };
      request.onblocked = () =>
        reject(
          new StorageError(
            "storage_blocked",
            "Close older DM tabs to upgrade local storage.",
          ),
        );
      request.onsuccess = () => {
        const db = request.result;
        db.onversionchange = () => {
          db.close();
          opened = undefined;
        };
        resolve(db);
      };
    });
  return opened;
}
function request<T>(operation: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    operation.onsuccess = () => resolve(operation.result);
    operation.onerror = () => reject(operation.error);
  });
}
async function transaction<T>(
  names: StoreName[],
  mode: IDBTransactionMode,
  run: (tx: IDBTransaction) => Promise<T>,
): Promise<T> {
  const db = await database();
  const tx = db.transaction(names, mode);
  const completion = new Promise<void>((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () =>
      reject(
        tx.error ??
          new StorageError(
            "storage_error",
            "Local storage failed; delivery was not acknowledged.",
          ),
      );
    tx.onabort = () =>
      reject(
        tx.error ??
          new StorageError("storage_aborted", "Local storage did not commit."),
      );
  });
  // Attach a handler immediately so transaction aborts cannot become unhandled rejections.
  void completion.catch(() => undefined);
  try {
    const result = await run(tx);
    await completion;
    return result;
  } catch (error) {
    try {
      tx.abort();
    } catch {
      /* Already completed or aborted. */
    }
    throw error;
  }
}
async function metadata<T>(
  tx: IDBTransaction,
  id: string,
): Promise<T | undefined> {
  return (await request(tx.objectStore("metadata").get(id)))?.value as
    | T
    | undefined;
}
function setMetadata(tx: IDBTransaction, id: string, value: unknown): void {
  tx.objectStore("metadata").put({ id, value });
}
export async function getGeneration(
  session: Session,
): Promise<number | null | undefined> {
  if (!session.testing_environment_id) return null;
  return transaction(["metadata"], "readonly", (tx) =>
    metadata<number | null>(tx, key(scopeFor(session), "generation")),
  );
}
export async function getDeviceId(session: Session): Promise<string> {
  const scope = scopeFor(session);
  return transaction(["metadata"], "readwrite", async (tx) => {
    const id = key(scope, "device");
    const existing = await metadata<string>(tx, id);
    if (existing) return existing;
    const device = `browser-${crypto.randomUUID()}`;
    setMetadata(tx, id, device);
    return device;
  });
}
async function removeScope(
  tx: IDBTransaction,
  name: StoreName,
  scope: string,
): Promise<void> {
  const store = tx.objectStore(name);
  const keys = await request(
    store.index("scope").getAllKeys(IDBKeyRange.only(scope)),
  );
  keys.forEach((id) => store.delete(id));
}
export async function adoptGeneration(
  session: Session,
  generation: number | null,
): Promise<boolean> {
  const scope = scopeFor(session);
  if (
    session.testing_environment_id
      ? !Number.isSafeInteger(generation) || (generation ?? 0) < 1
      : generation !== null
  )
    throw new StorageError(
      "invalid_generation",
      "The server returned an invalid environment generation.",
    );
  return transaction([...STORES], "readwrite", async (tx) => {
    const old = await metadata<number | null>(tx, key(scope, "generation"));
    const changed = old !== undefined && old !== generation;
    if (changed) {
      await removeScope(tx, "events", scope);
      await removeScope(tx, "cursors", scope);
      await removeScope(tx, "messages", scope);
      await removeScope(tx, "receipts", scope);
      const rows = await request<OutboxEntry[]>(
        tx.objectStore("outbox").index("scope").getAll(scope),
      );
      for (const row of rows) {
        row.status = "fenced";
        row.error_code = "environment_changed";
        row.error =
          "This request belongs to an older testing generation. Create a new message after reviewing it.";
        row.updated_at = Date.now();
        tx.objectStore("outbox").put(row);
      }
    }
    setMetadata(tx, key(scope, "generation"), generation);
    return changed;
  });
}
async function requireGeneration(
  tx: IDBTransaction,
  session: Session,
  generation: number | null,
): Promise<void> {
  const current = await metadata<number | null>(
    tx,
    key(scopeFor(session), "generation"),
  );
  if (
    session.testing_environment_id &&
    (current === undefined || current !== generation)
  )
    throw new StorageError(
      "environment_changed",
      "Testing environment changed; this operation was not applied locally.",
    );
}
const statusOrder: Record<MessageStatus, number> = {
  waiting: 0,
  sent: 1,
  failed: 2,
  delivered: 3,
  read: 4,
};
export function mergeStatus(
  left: MessageStatus,
  right: MessageStatus,
): MessageStatus {
  return statusOrder[left] >= statusOrder[right] ? left : right;
}
export function mergeMessage(
  previous: Message | undefined,
  incoming: Message,
): Message {
  if (!previous) return incoming;
  const content =
    (previous.version ?? 1) > (incoming.version ?? 1) ? previous : incoming;
  return {
    ...content,
    // Bundle membership is immutable and is not a content revision. A replay
    // from before bundling must not erase membership learned more recently.
    bundle: incoming.bundle ?? previous.bundle,
    status: mergeStatus(previous.status, incoming.status),
    delivered_at: previous.delivered_at || incoming.delivered_at,
    read_at: previous.read_at || incoming.read_at,
  };
}
async function putMessage(
  tx: IDBTransaction,
  scope: string,
  message: Message,
): Promise<Message> {
  const store = tx.objectStore("messages");
  const id = key(scope, message.id);
  const previous = (await request(store.get(id)))?.message as
    | Message
    | undefined;
  let merged = mergeMessage(previous, message);
  const generation = await metadata<number | null>(
    tx,
    key(scope, "generation"),
  );
  const receipt = await metadata<MessageStatus>(
    tx,
    key(scope, `receipt:${generation ?? null}:${message.id}`),
  );
  if (receipt)
    merged = { ...merged, status: mergeStatus(merged.status, receipt) };
  store.put({
    id,
    scope,
    conversation_id: message.conversation_id,
    message: merged,
  });
  return merged;
}
export async function cacheMessage(
  session: Session,
  message: Message,
  generation: number | null,
): Promise<Message> {
  return transaction(["metadata", "messages"], "readwrite", async (tx) => {
    await requireGeneration(tx, session, generation);
    return putMessage(tx, scopeFor(session), message);
  });
}
export async function cachedMessages(
  session: Session,
  conversationId: string,
  limit = 50,
): Promise<Message[]> {
  const scope = scopeFor(session);
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100)
    throw new StorageError(
      "invalid_limit",
      "Cached history limit must be between 1 and 100.",
    );
  return transaction(["messages"], "readonly", async (tx) => {
    const result: Message[] = [];
    let bytes = 0;
    const range = IDBKeyRange.bound(
      [scope, conversationId, 0],
      [scope, conversationId, Number.MAX_SAFE_INTEGER],
    );
    await new Promise<void>((resolve, reject) => {
      const cursor = tx
        .objectStore("messages")
        .index("conversation_sequence")
        .openCursor(range, "prev");
      cursor.onerror = () => reject(cursor.error);
      cursor.onsuccess = () => {
        const current = cursor.result;
        if (!current) {
          resolve();
          return;
        }
        const message = current.value.message as Message;
        const nextBytes =
          2048 +
          2 *
            ((message.text?.length ?? 0) +
              (message.voice_transcript?.length ?? 0) +
              JSON.stringify(message.metadata ?? {}).length);
        if (result.length && bytes + nextBytes > 16 * 1024 * 1024) {
          resolve();
          return;
        }
        result.push(message);
        bytes += nextBytes;
        if (result.length >= limit || bytes >= 16 * 1024 * 1024) resolve();
        else current.continue();
      };
    });
    return result.reverse();
  });
}
export async function getCursor(
  session: Session,
  actorId: string,
): Promise<number> {
  const id = key(scopeFor(session), actorId);
  return transaction(
    ["cursors"],
    "readonly",
    async (tx) =>
      (await request(tx.objectStore("cursors").get(id)))?.sequence ?? 0,
  );
}
export interface DeliveryCommit {
  duplicate: boolean;
  gap: boolean;
  cursor: number;
  message?: Message;
  status?: MessageStatus;
}
export async function commitDelivery(
  session: Session,
  generation: number | null,
  delivery: DurableDelivery,
  deviceId: string,
): Promise<DeliveryCommit> {
  const scope = scopeFor(session);
  if (
    !Number.isSafeInteger(delivery.delivery_sequence) ||
    delivery.delivery_sequence < 1
  )
    throw new StorageError(
      "invalid_sequence",
      "The delivery sequence is invalid.",
    );
  return transaction(
    ["metadata", "events", "cursors", "messages", "receipts"],
    "readwrite",
    async (tx) => {
      await requireGeneration(tx, session, generation);
      const cursorId = key(scope, delivery.actor_id);
      const cursor: number =
        (await request(tx.objectStore("cursors").get(cursorId)))?.sequence ?? 0;
      const events = tx.objectStore("events");
      const ordered = events.index("actor_sequence");
      const hasPending = async (through: number) =>
        through < Number.MAX_SAFE_INTEGER &&
        (await request(
          ordered.getKey(
            IDBKeyRange.bound(
              [scope, delivery.actor_id, through],
              [scope, delivery.actor_id, Number.MAX_SAFE_INTEGER],
              true,
              false,
            ),
          ),
        )) !== undefined;
      if (delivery.delivery_sequence <= cursor)
        return { duplicate: true, gap: await hasPending(cursor), cursor };
      const eventId = key(scope, `${generation}:${delivery.delivery_id}`);
      const seen = await request(events.get(eventId));
      if (seen) {
        if (
          seen.actor_id !== delivery.actor_id ||
          seen.sequence !== delivery.delivery_sequence
        )
          throw new StorageError(
            "delivery_conflict",
            "A delivery ID was reused with another actor or sequence.",
          );
        return { duplicate: true, gap: await hasPending(cursor), cursor };
      }
      const result: DeliveryCommit = { duplicate: false, gap: false, cursor };
      if (delivery.type === "message") {
        result.message = await putMessage(tx, scope, delivery.message);
        if (delivery.message.sender.id !== session.actor?.id)
          await putReceipt(
            tx,
            session,
            generation,
            deviceId,
            delivery.message.conversation_id,
            delivery.message.id,
            "delivered",
          );
      } else {
        const receiptId = key(
          scope,
          `receipt:${generation}:${delivery.message_id}`,
        );
        const old = await metadata<MessageStatus>(tx, receiptId);
        const status = old
          ? mergeStatus(old, delivery.status)
          : delivery.status;
        setMetadata(tx, receiptId, status);
        result.status = status;
        const stored = await request(
          tx.objectStore("messages").get(key(scope, delivery.message_id)),
        );
        if (stored) {
          result.message = {
            ...stored.message,
            status: mergeStatus(stored.message.status, status),
          };
          tx.objectStore("messages").put({
            ...stored,
            message: result.message,
          });
        }
      }
      events.put({
        id: eventId,
        scope,
        actor_id: delivery.actor_id,
        sequence: delivery.delivery_sequence,
      });
      // Future events are durable too, but only a complete run may advance ACKs.
      let through = cursor;
      while (
        through < Number.MAX_SAFE_INTEGER &&
        (await request(
          ordered.getKey([scope, delivery.actor_id, through + 1]),
        )) !== undefined
      )
        through++;
      result.cursor = through;
      result.gap = await hasPending(through);
      tx.objectStore("cursors").put({ id: cursorId, scope, sequence: through });
      return result;
    },
  );
}
export async function queueMessage(
  session: Session,
  conversationId: string,
  body: MessageCreate,
  idempotencyKey: string = crypto.randomUUID(),
): Promise<OutboxEntry> {
  authenticated(session);
  const scope = scopeFor(session);
  const generation = await getGeneration(session);
  if (generation === undefined)
    throw new StorageError(
      "generation_unknown",
      "Connect to this testing environment once before queuing messages.",
    );
  const id = key(scope, `outbox:${idempotencyKey}`);
  return transaction(
    ["metadata", "outbox", "outboxBodies"],
    "readwrite",
    async (tx) => {
      await requireGeneration(tx, session, generation);
      if (await request(tx.objectStore("outbox").get(id)))
        throw new StorageError(
          "request_exists",
          "That request is already queued. Retry the existing request.",
        );
      const entry: OutboxEntry = {
        id,
        scope,
        profile_id: session.profile_id,
        conversation_id: conversationId,
        idempotency_key: idempotencyKey,
        generation,
        status: "queued",
        preview: (
          body.text ||
          body.voice_transcript ||
          (body.gif ? "GIF" : "Attachment")
        ).slice(0, 240),
        created_at: Date.now(),
        updated_at: Date.now(),
      };
      tx.objectStore("outbox").put(entry);
      tx.objectStore("outboxBodies").put({ id, scope, body });
      return entry;
    },
  );
}
export async function listOutbox(session: Session): Promise<OutboxEntry[]> {
  return transaction(["outbox"], "readonly", async (tx) => {
    const rows = await request<OutboxEntry[]>(
      tx.objectStore("outbox").index("scope").getAll(scopeFor(session)),
    );
    return rows.sort((a, b) => a.created_at - b.created_at);
  });
}
export async function claimOutbox(
  session: Session,
  id: string,
): Promise<{ entry: OutboxEntry; body: MessageCreate } | null> {
  return transaction(
    ["metadata", "outbox", "outboxBodies"],
    "readwrite",
    async (tx) => {
      const entry = await request<OutboxEntry | undefined>(
        tx.objectStore("outbox").get(id),
      );
      if (!entry || entry.scope !== scopeFor(session)) return null;
      if (entry.status === "fenced")
        throw new StorageError(
          "environment_changed",
          entry.error ||
            "This request belongs to another environment generation.",
        );
      if (entry.status === "sending" && Date.now() - entry.updated_at < 130_000)
        return null;
      await requireGeneration(tx, session, entry.generation);
      const saved = await request(tx.objectStore("outboxBodies").get(id));
      if (!saved)
        throw new StorageError(
          "missing_request_body",
          "The queued request body is unavailable.",
        );
      entry.status = "sending";
      entry.updated_at = Date.now();
      entry.error = undefined;
      entry.error_code = undefined;
      tx.objectStore("outbox").put(entry);
      return { entry, body: saved.body };
    },
  );
}
export async function failOutbox(
  session: Session,
  id: string,
  error: string,
  code: string,
): Promise<void> {
  return transaction(["outbox"], "readwrite", async (tx) => {
    const entry = await request<OutboxEntry | undefined>(
      tx.objectStore("outbox").get(id),
    );
    if (
      !entry ||
      entry.scope !== scopeFor(session) ||
      entry.status === "fenced"
    )
      return;
    entry.status = code === "environment_changed" ? "fenced" : "failed";
    entry.error = error;
    entry.error_code = code;
    entry.updated_at = Date.now();
    tx.objectStore("outbox").put(entry);
  });
}
export async function completeOutbox(
  session: Session,
  entry: OutboxEntry,
  message: Message,
): Promise<Message> {
  return transaction(
    ["metadata", "outbox", "outboxBodies", "messages"],
    "readwrite",
    async (tx) => {
      await requireGeneration(tx, session, entry.generation);
      const result = await putMessage(tx, scopeFor(session), message);
      tx.objectStore("outbox").delete(entry.id);
      tx.objectStore("outboxBodies").delete(entry.id);
      return result;
    },
  );
}
export async function removeOutbox(
  session: Session,
  id: string,
): Promise<void> {
  return transaction(["outbox", "outboxBodies"], "readwrite", async (tx) => {
    const entry = await request<OutboxEntry | undefined>(
      tx.objectStore("outbox").get(id),
    );
    if (entry?.scope === scopeFor(session)) {
      tx.objectStore("outbox").delete(id);
      tx.objectStore("outboxBodies").delete(id);
    }
  });
}

export interface PendingReceipt {
  id: string;
  scope: string;
  generation: number | null;
  actor_id: string;
  device_id: string;
  conversation_id: string;
  message_id: string;
  status: ReceiptStatus;
}
async function putReceipt(
  tx: IDBTransaction,
  session: Session,
  generation: number | null,
  deviceId: string,
  conversationId: string,
  messageId: string,
  status: ReceiptStatus,
): Promise<PendingReceipt> {
  authenticated(session);
  const scope = scopeFor(session);
  const id = key(scope, `pending-receipt:${messageId}`);
  const old = await request<PendingReceipt | undefined>(
    tx.objectStore("receipts").get(id),
  );
  const receipt: PendingReceipt = {
    id,
    scope,
    generation,
    actor_id: session.actor.id,
    device_id: deviceId,
    conversation_id: conversationId,
    message_id: messageId,
    status: old?.status === "read" ? "read" : status,
  };
  tx.objectStore("receipts").put(receipt);
  return receipt;
}
export async function queueReceipt(
  session: Session,
  generation: number | null,
  deviceId: string,
  conversationId: string,
  messageId: string,
  status: ReceiptStatus,
): Promise<PendingReceipt> {
  return transaction(["metadata", "receipts"], "readwrite", async (tx) => {
    await requireGeneration(tx, session, generation);
    return putReceipt(
      tx,
      session,
      generation,
      deviceId,
      conversationId,
      messageId,
      status,
    );
  });
}
export async function pendingReceipts(
  session: Session,
): Promise<PendingReceipt[]> {
  return transaction(["receipts"], "readonly", (tx) =>
    request(
      tx.objectStore("receipts").index("scope").getAll(scopeFor(session)),
    ),
  );
}
export async function completeReceipt(
  session: Session,
  generation: number | null,
  messageId: string,
  status: ReceiptStatus,
): Promise<void> {
  return transaction(["metadata", "receipts"], "readwrite", async (tx) => {
    await requireGeneration(tx, session, generation);
    const id = key(scopeFor(session), `pending-receipt:${messageId}`);
    const receipt = await request<PendingReceipt | undefined>(
      tx.objectStore("receipts").get(id),
    );
    if (receipt && (status === "read" || receipt.status === "delivered"))
      tx.objectStore("receipts").delete(id);
  });
}

/** Cross-tab hints carry identifiers only; large payloads remain in IndexedDB. */
export interface StorageUpdate {
  scope: string;
  kind: "message" | "receipt" | "reset" | "outbox";
  message_id?: string;
  status?: MessageStatus;
  generation?: number | null;
  source?: string;
}
export const broadcastSource = crypto.randomUUID();
let updates: BroadcastChannel | undefined;
export function broadcastUpdate(update: StorageUpdate): void {
  if (typeof BroadcastChannel === "undefined") return;
  updates ??= new BroadcastChannel("silicon-dm-browser-updates");
  updates.postMessage({ ...update, source: broadcastSource });
}
export async function cachedMessage(
  session: Session,
  messageId: string,
): Promise<Message | undefined> {
  return transaction(
    ["messages"],
    "readonly",
    async (tx) =>
      (
        await request(
          tx.objectStore("messages").get(key(scopeFor(session), messageId)),
        )
      )?.message,
  );
}

/** Remove a locally-created optimistic message after its durable request is acknowledged. */
export async function removeCachedMessage(
  session: Session,
  messageId: string,
): Promise<void> {
  return transaction(["messages"], "readwrite", async (tx) => {
    tx.objectStore("messages").delete(key(scopeFor(session), messageId));
  });
}
