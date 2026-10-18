/**
 * The outbox between tabs (lib/chat/outbox-store.ts): while another tab sends an item (it holds the item's Web Lock),
 * this tab leaves it alone instead of asking for the lock again at once (every other tab used to spin through tens of
 * thousands of lock requests a second for as long as the send took), and never sends it a second time once the other
 * tab has. If the other tab goes away without sending, this tab sends it.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { QueryClient } from "@tanstack/react-query";
import { attachOutbox } from "../lib/chat/outbox-store";
import type { OutboxItem } from "../lib/chat/outbox";

/* One tab of a browser, as far as the outbox needs one: localStorage, its storage event, and online/offline. */
const stored = new Map<string, string>();
const tab = Object.assign(new EventTarget(), {
  localStorage: {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => void stored.set(key, String(value)),
    removeItem: (key: string) => void stored.delete(key),
  },
});
(globalThis as { window?: unknown }).window = tab;

/** Each test signs in as an account of its own, so the outbox reads its stored items afresh. */
const accountOf = (name: string) => ({ me: { type: "carbon" as const, id: `c:${name}`, uuid: `u-${name}`, display_name: name }, key: `dm:outbox:v1:u-${name}` });
let KEY = "";
const me = accountOf("outbox-a").me;

/** Counts this tab's lock requests (the real Web Locks of Node, shared by "the other tab": the test itself). */
const locks = navigator.locks;
let requests = 0;
const realRequest = locks.request.bind(locks) as (...args: unknown[]) => Promise<unknown>;
(locks as unknown as { request: (...args: unknown[]) => Promise<unknown> }).request = (...args: unknown[]) => {
  requests += 1;
  return realRequest(...args);
};

/** The other tab sending an item: it holds the item's lock until `release` is called. */
async function otherTabHolds(key: string): Promise<() => void> {
  let release!: () => void;
  const held = new Promise<void>(resolve => (release = resolve));
  let granted!: () => void;
  const got = new Promise<void>(resolve => (granted = resolve));
  void realRequest(`dm:outbox:${key}`, async () => {
    granted();
    await held;
  });
  await got;
  return release;
}

function otherTabWrites(items: OutboxItem[] | null) {
  if (items?.length) stored.set(KEY, JSON.stringify(items));
  else stored.delete(KEY);
  tab.dispatchEvent(Object.assign(new Event("storage"), { key: KEY, newValue: stored.get(KEY) ?? null }));
}

const item = (key: string): OutboxItem => ({ key, address: "g:outbox-tabs", body: { message: `hello from ${key}` }, replyPreview: null, createdAt: Date.now(), status: "sending", attempts: 0, nextAttemptAt: Date.now() });

/** The DM service's send route behind the proxy: answers with the message, and counts the sends. */
function stubSends() {
  const sent: string[] = [];
  const original = globalThis.fetch;
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const key = new Headers(init?.headers).get("Idempotency-Key") ?? "";
    sent.push(`${init?.method ?? "GET"} ${String(input)} ${key}`);
    const message = { "message-id": "001", conversation_id: "g:outbox-tabs", sender: me, message: "hello", attachments: [], created_at: new Date().toISOString(), updated_at: null, deleted_at: null, delivered_at: null, read_at: null };
    return new Response(JSON.stringify({ type: "message", data: message }), { status: 201, headers: { "content-type": "application/json" } });
  }) as typeof fetch;
  return { sent, restore: () => { globalThis.fetch = original; } };
}

const pause = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

test("an item another tab is sending is left alone, then dropped once that tab has sent it", async () => {
  const service = stubSends();
  const account = accountOf("outbox-a");
  KEY = account.key;
  const release = await otherTabHolds("sent-there");
  stored.set(KEY, JSON.stringify([item("sent-there")]));
  const detach = attachOutbox({ queryClient: new QueryClient(), me: account.me });
  try {
    await pause(1200);
    assert.ok(requests <= 2, `asked for the lock ${requests} times in 1.2 s while the other tab held it`);
    // The other tab's send answered: it takes the item out of the stored outbox, then lets go of the lock.
    otherTabWrites(null);
    release();
    await pause(2500);
    assert.deepEqual(service.sent, [], "never sent a second time from this tab");
  } finally {
    detach();
    service.restore();
  }
});

test("an item whose sending tab went away is sent from this tab, once", async () => {
  const service = stubSends();
  const account = accountOf("outbox-b");
  KEY = account.key;
  requests = 0;
  const release = await otherTabHolds("left-behind");
  stored.set(KEY, JSON.stringify([item("left-behind")]));
  const detach = attachOutbox({ queryClient: new QueryClient(), me: account.me });
  try {
    await pause(300);
    assert.deepEqual(service.sent, []);
    // The other tab closed mid-send: its lock is gone, the item still in the stored outbox.
    release();
    await pause(2600);
    assert.equal(service.sent.length, 1, `sent ${service.sent.length} times`);
    assert.match(service.sent[0]!, /POST \/api\/api\/v1\/conversations\/g%3Aoutbox-tabs\/messages left-behind$/, "with the item's own Idempotency-Key");
    assert.equal(stored.has(KEY), false, "gone from the stored outbox once the service had it");
  } finally {
    detach();
    service.restore();
  }
});
