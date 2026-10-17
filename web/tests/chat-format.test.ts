/** What rows and bubbles say (lib/chat/format.ts, preview.ts, media.ts, compose.ts, address.ts). */
import assert from "node:assert/strict";
import { test } from "node:test";
import { conversationHref, conversationKind, conversationTitle, decodeAddress, directParticipants, encodeAddress, isiOf, shellQuote } from "../lib/chat/address";
import { canEditHere, editContent, replyPreviewOf } from "../lib/chat/compose";
import { clockDuration, fileSize, lastSeen, rowTime } from "../lib/chat/format";
import { attachmentName, mediaKind, messageMedia, safeHttpsUrl } from "../lib/chat/media";
import { messageGist, rowPreview, spokenWords } from "../lib/chat/preview";
import type { AccountRef, ConversationSummary, Message } from "../lib/chat/types";

const TZ = "Asia/Kolkata";
const now = new Date("2026-10-17T15:00:00+05:30");
const ada: AccountRef = { type: "carbon", id: "c:ada", uuid: "u-ada", display_name: "Ada Lovelace" };
const scout: AccountRef = { type: "silicon", id: "si:scout", uuid: "u-scout" };
const base = (overrides: Partial<Message> = {}): Message => ({
  "message-id": "00a", conversation_id: "c:ada::si:scout", recipient_id: "c:ada", sender: scout, message: "", attachments: [], voice_transcript: null,
  reply: null, bundle: null, history: [], created_at: "2026-10-17T09:00:00Z", updated_at: null, deleted_at: null, delivered_at: null, read_at: null, ...overrides,
});

test("row times: the clock today, then Yesterday, the weekday, the date", () => {
  assert.equal(rowTime("2026-10-17T14:05:00+05:30", now, TZ), "14:05");
  assert.equal(rowTime("2026-10-16T23:00:00+05:30", now, TZ), "Yesterday");
  assert.equal(rowTime("2026-10-13T10:00:00+05:30", now, TZ), "Tue");
  assert.equal(rowTime("2026-09-02T10:00:00+05:30", now, TZ), "Sep 2");
  assert.equal(rowTime("2025-12-30T10:00:00+05:30", now, TZ), "Dec 30, 2025");
  assert.equal(rowTime(null, now, TZ), "");
});

test("durations, sizes and last seen read the way people say them", () => {
  assert.equal(clockDuration(12_400), "0:12");
  assert.equal(clockDuration(65_000), "1:05");
  assert.equal(clockDuration(3_729_000), "1:02:09");
  assert.equal(clockDuration(null), "0:00");
  assert.equal(fileSize(512), "512 B");
  assert.equal(fileSize(1536), "1.5 KB");
  assert.equal(fileSize(5 * 1024 * 1024), "5 MB");
  assert.equal(lastSeen("2026-10-17T14:55:00+05:30", now, TZ), "last seen 5 minutes ago");
  assert.equal(lastSeen("2026-10-17T09:30:00+05:30", now, TZ), "last seen today at 09:30");
  assert.equal(lastSeen(null, now, TZ), "offline");
});

test("a row's line: You:, a sender's first name in groups, and what a message carries", () => {
  assert.deepEqual(rowPreview(base({ message: "**Done**: see `log`" }), { me: ada, group: false }), { who: null, text: "Done: see log", quiet: false });
  assert.deepEqual(rowPreview(base({ sender: ada, message: "hi" }), { me: ada, group: false }), { who: "You", text: "hi", quiet: false });
  assert.equal(rowPreview(base({ sender: ada, message: "hi" }), { me: scout, group: true }).who, "Ada");
  assert.equal(messageGist(base({ voice: { url: "https://x.test/a.webm", duration_milliseconds: 12_000 }, attachments: ["https://x.test/a.webm"] })), "Voice message · 0:12");
  assert.equal(messageGist(base({ attachments: ["https://x.test/a.png", "https://x.test/b.jpg"] })), "2 photos");
  assert.equal(messageGist(base({ gif: { provider_id: "g1", url: "https://media.giphy.com/x.gif" }, attachments: ["https://media.giphy.com/x.gif"] })), "GIF");
  assert.equal(messageGist(base({ deleted_at: "2026-10-17T10:00:00Z", message: null })), "This message was deleted");
  assert.equal(messageGist(base({ speech: { voice: "mark", script: "[excited] It works!", status: "ready" } })), "It works!");
  assert.equal(spokenWords("[whispers]quietly[/whispers] now"), "quietly now");
  assert.deepEqual(rowPreview(null, { me: ada, group: false }), { who: null, text: "No messages yet", quiet: true });
});

