/** What rows and bubbles say (lib/chat/format.ts, preview.ts, media.ts, compose.ts, address.ts). */
import assert from "node:assert/strict";
import { test } from "node:test";
import { conversationHref, conversationKind, conversationKindWords, conversationTitle, custodianFromRef, custodianWords, decodeAddress, directParticipants, encodeAddress, isiOf, shellQuote } from "../lib/chat/address";
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

test("a Silicon's custodian comes from its reference when DM names it, and only by a name people know", () => {
  const ada: AccountRef = { type: "carbon", id: "c:ada", uuid: "u-ada", display_name: "Ada Marlow" };
  const bo: AccountRef = { type: "carbon", id: "c:bo", uuid: "u-bo", display_name: "Bo Okafor" };
  const atlas: AccountRef = { type: "silicon", id: "si:atlas", uuid: "u-atlas", display_name: "Atlas", custodian: bo };
  const scout: AccountRef = { type: "silicon", id: "si:scout", uuid: "u-scout", display_name: "Scout", custodian: ada };
  const unseen: AccountRef = { type: "silicon", id: "si:pip", uuid: "u-pip", custodian: { type: "carbon", id: "0d6c4a8e-uuid", uuid: "0d6c4a8e-uuid" } };
  const older: AccountRef = { type: "silicon", id: "si:nova", uuid: "u-nova" };
  assert.equal(custodianFromRef(atlas)?.id, "c:bo");
  assert.equal(custodianFromRef(unseen), null, "a custodian DM knows only by uuid is not named");
  assert.equal(custodianFromRef(older), null, "an older service says nothing");
  assert.equal(custodianFromRef({ ...bo, custodian: ada }), null, "a Carbon has no custodian");
  assert.equal(custodianWords(atlas, ada, []), "Looked after by Bo Okafor");
  assert.equal(custodianWords(scout, ada, []), "Looked after by you");
  assert.equal(custodianWords(older, ada, [older]), "Looked after by you", "your own Silicons are known without the field");
  assert.equal(custodianWords(older, ada, []), null);
  assert.equal(custodianWords(bo, ada, []), null);
});

test("a conversation says what it is: a group, a number of people, or the other account's kind", () => {
  const me: AccountRef = { type: "carbon", id: "c:ada", uuid: "u-ada" };
  const bo: AccountRef = { type: "carbon", id: "c:bo", uuid: "u-bo" };
  const cy: AccountRef = { type: "carbon", id: "c:cy", uuid: "u-cy" };
  const scout: AccountRef = { type: "silicon", id: "si:scout", uuid: "u-scout" };
  const conversation = (id: string, participants: AccountRef[], group?: ConversationSummary["group"]) => ({ id, participants, group });
  assert.equal(conversationKindWords(conversation("c:ada::si:scout", [me, scout]), me), "Silicon");
  assert.equal(conversationKindWords(conversation("c:ada::c:bo", [me, bo]), me), "Carbon");
  assert.equal(conversationKindWords(conversation("0193f0aa-uuid", [me, bo, cy]), me), "3 people", "several without a name are people, not a group");
  assert.equal(conversationKindWords(conversation("g:launch", [me, bo, cy], { name: "Launch", description: "", owners: [me], version: 1 } as ConversationSummary["group"]), me), "Group");
  assert.equal(conversationKindWords(conversation("g:launch", [me, bo, cy]), me), "Group", "a group's address says it before its details arrive");
});
