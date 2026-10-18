/**
 * Drafts between tabs and against a save on its way (lib/chat/draft-store.ts, drafts.ts adoptStored):
 * - a draft sent in another tab leaves this tab's field (it was sent a second time from a stale tab);
 * - a draft typed in another tab is what this tab edits next (one stale keystroke replaced it on every device);
 * - a message sent while its draft's save is on its way does not leave that save as the service's draft (the sent
 *   words came back after a reload and on every other device).
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { changeDraft, clearDraft, draftOf } from "../lib/chat/draft-store";
import { adoptStored, type LocalDraft } from "../lib/chat/drafts";

/* A browser tab, as far as the store needs one: localStorage and the storage event other tabs' writes raise. */
const stored = new Map<string, string>();
const tab = Object.assign(new EventTarget(), {
  localStorage: {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => void stored.set(key, String(value)),
    removeItem: (key: string) => void stored.delete(key),
  },
});
(globalThis as { window?: unknown }).window = tab;

/** Another tab writes (or removes) a key: storage changes, then this tab hears the storage event. */
function otherTabWrites(key: string, value: LocalDraft | null) {
  if (value) stored.set(key, JSON.stringify(value));
  else stored.delete(key);
  tab.dispatchEvent(Object.assign(new Event("storage"), { key, newValue: value ? JSON.stringify(value) : null }));
}

const draft = (overrides: Partial<LocalDraft> = {}): LocalDraft => ({ text: "hello", replyTo: null, version: 1, dirty: false, updatedAt: 1000, ...overrides });
const keyOf = (account: string, address: string) => `dm:draft:v1:${account}:${address}`;

/** The DM service's draft routes behind the proxy: PUT (answering after `putDelayMs`), GET and DELETE. */
function stubService(putDelayMs = 0) {
  const calls: string[] = [];
  const drafts = new Map<string, { version: number; text: string }>();
  const original = globalThis.fetch;
  const json = (status: number, body: unknown) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    const method = init?.method ?? "GET";
    const address = decodeURIComponent(/conversations\/([^/]+)\/draft/.exec(url)?.[1] ?? "");
    calls.push(`${method} ${address}`);
    if (method === "PUT") {
      const body = JSON.parse(String(init?.body)) as { data: { message_content: string } };
      await new Promise(resolve => setTimeout(resolve, putDelayMs));
      const version = (drafts.get(address)?.version ?? 0) + 1;
      drafts.set(address, { version, text: body.data.message_content });
      calls.push(`answered PUT ${address}`);
      return json(200, { type: "draft", data: { conversation_id: address, member_id: "c:ada", version, message_content: body.data.message_content, metadata: {}, updated_at: "2026-10-18T00:00:00Z" } });
    }
    if (method === "DELETE") {
      drafts.delete(address);
      return new Response(null, { status: 204 });
    }
    const found = drafts.get(address);
    if (!found) return json(404, { type: "error", data: { error: { code: "not_found", message: "resource not found" } } });
    return json(200, { type: "draft", data: { conversation_id: address, member_id: "c:ada", version: found.version, message_content: found.text, metadata: {}, updated_at: "2026-10-18T00:00:00Z" } });
  }) as typeof fetch;
  return { calls, drafts, restore: () => { globalThis.fetch = original; } };
}

test("what another tab wrote: taken when nothing is unsaved here; unsaved words here stay, on the newer version", () => {
  assert.equal(adoptStored(draft({ text: "sent elsewhere" }), null), null, "sent in the other tab: gone here too");
  assert.deepEqual(adoptStored(null, draft({ text: "typed there", version: 2 })), draft({ text: "typed there", version: 2 }));
  assert.deepEqual(adoptStored(draft({ text: "old" }), draft({ text: "a long draft", version: 2 })), draft({ text: "a long draft", version: 2 }));
  const unsaved = draft({ text: "typing here", version: 1, dirty: true });
  assert.deepEqual(adoptStored(unsaved, draft({ text: "typed there", version: 3 })), { ...unsaved, version: 3 }, "kept, rebased on the version the other tab saw");
  assert.deepEqual(adoptStored(unsaved, null), { ...unsaved, version: 0 }, "the service no longer has the one it was based on");
  const same = draft();
  assert.equal(adoptStored(same, draft()), same, "nothing new: the same object, so nothing renders again");
  assert.equal(adoptStored(unsaved, draft({ version: 1 })), unsaved);
});

test("a draft sent in another tab leaves this tab's field, so it is never sent twice", () => {
  const key = keyOf("u-tabs", "g:sent-elsewhere");
  stored.set(key, JSON.stringify(draft({ text: "the words both tabs show" })));
  assert.equal(draftOf("u-tabs", "g:sent-elsewhere")?.text, "the words both tabs show");
  otherTabWrites(key, null);
  assert.equal(draftOf("u-tabs", "g:sent-elsewhere"), null);
});

test("a long draft typed in another tab is what this tab's next keystroke edits", async () => {
  const service = stubService();
  try {
    const key = keyOf("u-tabs", "g:typed-elsewhere");
    assert.equal(draftOf("u-tabs", "g:typed-elsewhere"), null, "this tab was open first, with nothing typed");
    otherTabWrites(key, draft({ text: "a long draft typed in the other tab", version: 1 }));
    assert.equal(draftOf("u-tabs", "g:typed-elsewhere")?.text, "a long draft typed in the other tab");
    changeDraft("u-tabs", "g:typed-elsewhere", { text: "a long draft typed in the other tab, and more" });
    assert.deepEqual(draftOf("u-tabs", "g:typed-elsewhere"), { text: "a long draft typed in the other tab, and more", replyTo: null, version: 1 });
    clearDraft("u-tabs", "g:typed-elsewhere", false);
  } finally {
    service.restore();
  }
});

test("a message sent while its draft's save is on its way: the draft is deleted once that save answers", async () => {
  const service = stubService(300);
  try {
    changeDraft("u-ghost", "g:ghost", { text: "hello " });
    // The save starts 1.5 s after the last keystroke; the send comes while its answer is on its way.
    await new Promise(resolve => setTimeout(resolve, 1600));
    assert.deepEqual(service.calls, ["PUT g:ghost"], "the save is on its way");
    clearDraft("u-ghost", "g:ghost", true);
    assert.equal(draftOf("u-ghost", "g:ghost"), null, "gone from the field at once");
    // A new draft typed right after the send is saved after the delete, never before it.
    changeDraft("u-ghost", "g:ghost", { text: "the next message" });
    await new Promise(resolve => setTimeout(resolve, 2400));
    assert.deepEqual(service.calls.slice(0, 3), ["PUT g:ghost", "answered PUT g:ghost", "DELETE g:ghost"], "deleted once the save answered");
    assert.equal(service.drafts.get("g:ghost")?.text, "the next message", "the next draft saved after it");
    assert.deepEqual(draftOf("u-ghost", "g:ghost"), { text: "the next message", replyTo: null, version: service.drafts.get("g:ghost")!.version });
    clearDraft("u-ghost", "g:ghost", true);
    await new Promise(resolve => setTimeout(resolve, 50));
    assert.equal(service.drafts.has("g:ghost"), false);
  } finally {
    service.restore();
  }
});
