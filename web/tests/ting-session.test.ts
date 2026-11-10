import { test, type TestContext } from "node:test";
import assert from "node:assert/strict";
import { openTingSession, type TingConnection } from "../lib/chat/ting-session";

class Socket {
  static all: Socket[] = [];
  sent: Record<string, unknown>[] = [];
  closed = false;
  onmessage?: (event: { data: string }) => void;
  onclose?: () => void;
  onerror?: () => void;
  constructor(public url: URL) { Socket.all.push(this); }
  send(data: string) { this.sent.push(JSON.parse(data)); }
  close() { this.closed = true; this.onclose?.(); }
  frame(value: unknown) { this.onmessage?.({ data: JSON.stringify(value) }); }
  attach(session = "receiver-1", account = "account-1") {
    this.frame({ op: "ready", protocol: "v1", session_id: session });
    this.frame({ op: "registered", session_id: session, request_id: this.sent[0]?.request_id, for: account, app_ids: [] });
    this.frame({ op: "watching_inbox", session_id: session, request_id: this.sent[1]?.request_id });
  }
}
const tick = () => new Promise<void>(resolve => setImmediate(resolve));
function harness(t: TestContext) {
  const saved = { fetch: globalThis.fetch, WebSocket: globalThis.WebSocket, window: globalThis.window, document: globalThis.document };
  const statuses: TingConnection[] = [], calls: RequestInit[] = [];
  let changes = 0;
  Socket.all = [];
  globalThis.WebSocket = Socket as unknown as typeof WebSocket;
  globalThis.window = new EventTarget() as unknown as Window & typeof globalThis;
  globalThis.document = Object.assign(new EventTarget(), { visibilityState: "visible" }) as unknown as Document;
  globalThis.fetch = async (_url, init) => { calls.push(init ?? {}); return Response.json({ authenticated: true, uuid: "account-1", id: "c:renamed", kind: "carbon" }); };
  const receiver = openTingSession({ origin: "https://ting.example", account: { uuid: "account-1", type: "carbon" }, appId: "dm", changed: () => changes++, status: value => statuses.push(value) });
  t.after(() => { receiver.close(); Object.assign(globalThis, saved); });
  return { receiver, statuses, calls, changes: () => changes };
}

test("receiver confirms the socket account, handles direct and digest hints, and sends no ACK", async t => {
  const h = harness(t); await tick(); const socket = Socket.all[0]!; socket.attach();
  assert.deepEqual(h.statuses.at(-1), { state: "connected", sessionId: "receiver-1" });
  assert.equal(h.calls[0]?.credentials, "include");
  assert.equal(new URL(socket.url).search, "?protocol=v1");
  assert.equal(socket.sent[0]?.app_ids, undefined, "signed-in receivers use recipient grants");
  assert.equal(h.changes(), 1);
  for (const op of ["tings", "silent_digest"]) socket.frame({ op, session_id: "receiver-1", tings: [{ type: "dm.syncChanged", for: "account-1" }] });
  assert.equal(h.changes(), 3);
  for (const frame of [
    { session_id: "old", tings: [{ type: "dm.syncChanged", for: "account-1" }] },
    { session_id: "receiver-1", tings: [{ type: "dm.syncChanged", for: "someone-else" }] },
    { session_id: "receiver-1", tings: [{ type: "dm.syncChanged", for: "account-1", session_id: "other-session" }] },
    { session_id: "receiver-1", tings: [{ type: "other.event", for: "account-1" }] },
    { session_id: "receiver-1", webhook_id: "not-ours", tings: [{ type: "dm.syncChanged", for: "account-1" }] },
    { session_id: "receiver-1", webhook_id: "", tings: [{ type: "dm.syncChanged", for: "account-1" }] },
  ]) socket.frame({ op: "tings", ...frame });
  assert.equal(h.changes(), 3);
  assert.deepEqual(socket.sent.map(frame => frame.op), ["register", "watch_inbox"]);
});

test("an account switch between /me and the upgrade closes the socket without watching", async t => {
  const h = harness(t); await tick(); const socket = Socket.all[0]!; socket.attach("receiver-1", "other-account");
  assert.equal(socket.closed, true);
  assert.deepEqual(socket.sent.map(frame => frame.op), ["register"]);
  assert.equal(h.statuses.at(-1)?.state, "sign-in");
});

test("reconnect forgets the old receiver and ignores its late frames", async t => {
  const h = harness(t); await tick(); const first = Socket.all[0]!; first.attach();
  h.receiver.reconnect(); await tick();
  assert.equal(first.closed, true); assert.equal(h.statuses.at(-1)?.sessionId, null);
  Socket.all[1]!.attach("receiver-2"); const changes = h.changes();
  first.frame({ op: "inbox_changed" });
  assert.equal(h.changes(), changes); assert.equal(h.statuses.at(-1)?.sessionId, "receiver-2");
});

test("revocation stops receiver use and returning from Ting sign-in revalidates", async t => {
  const h = harness(t); await tick(); const first = Socket.all[0]!; first.attach();
  first.frame({ op: "paused", reason: "session_expired", webhook_ids: [] });
  assert.deepEqual(h.statuses.at(-1), { state: "sign-in", sessionId: null }); assert.equal(first.closed, true);
  window.dispatchEvent(new Event("focus")); await tick();
  assert.equal(h.calls.length, 2); Socket.all[1]!.attach("receiver-2"); assert.equal(h.statuses.at(-1)?.state, "connected");
});

test("legacy greetings and a mismatched identity never report a working receiver", async t => {
  const h = harness(t); await tick(); Socket.all[0]!.frame({ op: "ready", protocol: "v1", receiver_id: "old-server" });
  assert.equal(h.statuses.at(-1)?.state, "unavailable");
  globalThis.fetch = async () => Response.json({ authenticated: true, uuid: "different", kind: "carbon" });
  h.receiver.reconnect(); await tick(); assert.equal(Socket.all.length, 1); assert.equal(h.statuses.at(-1)?.state, "sign-in");
});

test("closing during a pending identity check creates no late socket", async t => {
  const h = harness(t); await tick(); let finish!: (response: Response) => void;
  globalThis.fetch = () => new Promise(resolve => { finish = resolve; });
  h.receiver.reconnect(); h.receiver.close();
  finish(Response.json({ authenticated: true, uuid: "account-1", kind: "carbon" })); await tick(); assert.equal(Socket.all.length, 1);
});
