/**
 * Looking for a draft another device saved (lib/chat/drafts.ts draftHint and draftCheck, lib/chat/draft-store.ts
 * lookForDraft): one request at most for everyone who asks at once, none when the conversation list already says what
 * the service has, and "no draft" (the service's 404) is an answer, not a failure.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { draftOf, lookForDraft } from "../lib/chat/draft-store";
import { draftCheck, draftHint, type LocalDraft } from "../lib/chat/drafts";

const local = (overrides: Partial<LocalDraft> = {}): LocalDraft => ({ text: "hello", replyTo: null, version: 3, dirty: false, updatedAt: 1000, ...overrides });

test("what a conversation summary says about the service's draft", () => {
  assert.equal(draftHint(undefined), "pending", "not there yet: wait for it");
  assert.equal(draftHint({ id: "g:x" }), "unknown", "a service that does not say: ask it");
  assert.equal(draftHint({ id: "g:x", draft_version: null }), null);
  assert.equal(draftHint({ id: "g:x", draft_version: 4 }), 4);
  assert.equal(draftHint({ id: "g:x", draft_version: "4" }), "unknown", "anything else: ask");
  assert.equal(draftHint({ id: "g:x", draft_version: -1 }), "unknown");
});

test("whether to ask: never for what the list already settles", () => {
  assert.equal(draftCheck(null, "pending"), "wait");
  assert.equal(draftCheck(null, "unknown"), "fetch");
  assert.equal(draftCheck(null, null), "absent", "no draft anywhere: no request");
  assert.equal(draftCheck(local({ version: 0, dirty: true }), null), "absent", "typed here, never saved: it saves itself");
  assert.equal(draftCheck(local({ version: 3 }), null), "fetch", "saved here after the list was read: checked, never dropped on its word");
  assert.equal(draftCheck(local({ version: 3 }), 3), "same");
  assert.equal(draftCheck(local({ version: 3 }), 5), "fetch", "another device saved a newer one");
  assert.equal(draftCheck(local({ version: 3, dirty: true }), 3), "fetch", "unsaved text here: merged with what the service has");
  assert.equal(draftCheck(null, 2), "fetch");
});

/** The service behind the website's proxy, as fetch sees it: a 404 for a missing draft, or the draft. */
function stubService(drafts: Record<string, { version: number; text: string }>) {
  const calls: string[] = [];
  const original = globalThis.fetch;
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = String(input);
    calls.push(url);
    const address = decodeURIComponent(/conversations\/([^/]+)\/draft/.exec(url)?.[1] ?? "");
    const draft = drafts[address];
    await new Promise(resolve => setTimeout(resolve, 5));
    if (!draft) return new Response(JSON.stringify({ type: "error", data: { error: { code: "not_found", message: "resource not found" } } }), { status: 404, headers: { "content-type": "application/json" } });
    return new Response(JSON.stringify({ type: "draft", data: { conversation_id: address, member_id: "c:ada", version: draft.version, message_content: draft.text, metadata: {}, updated_at: "2026-10-18T00:00:00Z" } }), { status: 200, headers: { "content-type": "application/json" } });
  }) as typeof fetch;
  return { calls, restore: () => { globalThis.fetch = original; } };
}

test("opening a conversation asks once, however many ask at once; a 404 is no draft", async () => {
  const service = stubService({ "g:phone": { version: 2, text: "typed on the phone" } });
  try {
    await Promise.all([lookForDraft("u-ada", "g:empty", "unknown"), lookForDraft("u-ada", "g:empty", "unknown")]);
    assert.equal(service.calls.length, 1, "React's double effects share one request");
    assert.equal(draftOf("u-ada", "g:empty"), null, "no draft, and nothing failed");
    await lookForDraft("u-ada", "g:empty", "unknown");
    assert.equal(service.calls.length, 1, "opened again within 30 seconds: not asked again");

    await lookForDraft("u-ada", "g:phone", "unknown");
    assert.deepEqual(draftOf("u-ada", "g:phone"), { text: "typed on the phone", replyTo: null, version: 2 }, "another device's draft arrives");
    assert.equal(service.calls.length, 2);

    // The list says: no request when it settles it.
    await lookForDraft("u-ada", "g:listed-none", null);
    await lookForDraft("u-ada", "g:phone", 2);
    assert.equal(service.calls.length, 2, "nothing asked: the list said none, and the same version");
    await lookForDraft("u-ada", "g:listed-pending", "pending");
    assert.equal(service.calls.length, 2, "nothing asked before the list answered");
    await lookForDraft("u-ada", "g:phone", 3);
    assert.equal(service.calls.length, 3, "a newer version on the list: asked");
  } finally {
    service.restore();
  }
});
