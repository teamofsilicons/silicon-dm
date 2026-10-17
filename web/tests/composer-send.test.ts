/**
 * Sending what the tray carries (components/chat/composer/send-latch.ts, tray.ts and tray-store.ts): a send asked for
 * while a file uploads waits and goes exactly once, also after the composer went (another conversation was opened); a
 * failed upload holds the message until the Carbon retries or removes it, and the words say which; what a message
 * carries, and what a conversation keeps of its tray in this browser.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { blockedWords, decideSend, SendLatch, trayState, waitingWords } from "../components/chat/composer/send-latch";
import { keptItems, restoreItems, TRAY_MAX_ITEMS, trayBusy, trayFailed, trayHasContent, trayPayload, trayReducer, type TrayItem } from "../components/chat/composer/tray";
import { trayActions, type TraySender } from "../components/chat/composer/tray-store";

const uploading = { busy: true, failed: false, content: true };
const ready = { busy: false, failed: false, content: true };

test("decideSend: send, wait for uploads, refuse a failed upload, or nothing to send", () => {
  assert.equal(decideSend(ready), "send");
  assert.equal(decideSend(uploading), "wait");
  assert.equal(decideSend({ busy: false, failed: true, content: true }), "blocked");
  assert.equal(decideSend({ busy: true, failed: true, content: true }), "blocked", "a failure wins: waiting would never end");
  assert.equal(decideSend({ busy: false, failed: false, content: false }), "nothing");
});

test("the latch: asked twice while uploading, it sends exactly once when the uploads are done", () => {
  const latch = new SendLatch();
  assert.equal(latch.request(uploading), "wait");
  assert.equal(latch.request(uploading), "wait");
  assert.equal(latch.pending, true);
  assert.equal(latch.settle(uploading), null, "still uploading: nothing yet");
  assert.equal(latch.settle(ready), "send");
  assert.equal(latch.pending, false);
  assert.equal(latch.settle(ready), null, "never twice");
});

test("the latch: a failure lets the wish go, Esc cancels it, and a send that can go now never waits", () => {
  const latch = new SendLatch();
  latch.request(uploading);
  assert.equal(latch.settle({ busy: false, failed: true, content: true }), "blocked");
  assert.equal(latch.pending, false);
  latch.request(uploading);
  assert.equal(latch.cancel(), true);
  assert.equal(latch.cancel(), false, "nothing left to cancel");
  assert.equal(latch.settle(ready), null, "a cancelled send never goes");
  assert.equal(latch.request(ready), "send");
  assert.equal(latch.pending, false);
  latch.request(uploading);
  assert.equal(latch.settle({ busy: false, failed: false, content: false }), "nothing", "everything was removed meanwhile");
  assert.equal(latch.pending, false);
});

const file = (id: string, status: "uploading" | "ready" | "failed", extra: Partial<Extract<TrayItem, { kind: "file" }>> = {}): TrayItem => ({ id, kind: "file", name: `${id}.pdf`, size: 10, contentType: "application/pdf", status, progress: status === "ready" ? 1 : 0, ...extra });

test("the tray: upload, retry and restore; one GIF and one voice note per message", () => {
  let items: TrayItem[] = [];
  items = trayReducer(items, { type: "add", item: file("a", "uploading", { queued: true }) });
  items = trayReducer(items, { type: "start", id: "a" });
  assert.equal((items[0] as Extract<TrayItem, { kind: "file" }>).queued, false);
  items = trayReducer(items, { type: "progress", id: "a", progress: 1.7 });
  assert.equal(items[0]!.kind === "file" && items[0]!.progress, 1, "progress stays within 0..1");
  items = trayReducer(items, { type: "fail", id: "a", error: "Silicon Briefcase did not answer." });
  assert.equal(trayFailed(items), true);
  items = trayReducer(items, { type: "retry", id: "a" });
  assert.deepEqual([items[0]!.status, (items[0] as Extract<TrayItem, { kind: "file" }>).error, (items[0] as Extract<TrayItem, { kind: "file" }>).queued], ["uploading", undefined, true]);
  assert.equal(trayBusy(items), true);
  items = trayReducer(items, { type: "ready", id: "a", url: "https://api.briefcase.test/api/v1/public/u/apps/dm/a.pdf?view=inline", size: 12 });
  assert.equal(trayHasContent(items), true);
  assert.deepEqual(trayReducer(items, { type: "retry", id: "a" }), items, "Retry applies to a failed file only");
  const gif = (id: string): TrayItem => ({ id, kind: "gif", status: "ready", gif: { provider_id: id, url: `https://media.giphy.com/media/${id}/giphy.gif` } });
  items = trayReducer(trayReducer(items, { type: "add", item: gif("g1") }), { type: "add", item: gif("g2") });
  assert.deepEqual(items.filter(item => item.kind === "gif").map(item => item.id), ["g2"], "a new GIF replaces the old one");
  const restored = trayReducer(items, { type: "restore", items: [file("a", "ready", { url: "https://x.test/a" }), file("b", "ready", { url: "https://x.test/b" })] });
  assert.deepEqual(restored.map(item => item.id), ["b", "a", "g2"], "restoring never doubles an item the tray already has");
});

test("what a message carries: ready files with their details, links, the GIF, the voice note with its words", () => {
  const voice = { permanent_url: "https://api.briefcase.test/api/v1/public/u/apps/dm/note.webm?view=inline", duration_milliseconds: 3000 };
  const items: TrayItem[] = [
    file("a", "ready", { url: "https://api.briefcase.test/api/v1/public/u/apps/dm/a.pdf?view=inline" }),
    file("b", "uploading"),
    file("c", "failed"),
    { id: "l", kind: "link", url: "https://example.com/doc", status: "ready" },
    { id: "g", kind: "gif", status: "ready", gif: { provider_id: "x", url: "https://media.giphy.com/media/x/giphy.gif", title: "Wave" } },
    { id: "v", kind: "voice", status: "ready", progress: 1, voice, transcript: "On my way." },
  ];
  assert.deepEqual(trayPayload(items), {
    attachments: [{ permanent_url: "https://api.briefcase.test/api/v1/public/u/apps/dm/a.pdf?view=inline", name: "a.pdf", content_type: "application/pdf", size: 10 }, "https://example.com/doc"],
    gif: { provider_id: "x", url: "https://media.giphy.com/media/x/giphy.gif", title: "Wave" },
    voice,
    voice_transcript: "On my way.",
  });
  assert.deepEqual(trayPayload([]), {});
});

test("kept in this browser: only what is ready, read back with every field checked", () => {
  const items: TrayItem[] = [
    file("a", "ready", { url: "https://api.briefcase.test/api/v1/public/u/apps/dm/a.pdf?view=inline" }),
    file("b", "uploading"),
    file("c", "failed"),
    file("d", "ready"),
    { id: "l", kind: "link", url: "https://example.com", status: "ready" },
  ];
  const kept = keptItems(items);
  assert.deepEqual(kept.map(item => item.id), ["a", "l"], "uploads still on their way, failures and files without a link are not kept");
  assert.deepEqual(restoreItems(JSON.parse(JSON.stringify(kept))), kept);
  assert.deepEqual(restoreItems([{ id: "x", kind: "link", url: "javascript:alert(1)", status: "ready" }, { id: "y", kind: "file", name: "n", url: "http://plain.test/a", status: "ready" }, { kind: "link", url: "https://ok.test", status: "ready" }, null, "nonsense"]), [], "anything malformed or not https is dropped");
  assert.deepEqual(restoreItems("not a list"), []);
  assert.equal(restoreItems(Array.from({ length: TRAY_MAX_ITEMS + 5 }, (_, index) => ({ id: `l${index}`, kind: "link", url: `https://x.test/${index}`, status: "ready" }))).length, TRAY_MAX_ITEMS);
});

test("trayState: uploading, failed, and whether words or ready items are there to send", () => {
  assert.deepEqual(trayState([], "  "), { busy: false, failed: false, content: false });
  assert.deepEqual(trayState([], "hi"), { busy: false, failed: false, content: true });
  assert.deepEqual(trayState([file("a", "uploading")], ""), { busy: true, failed: false, content: false });
  assert.deepEqual(trayState([file("a", "ready", { url: "https://x.test/a" }), file("b", "failed")], ""), { busy: false, failed: true, content: true });
});

test("the words under the field: what a send waits for (online and off), and what to do when it cannot go", () => {
  const voice: TrayItem = { id: "v", kind: "voice", status: "uploading", progress: 0 };
  assert.equal(waitingWords([file("a", "uploading")]), "Sends when a.pdf is uploaded.");
  assert.equal(waitingWords([voice]), "Sends when the voice message is uploaded.");
  // Its bytes saved, its words still coming.
  const heard = trayReducer([voice], { type: "transcribing", id: "v" });
  assert.deepEqual(heard[0], { ...voice, progress: 1, transcribing: true });
  assert.equal(waitingWords(heard), "Sends when the voice message is transcribed.");
  assert.equal(trayBusy(heard), true, "a voice message still waiting for its words holds the send");
  const done = trayReducer(heard, { type: "ready", id: "v", voice: { permanent_url: "https://x.test/v.webm", duration_milliseconds: 3000 }, transcript: "hello" });
  assert.equal(done[0]!.status, "ready");
  assert.equal(done[0]!.kind === "voice" && done[0]!.transcribing, false);
  assert.equal(trayReducer([file("a", "uploading")], { type: "transcribing", id: "a" })[0]!.status, "uploading", "a file is never transcribed");
  assert.equal(waitingWords([file("a", "uploading"), file("b", "uploading"), file("c", "ready")]), "Sends when 2 files are uploaded.");
  assert.equal(waitingWords([file("a", "uploading")], false), "You are offline: it sends when you are back and a.pdf is uploaded.");
  const retryable = (id: string) => id !== "empty";
  assert.equal(blockedWords([file("a", "failed")], retryable), "a.pdf did not upload. Retry it or remove it, then send.");
  assert.equal(blockedWords([file("empty", "failed")], retryable), "empty.pdf did not upload. Remove it, then send.", "an empty file can only be removed");
  assert.equal(blockedWords([{ ...voice, status: "failed" }], retryable), "The voice message did not upload. Remove it, then send.", "the composer has no recording to retry");
  assert.equal(blockedWords([file("a", "failed"), file("b", "failed")], retryable), "2 attachments did not upload. Retry or remove them, then send.");
  assert.equal(blockedWords([file("a", "failed"), file("empty", "failed")], retryable), "2 attachments did not upload. Retry or remove each one, then send.");
  assert.equal(blockedWords([file("empty", "failed"), { ...voice, status: "failed" }], retryable), "2 attachments did not upload. Remove them, then send.");
});

test("an uploaded file takes the name the website saved it under", () => {
  const items = trayReducer([file("a", "uploading", { name: "notes\u202egnp.exe" })], { type: "ready", id: "a", url: "https://x.test/a", name: "notesgnp.exe" });
  assert.equal(items[0]!.kind === "file" && items[0]!.name, "notesgnp.exe");
  const kept = trayReducer([file("b", "uploading")], { type: "ready", id: "b", url: "https://x.test/b" });
  assert.equal(kept[0]!.kind === "file" && kept[0]!.name, "b.pdf", "no name in the answer keeps the file's own");
});

const voiceNote = { permanent_url: "https://api.briefcase.test/api/v1/public/u/apps/dm/note.webm?view=inline", duration_milliseconds: 2000 };

function recorder(tray: ReturnType<typeof trayActions>, words = "On my way") {
  const calls: string[] = [];
  const sender = (who: string): TraySender => ({
    words: () => words,
    send: () => {
      calls.push(`${who} sent ${JSON.stringify(trayPayload(tray.current()).voice?.duration_milliseconds ?? null)}`);
      tray.clear();
    },
    blocked: items => calls.push(`${who} blocked: ${blockedWords(items, tray.canRetry)}`),
  });
  return { calls, sender };
}

test("a waiting send outlives the composer: what it left behind sends exactly once when the voice note is ready", () => {
  const tray = trayActions(`acct-${Math.random().toString(36).slice(2)}`, "g:composer-unit");
  const { calls, sender } = recorder(tray);
  tray.dispatch({ type: "add", item: { id: "v1", kind: "voice", status: "uploading", progress: 0 } });
  tray.setSender(sender("composer"));
  assert.equal(tray.request("On my way"), "wait");
  assert.equal(tray.request("On my way"), "wait", "asking again changes nothing");
  tray.dispatch({ type: "progress", id: "v1", progress: 0.5 });
  assert.deepEqual(calls, [], "still uploading");
  // The Carbon opens another conversation: the composer leaves a sender behind with the words as they are.
  tray.setSender(sender("left behind"));
  tray.dispatch({ type: "ready", id: "v1", voice: voiceNote });
  assert.deepEqual(calls, ["left behind sent 2000"]);
  assert.deepEqual(tray.current(), [], "sending emptied the tray");
  tray.dispatch({ type: "add", item: { id: "l", kind: "link", url: "https://example.com", status: "ready" } });
  assert.deepEqual(calls, ["left behind sent 2000"], "never twice: the wish went with the message");
  tray.clear();
});

test("a waiting send that cannot go says why once; Esc takes the wish back; a sender set when it is ready sends at once", () => {
  const tray = trayActions(`acct-${Math.random().toString(36).slice(2)}`, "g:composer-unit-2");
  const { calls, sender } = recorder(tray);
  tray.setSender(sender("composer"));
  tray.dispatch({ type: "add", item: { id: "v1", kind: "voice", status: "uploading", progress: 0 } });
  assert.equal(tray.request("x"), "wait");
  tray.dispatch({ type: "fail", id: "v1", error: "Silicon Briefcase did not answer." });
  assert.deepEqual(calls, ["composer blocked: The voice message did not upload. Remove it, then send."]);
  tray.remove("v1");
  assert.equal(calls.length, 1, "blocked once; removing it later sends nothing by itself");
  tray.dispatch({ type: "add", item: { id: "v2", kind: "voice", status: "uploading", progress: 0 } });
  assert.equal(tray.request("x"), "wait");
  assert.equal(tray.cancelPending(), true);
  assert.equal(tray.cancelPending(), false);
  tray.dispatch({ type: "ready", id: "v2", voice: voiceNote });
  assert.equal(calls.length, 1, "a cancelled wish never sends");
  // Waiting with no one to send it (between two composers): the next sender sends at once if it can go.
  tray.dispatch({ type: "add", item: { id: "v3", kind: "voice", status: "uploading", progress: 0 } });
  tray.setSender(null);
  assert.equal(tray.request("x"), "wait");
  tray.dispatch({ type: "ready", id: "v3", voice: voiceNote });
  assert.equal(calls.length, 1);
  tray.setSender(sender("next composer"));
  assert.deepEqual(calls.slice(1), ["next composer sent 2000"]);
});