test("attachments: kinds from details or addresses, names, and https only", () => {
  assert.equal(mediaKind("https://x.test/a.PNG"), "image");
  assert.equal(mediaKind("https://x.test/file", "video/mp4"), "video");
  assert.equal(mediaKind("https://x.test/report.pdf"), "file");
  assert.equal(attachmentName({ url: "https://x.test/files/Q4%20report.pdf" }), "Q4 report.pdf");
  assert.equal(safeHttpsUrl("http://x.test/a.png"), null);
  assert.equal(safeHttpsUrl("https://user:pass@x.test/a.png"), null);
  const media = messageMedia(base({ attachments: ["https://x.test/a.webm", "https://x.test/b.png"], voice: { url: "https://x.test/a.webm" }, attachment_details: [{ url: "https://x.test/a.webm" }, { url: "https://x.test/b.png", name: "b.png", size: 10 }] }));
  assert.deepEqual(media.attachments.map(item => [item.url, item.kind, item.name]), [["https://x.test/b.png", "image", "b.png"]], "the voice note is not an attachment too");
});

test("an edit carries everything the message keeps; a reply preview quotes it", () => {
  const message = base({
    sender: ada, message: "old", attachments: ["https://x.test/a.webm", "https://x.test/doc.pdf", "https://media.giphy.com/x.gif"],
    attachment_details: [{ url: "https://x.test/a.webm" }, { url: "https://x.test/doc.pdf", name: "doc.pdf", content_type: "application/pdf", size: 9 }, { url: "https://media.giphy.com/x.gif" }],
    voice: { url: "https://x.test/a.webm", duration_milliseconds: 4000 }, voice_transcript: "hello", gif: { provider_id: "g1", url: "https://media.giphy.com/x.gif" },
    reply: { "message-id": "009", sender: scout, content: null }, metadata: { topic: "launch" },
  });
  assert.deepEqual(editContent(message, "new words"), {
    message: "new words",
    attachments: [{ permanent_url: "https://x.test/doc.pdf", name: "doc.pdf", content_type: "application/pdf", size: 9 }],
    voice: { permanent_url: "https://x.test/a.webm", duration_milliseconds: 4000 },
    voice_transcript: "hello",
    gif: { provider_id: "g1", url: "https://media.giphy.com/x.gif" },
    reply: { "message-id": "009" },
    metadata: { topic: "launch" },
  });
  assert.equal(canEditHere(base({ speech: { voice: "mark", script: "hi", status: "ready" } })), false, "spoken messages are edited with the CLI");
  assert.deepEqual(replyPreviewOf(base({ message: "quoted" })).content, { message: "quoted", attachments: [], voice_transcript: null });
});

test("addresses: readable links, direct pairs, titles and kinds, ISI, shell quoting", () => {
  assert.equal(encodeAddress("c:ada::si:scout"), "c:ada::si:scout");
  assert.equal(conversationHref("g:product design", { view: "si:scout", code: "00a" }), "/c/g:product%20design?for=si:scout#00a");
  assert.equal(decodeAddress("c%3Aada%3A%3Asi%3Ascout"), "c:ada::si:scout");
  assert.deepEqual(directParticipants("c:ada::si:scout"), ["c:ada", "si:scout"]);
  assert.equal(directParticipants("g:product"), null);
  const conversation: ConversationSummary = { id: "c:ada::si:scout", participants: [ada, scout], last_message: null, created_at: "", updated_at: "" };
  assert.equal(conversationTitle(conversation, ada), "si:scout");
  assert.equal(conversationTitle(conversation, scout), "Ada Lovelace");
  assert.equal(conversationKind(conversation, ada), "silicon");
  assert.equal(conversationKind({ ...conversation, group: { name: "Launch", description: "", owners: [] } }, ada), "group");
  assert.equal(isiOf("deliberate@si:cos"), "deliberate");
  assert.equal(isiOf("c:ada"), null);
  assert.equal(shellQuote("c:ada::si:scout"), "c:ada::si:scout");
  assert.equal(shellQuote("it's"), "'it'\\''s'");
});
