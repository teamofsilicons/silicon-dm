/** Draft merging (lib/chat/drafts.ts): local first, the text being typed always wins, other devices' drafts arrive. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { editDraft, emptyDraft, hasContent, mergeDraft, savedDraft, type LocalDraft, type ServerDraft } from "../lib/chat/drafts";

const local = (overrides: Partial<LocalDraft> = {}): LocalDraft => ({ text: "hello", replyTo: null, version: 3, dirty: false, updatedAt: 1000, ...overrides });
const server = (overrides: Partial<ServerDraft> = {}): ServerDraft => ({ text: "hello", replyTo: null, version: 3, updatedAt: 1000, ...overrides });

test("unsaved local text wins over the service's draft and is saved against the newer version", () => {
  const result = mergeDraft(local({ text: "typed here", dirty: true, version: 3 }), server({ text: "from the phone", version: 5 }), 2000);
  assert.equal(result.action, "push");
  assert.equal(result.draft?.text, "typed here");
  assert.equal(result.draft?.version, 5, "the next save uses the service's version, so it is not refused again");
  assert.equal(result.draft?.dirty, true);
});

test("a clean local draft adopts newer text written on another device", () => {
  const result = mergeDraft(local({ text: "old", version: 3 }), server({ text: "new on the laptop", version: 4, updatedAt: 5000 }), 6000);
  assert.equal(result.action, "adopt");
  assert.deepEqual(result.draft, { text: "new on the laptop", replyTo: null, version: 4, dirty: false, updatedAt: 5000 });
});

test("a draft the service no longer has was sent or discarded elsewhere: a saved one clears, an unsaved one is pushed", () => {
  assert.deepEqual(mergeDraft(local({ version: 4 }), null, 2000), { draft: null, action: "clear" });
  const unsaved = mergeDraft(local({ text: "still typing", dirty: true, version: 4 }), null, 2000);
  assert.equal(unsaved.action, "push");
  assert.equal(unsaved.draft?.version, 0, "saved as a new draft");
  assert.deepEqual(mergeDraft(local({ version: 0 }), null, 2000).action, "none", "never saved: kept as it is");
  assert.deepEqual(mergeDraft(null, null, 2000), { draft: null, action: "none" });
});

test("nothing local yet: the service's draft is shown", () => {
  const result = mergeDraft(null, server({ text: "from yesterday", replyTo: "00a", version: 2, updatedAt: 500 }), 1000);
  assert.equal(result.action, "adopt");
  assert.deepEqual(result.draft, { text: "from yesterday", replyTo: "00a", version: 2, dirty: false, updatedAt: 500 });
  assert.equal(mergeDraft(null, server({ text: "", version: 2 }), 1000).action, "none", "an empty draft has nothing to show");
});

test("when both already agree nothing happens, and an unsaved edit equal to the service's becomes saved", () => {
  const same = local();
  assert.equal(mergeDraft(same, server(), 2000).draft, same);
  assert.equal(mergeDraft(same, server(), 2000).action, "none");
  const result = mergeDraft(local({ dirty: true, version: 2 }), server({ version: 6 }), 2000);
  assert.equal(result.action, "none");
  assert.equal(result.draft?.dirty, false);
  assert.equal(result.draft?.version, 6);
});

test("edits mark the draft unsaved; a save that raced newer typing leaves it unsaved", () => {
  const typed = editDraft(null, { text: "h" }, 100);
  assert.deepEqual(typed, { ...emptyDraft(100), text: "h", dirty: true });
  const more = editDraft(typed, { text: "hello" }, 200);
  assert.equal(savedDraft(more, { text: "hello", replyTo: null }, 1).dirty, false);
  assert.equal(savedDraft(more, { text: "h", replyTo: null }, 1).dirty, true, "the save carried older text");
  assert.equal(savedDraft(more, { text: "h", replyTo: null }, 1).version, 1);
  assert.equal(hasContent({ text: "   ", replyTo: null }), false);
  assert.equal(hasContent({ text: "", replyTo: "00a" }), true, "a reply on its own is worth keeping");
});
