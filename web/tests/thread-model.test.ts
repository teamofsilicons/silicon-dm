/** The thread's pure decisions (components/chat/thread/thread-model.ts). */
import assert from "node:assert/strict";
import { test } from "node:test";
import { activityStatus, arrivedSends, busyWords, conversationPhrase, copyableText, custodianOf, cutAtWord, emojiOnly, estimateBubbleHeight, fileNameParts, findFirstUnread, friendlyType, galleryRatio, galleryRows, locateCode, messageCommands, messageLabel, namesInWords, pendingMedia, peopleWords, photoRatio, plainFailure, routingOf, sentSentence, sentWhen, toolbarPlacement, unreachableWords, unreadBeyondLoaded, unreadWithinReach, unseenCount, viewedName } from "../components/chat/thread/thread-model";
import type { AccountRef, ConversationSummary, Message } from "../lib/chat/types";
import { ApiError } from "../lib/errors";

const ada: AccountRef = { type: "carbon", id: "c:ada", uuid: "u-ada", display_name: "Ada Marlow" };
const bo: AccountRef = { type: "carbon", id: "c:bo", uuid: "u-bo", display_name: "Bo Okafor" };
const cy: AccountRef = { type: "carbon", id: "c:cy", uuid: "u-cy", display_name: "Cy Varga" };
const scout: AccountRef = { type: "silicon", id: "si:scout", uuid: "u-scout", display_name: "Scout" };
const atlas: AccountRef = { type: "silicon", id: "si:atlas", uuid: "u-atlas", display_name: "Atlas" };

function message(code: string, sender: AccountRef, text = `message ${code}`, extra: Partial<Message> = {}): Message {
  return {
    "message-id": code, conversation_id: "g:x", recipient_id: "g:x", sender, message: text, attachments: [], voice_transcript: null, reply: null,
    bundle: null, history: [], created_at: "2026-10-17T10:00:00Z", updated_at: null, deleted_at: null, delivered_at: null, read_at: null, ...extra,
  };
}

const summary = (extra: Partial<ConversationSummary>): ConversationSummary => ({ id: "g:x", participants: [ada, bo], last_message: null, created_at: "", updated_at: "", ...extra });

test("the first unread follows the read position and takes the service's count", () => {
  const messages = [message("000", bo), message("001", ada), message("002", bo), message("003", bo)];
  assert.deepEqual(findFirstUnread(summary({ last_read_message_id: "000", unread_count: 2 }), messages, ada), { code: "002", count: 2 });
  // Nothing read yet: the first message from someone else.
  assert.deepEqual(findFirstUnread(summary({ last_read_message_id: null, unread_count: 3 }), messages, ada), { code: "000", count: 3 });
  // Nothing unread: no divider.
  assert.equal(findFirstUnread(summary({ last_read_message_id: "003", unread_count: 0 }), messages, ada), null);
  // More unread than loaded: the count says so, and older pages should load first.
  assert.deepEqual(findFirstUnread(summary({ last_read_message_id: null, unread_count: 60 }), messages, ada), { code: "000", count: 60 });
  assert.equal(unreadBeyondLoaded(summary({ last_read_message_id: null, unread_count: 60 }), messages, ada), true);
  assert.equal(unreadBeyondLoaded(summary({ last_read_message_id: "000", unread_count: 2 }), messages, ada), false);
});

test("an older service: a direct conversation's unread is what the other did not see read", () => {
  const messages = [message("000", bo, "a", { read_at: "2026-10-17T10:00:00Z" }), message("001", bo, "b")];
  assert.deepEqual(findFirstUnread(summary({}), messages, ada), { code: "001", count: 1 });
});

test("unseen messages below the screen: from others, not deleted, after the newest seen", () => {
  const messages = [message("000", bo), message("001", ada), message("002", bo), message("003", bo, "x", { deleted_at: "2026-10-17T10:00:00Z" }), message("004", scout)];
  assert.equal(unseenCount(messages, "001", ada), 2);
  assert.equal(unseenCount(messages, "004", ada), 0);
  assert.equal(unseenCount(messages, null, ada), 0);
});

test("emoji-only messages: one to three emoji, with skin tones, flags, keycaps and joined families", () => {
  assert.equal(emojiOnly("👍"), 1);
  assert.equal(emojiOnly(" 👍🎉 "), 2);
  assert.equal(emojiOnly("👍🏽"), 1);
  assert.equal(emojiOnly("🇮🇳"), 1);
  assert.equal(emojiOnly("👨‍👩‍👧"), 1);
  assert.equal(emojiOnly("1️⃣"), 1);
  assert.equal(emojiOnly("❤️ ❤️ ❤️"), 3);
  assert.equal(emojiOnly("👍👍👍👍"), 0);
  assert.equal(emojiOnly("ok 👍"), 0);
  assert.equal(emojiOnly("123"), 0);
  assert.equal(emojiOnly(""), 0);
  assert.equal(emojiOnly(null), 0);
});

test("the typing line names who is busy and what they are doing", () => {
  assert.equal(busyWords([]), "");
  assert.equal(busyWords([{ account: scout, activity: "typing" }]), "Scout is typing…");
  assert.equal(busyWords([{ account: bo, activity: "recording_voice" }]), "Bo is recording a voice message…");
  assert.equal(busyWords([{ account: bo, activity: "uploading_file" }]), "Bo is uploading a file…");
  assert.equal(busyWords([{ account: bo, activity: "searching_gifs" }]), "Bo is searching GIFs…");
  assert.equal(busyWords([{ account: bo, activity: "typing" }, { account: scout, activity: "typing" }]), "Bo and Scout are typing…");
  assert.equal(busyWords([{ account: bo, activity: "typing" }, { account: scout, activity: "uploading_file" }]), "Bo is typing and Scout is uploading a file…");
  assert.equal(busyWords([{ account: bo, activity: "typing" }, { account: scout, activity: "typing" }, { account: cy, activity: "typing" }]), "Several people are typing…");
  assert.equal(activityStatus("typing"), "typing…");
  assert.equal(activityStatus("recording_voice"), "recording a voice message…");
});

test("a message's accessible name: who, when, its state and its words", () => {
  assert.equal(messageLabel({ sender: scout, mine: false, time: "22:16", message: message("000", scout, "The build is **green**") }), "Message from Scout at 22:16: The build is green");
  assert.equal(messageLabel({ sender: ada, mine: true, time: "22:17", state: "Read", edited: true, message: message("001", ada, "thanks") }), "Your message at 22:17, Read, edited: thanks");
  assert.equal(messageLabel({ sender: bo, mine: false, time: "22:18", message: message("002", bo, "x", { deleted_at: "2026-10-17T10:00:00Z", message: null }) }), "Deleted message from Bo Okafor at 22:18");
  assert.equal(messageLabel({ sender: bo, mine: false, time: "22:19", message: message("003", bo, "", { attachments: ["https://img.example/a.png"] }) }), "Message from Bo Okafor at 22:19: Photo");
  // A spoken message is named by its caption when it has one (the bubble shows the caption as the message), else by
  // its spoken words.
  const speech = { voice: "mark", script: "[warmly] Down to one point eight seconds.", status: "ready" as const };
  assert.equal(messageLabel({ sender: scout, mine: false, time: "22:20", message: message("004", scout, "Build time: 1.8 s", { speech }) }), "Message from Scout at 22:20: Build time: 1.8 s");
  assert.equal(messageLabel({ sender: scout, mine: false, time: "22:21", message: message("005", scout, "", { speech }) }), "Message from Scout at 22:21: Down to one point eight seconds.");
});

test("a Silicon's custodian: yours, the one listed outside the conversation, or the one other Carbon in it", () => {
  const me = { account: ada, custodian_of: [scout] };
  assert.deepEqual(custodianOf(scout, undefined, me), ada);
  assert.deepEqual(custodianOf(atlas, { participants: [ada, atlas], visible_to_custodians: [bo] }, me), bo);
  assert.deepEqual(custodianOf(atlas, { participants: [ada, bo, atlas], visible_to_custodians: [] }, me), bo);
  // Two Silicons that are not yours: which custodian is whose cannot be told.
  assert.equal(custodianOf(atlas, { participants: [ada, atlas, { ...scout, id: "si:other", uuid: "u-other" }], visible_to_custodians: [bo, cy] }, me), null);
  // Your own Silicon in the conversation does not hide another's custodian.
  assert.deepEqual(custodianOf(atlas, { participants: [ada, scout, atlas], visible_to_custodians: [bo] }, me), bo);
  // Viewing as your Silicon (you are listed as its custodian outside): the other Silicon's custodian is still found.
  assert.deepEqual(custodianOf(atlas, { participants: [scout, atlas], visible_to_custodians: [ada, bo] }, me), bo);
  assert.equal(custodianOf(bo, { participants: [ada, bo] }, me), null);
  // DM 0.17 names a Silicon's custodian on the Silicon itself: told apart in a group too, and "you" for your own.
  const atlasOfBo = { ...atlas, custodian: bo };
  assert.deepEqual(custodianOf(atlasOfBo, { participants: [ada, atlasOfBo, { ...scout, id: "si:other", uuid: "u-other" }], visible_to_custodians: [bo, cy] }, me), bo);
  assert.deepEqual(custodianOf(atlas, { participants: [ada, atlasOfBo, cy], visible_to_custodians: [bo, cy] }, me), bo, "the conversation's own copy of it says so too");
  assert.deepEqual(custodianOf({ ...atlas, custodian: { type: "carbon", id: "c:ada", uuid: "u-ada" } }, undefined, { account: ada, custodian_of: [] }), ada);
  // A custodian DM knows only by uuid is not named (a uuid means nothing to a reader): the conversation decides.
  assert.deepEqual(custodianOf({ ...atlas, custodian: { type: "carbon", id: "0193f3b2-7c41-7a3e-8f00-000000000000" } }, { participants: [ada, atlas], visible_to_custodians: [bo] }, me), bo);
});

test("bubble heights are estimated from what a message carries", () => {
  const short = estimateBubbleHeight(message("000", bo, "hi"));
  const long = estimateBubbleHeight(message("001", bo, "word ".repeat(200)));
  const photo = estimateBubbleHeight(message("002", bo, "", { attachments: ["https://img.example/a.png"] }));
  assert.ok(short >= 36 && short < 60, `short ${short}`);
  assert.ok(long > short * 5, `long ${long}`);
  assert.ok(photo > 200, `photo ${photo}`);
  assert.equal(estimateBubbleHeight(message("003", bo, "x", { deleted_at: "2026-10-17T10:00:00Z" })), 38);
});

test("file types in words", () => {
  assert.equal(friendlyType("application/pdf", "spec.pdf"), "PDF");
  assert.equal(friendlyType(undefined, "notes.zip"), "ZIP archive");
  assert.equal(friendlyType("audio/mpeg", "standup.mp3"), "MP3 audio");
  assert.equal(friendlyType("image/png", "photo.png"), "PNG image");
  assert.equal(friendlyType("application/octet-stream", "data.bin"), "BIN");
  assert.equal(friendlyType(undefined, "README"), "");
});

test("a send leaves the screen when its message is in the thread, each message standing for one send", () => {
  const send = (key: string, text: string, at: number) => ({ key, createdAt: at, body: { message: text } });
  const before = [message("000", bo), message("001", ada, "ok")];
  // Both "ok" sends were written after 001: the older "ok" (001) stands for neither of them.
  const since = { a: "001", b: "001" };
  assert.deepEqual([...arrivedSends([send("a", "ok", 1), send("b", "ok", 2)], before, since, ada)], []);
  const after = [...before, message("002", ada, "ok")];
  assert.deepEqual([...arrivedSends([send("a", "ok", 1), send("b", "ok", 2)], after, since, ada)], ["a"]);
  const both = [...after, message("003", bo, "hi"), message("004", ada, "ok")];
  assert.deepEqual([...arrivedSends([send("a", "ok", 1), send("b", "ok", 2)], both, since, ada)].sort(), ["a", "b"]);
  // Other words, or someone else's message, are not the send.
  assert.deepEqual([...arrivedSends([send("c", "later", 3)], both, { c: "001" }, ada)], []);
  // A send never seen before has no "since": it is left alone.
  assert.deepEqual([...arrivedSends([send("d", "ok", 4)], both, {}, ada)], []);
});

test("a linked message: loaded, on an older page, or said at once why it cannot be shown", () => {
  const loaded = (codes: string[]) => ({ oldest: codes[0] ?? null, newest: codes.at(-1) ?? null, has: (code: string) => codes.includes(code) });
  const page = loaded(["0a0", "0a1", "0a3", "0a4"]);
  assert.equal(locateCode("0a1", page, true), "here");
  assert.equal(locateCode("01z", page, true), "older");
  // Older than everything and nothing before it: not in this conversation.
  assert.equal(locateCode("01z", page, false), "missing");
  // Inside the loaded range but not listed (a bundled original): no page would bring it.
  assert.equal(locateCode("0a2", page, true), "hidden");
  // Newer than anything loaded (zzz on a short thread, a code from another conversation): no older page has it.
  assert.equal(locateCode("zzz", page, true), "newer");
  assert.equal(locateCode("1000", page, true), "newer");
  assert.equal(locateCode("000", loaded([]), true), "older");
  assert.equal(locateCode("000", loaded([]), false), "missing");
  assert.match(unreachableWords("0a2", "hidden").hint, /bundle/);
  assert.match(unreachableWords("zzz", "newer").title, /^Message zzz is not in this thread yet$/);
  assert.match(unreachableWords("001", "too-far").hint, /Scroll up/);
});

test("a message from another day says the day in its name", () => {
  const m = message("000", bo, "hello");
  assert.equal(messageLabel({ sender: bo, mine: false, time: "09:30", day: "Today", message: m }), "Message from Bo Okafor at 09:30: hello");
  assert.equal(messageLabel({ sender: bo, mine: false, time: "09:30", day: "Yesterday", message: m }), "Message from Bo Okafor yesterday at 09:30: hello");
  assert.equal(messageLabel({ sender: ada, mine: true, time: "00:31", day: "Friday, Oct 9", state: "Read", message: m }), "Your message on Friday, Oct 9 at 00:31, Read: hello");
  assert.equal(messageLabel({ sender: bo, mine: false, time: "00:31", day: "Friday, Oct 9", message: { ...m, deleted_at: "2026-10-09T00:00:00Z" } }), "Deleted message from Bo Okafor on Friday, Oct 9 at 00:31");
});

test("the toolbar goes beside the bubble when there is room, else under a selected message, never over another", () => {
  const column = { left: 0, right: 760 };
  const base = { width: 140, compactWidth: 96, height: 36, obstacles: [] };
  const bubble = (left: number, right: number, top = 100) => ({ left, right, top, bottom: top + 40 });
  // Room on the side away from the sender: beside it, with the time.
  assert.deepEqual(toolbarPlacement({ ...base, mine: false, selected: false, bubble: bubble(40, 300), column }), { place: "side", compact: false });
  assert.deepEqual(toolbarPlacement({ ...base, mine: true, selected: true, bubble: bubble(500, 760), column }), { place: "side", compact: false });
  // Exactly the toolbar and its gap fits.
  assert.deepEqual(toolbarPlacement({ ...base, mine: false, selected: false, bubble: bubble(0, 614), column }), { place: "side", compact: false });
  // Only the toolbar without the time fits: beside it, without the time.
  assert.deepEqual(toolbarPlacement({ ...base, mine: false, selected: false, bubble: bubble(40, 650), column }), { place: "side", compact: true });
  // No room beside a message you selected: under it, in the flow of the thread.
  assert.deepEqual(toolbarPlacement({ ...base, mine: false, selected: true, bubble: bubble(40, 700), column }), { place: "below", compact: false });
  assert.deepEqual(toolbarPlacement({ ...base, mine: true, selected: true, bubble: bubble(100, 760), column }), { place: "below", compact: false });
  // The pointer only rests on it: over its top edge when nothing is drawn there…
  assert.deepEqual(toolbarPlacement({ ...base, mine: false, selected: false, bubble: bubble(40, 700), column }), { place: "above", compact: false });
  // …without the time when only that leaves the message before uncovered (its bubble ends left of the toolbar's end)…
  const before = { left: 40, right: 580, top: 50, bottom: 96 };
  assert.deepEqual(toolbarPlacement({ ...base, mine: false, selected: false, bubble: bubble(40, 700), column, obstacles: [before] }), { place: "above", compact: true });
  // …and nowhere when even that would cover something (the message before, a divider, the sender's name).
  const wide = { left: 40, right: 700, top: 50, bottom: 96 };
  assert.deepEqual(toolbarPlacement({ ...base, mine: false, selected: false, bubble: bubble(40, 700), column, obstacles: [wide] }), { place: "none", compact: false });
  assert.deepEqual(toolbarPlacement({ ...base, mine: true, selected: false, bubble: bubble(100, 760), column, obstacles: [{ left: 90, right: 200, top: 80, bottom: 98 }] }), { place: "none", compact: false });
  // Something beside it that it does not reach is no obstacle.
  assert.deepEqual(toolbarPlacement({ ...base, mine: true, selected: false, bubble: bubble(100, 760), column, obstacles: [{ left: 400, right: 760, top: 80, bottom: 98 }] }), { place: "above", compact: false });
});

test("a conversation of several is its people; a custodian reads as its Silicon by name", () => {
  assert.equal(peopleWords(3), "3 people");
  assert.equal(peopleWords(1), "1 person");
  assert.equal(namesInWords([]), "");
  assert.equal(namesInWords(["Bo"]), "Bo");
  assert.equal(namesInWords(["Bo", "Cy"]), "Bo and Cy");
  assert.equal(namesInWords(["Bo", "Cy", "Scout"]), "Bo, Cy and Scout");
  const conversation = summary({ participants: [bo, scout] });
  assert.equal(viewedName("si:scout", scout, conversation), "Scout");
  // Not in the custodian's own list (yet): the conversation's own copy of the Silicon names it.
  assert.equal(viewedName("si:scout", null, conversation), "Scout");
  assert.equal(viewedName("u-scout", { id: "si:scout" }, conversation), "Scout");
  // Nothing knows its name: its id.
  assert.equal(viewedName("si:nobody", null, conversation), "si:nobody");
  assert.equal(viewedName("si:quill", { id: "si:quill" }, undefined), "si:quill");
});

test("a send in the outbox shows what it carries as a sent message would", () => {
  const media = pendingMedia({
    attachments: [
      { permanent_url: "https://bc.example/p/photo.png", name: "photo.png", content_type: "image/png", size: 1200 },
      "https://example.com/notes.pdf",
    ],
    voice: { permanent_url: "https://bc.example/p/note.webm", content_type: "audio/webm", size: 9000, duration_milliseconds: 4200 },
    gif: { provider_id: "g1", url: "https://media.giphy.com/media/x/giphy.gif" },
  });
  assert.deepEqual(media.attachments, [
    { url: "https://bc.example/p/photo.png", name: "photo.png", content_type: "image/png", size: 1200, kind: "image" },
    { url: "https://example.com/notes.pdf", kind: "file" },
  ]);
  assert.deepEqual(media.voice, { url: "https://bc.example/p/note.webm", content_type: "audio/webm", size: 9000, duration_milliseconds: 4200 });
  assert.equal(media.gif?.provider_id, "g1");
  // Words only: nothing else.
  assert.deepEqual(pendingMedia({}), { attachments: [], voice: null, gif: null });
});

test("Copy text copies what the bubble shows: a spoken message's caption (the message), then its words without cues", () => {
  const speech = { voice: "mark", script: "[warmly] Morning. [short pause] The build is green.", status: "ready" as const };
  assert.equal(copyableText(message("000", scout, "", { speech })), "Morning. The build is green.");
  assert.equal(copyableText(message("001", scout, "Release notes", { speech })), "Release notes\n\nMorning. The build is green.");
  assert.equal(copyableText(message("002", bo, "plain words")), "plain words");
  assert.equal(copyableText(message("003", bo, "", { voice_transcript: "a note" })), "a note");
  assert.equal(copyableText(message("004", bo, "x", { deleted_at: "2026-10-17T10:00:00Z" })), "");
});

test("the CLI commands a message's details offer are the ones that work for it", () => {
  const plain = message("00a", bo, "hello");
  assert.equal(messageCommands("g:product", plain, null), "dm messages show g:product 00a\ndm messages send --text 'your reply' --reply-to 00a g:product\ndm messages listen g:product 00a");
  // A custodian's view is read only: no reply, and listening names the Silicon.
  assert.equal(messageCommands("c:dmc-bo::si:dmc-scout", plain, "si:dmc-scout"), "dm messages show c:dmc-bo::si:dmc-scout 00a\ndm messages listen c:dmc-bo::si:dmc-scout 00a --for si:dmc-scout");
  // Nothing to say (a photo alone), or deleted: no listen; deleted: no reply either.
  assert.equal(messageCommands("g:product", message("00b", bo, "", { attachments: ["https://img.example/a.png"] }), null), "dm messages show g:product 00b\ndm messages send --text 'your reply' --reply-to 00b g:product");
  assert.equal(messageCommands("g:product", message("00c", bo, undefined, { message: null, deleted_at: "2026-10-17T10:00:00Z" }), null), "dm messages show g:product 00c");
  // A conversation kept for its history is named by its UUID.
  assert.match(messageCommands("b6f0a3b2-0000-4000-8000-000000000000", plain, null), /^dm messages show b6f0a3b2-0000-4000-8000-000000000000 00a/);
  // On a phone the block is narrow: a command that does not fit continues after a backslash, never sideways.
  const narrow = messageCommands("g:thread-check-s8z53byb", plain, null, 38);
  assert.ok(narrow.split("\n").every(line => line.length <= 38), narrow);
  assert.equal(narrow.split("\n")[0], "dm messages show \\");
  assert.equal(narrow.replace(/ \\\n\s*/g, " "), messageCommands("g:thread-check-s8z53byb", plain, null));
});

test("a long message is cut at a space, and a long file name keeps its end", () => {
  assert.equal(cutAtWord("short", 10), "short");
  assert.equal(cutAtWord("lorem ipsum dolor sit amet", 20), "lorem ipsum dolor");
  // No space near the cut: cut where the limit is.
  assert.equal(cutAtWord("a".repeat(30), 20), "a".repeat(20));
  assert.deepEqual(fileNameParts("report.pdf"), { start: "report.pdf", end: "" });
  const long = `quarterly-report-final-final-v2-really-final-${"x".repeat(20)}-last.zip`;
  const parts = fileNameParts(long);
  assert.equal(parts.start + parts.end, long);
  assert.equal(parts.end, "x-last.zip");
  assert.equal(fileNameParts(`${"n".repeat(40)}`).end, "nnnnnn");
});

test("a first unread only a few more pages can reach is waited for; one further back opens the thread at once", () => {
  // The first page (50) and four older pages of 100 reach 450 back.
  assert.equal(unreadWithinReach(120, 50, 100, 4), true);
  assert.equal(unreadWithinReach(450, 50, 100, 4), true);
  assert.equal(unreadWithinReach(451, 50, 100, 4), false);
  assert.equal(unreadWithinReach(1000, 50, 100, 4), false, "999+ unread opens at once");
  assert.equal(unreadWithinReach(250, 50, 50, 4), true, "pages of one size: as before");
  assert.equal(unreadWithinReach(251, 50, 50, 4), false);
  assert.equal(unreadWithinReach(undefined, 50, 100, 4), true, "an older service says no count: as before");
  assert.equal(unreadWithinReach(3, 0, 0, 4), true, "an empty first page never divides by nothing");
});

test("failures the thread and the composer show: the service's words, or the website's plain words where nothing answered", () => {
  const lost = new ApiError({ status: 0, code: "network_error", message: "Could not reach this site for GET /api/v1/conversations/g%3Ax/messages: Failed to fetch.", hint: "Check your connection, then try again.", method: "GET", path: "/api/v1/conversations/g%3Ax/messages" });
  const plain = plainFailure(lost, false);
  // The words the rest of the website says (lib/errors.ts unreachableWords): a read keeps trying by itself.
  assert.equal(plain.message, "DM can't reach its service right now.");
  assert.equal(plain.hint, "It keeps trying; check your connection if this lasts.");
  assert.equal(plain.code, "network_error", "the code stays for whatever branches on it");
  assert.equal(plainFailure(lost, true).message, "This browser is offline, so DM can't reach its service.");
  // The website's own words for an answer that said nothing stay; anything older is said plainly.
  const worded = new ApiError({ status: 502, code: "server_unavailable", message: "DM's service is not answering right now (HTTP 502).", hint: "Try again in a moment." });
  assert.equal(plainFailure(worded), worded);
  const bare = new ApiError({ status: 502, code: "server_unavailable", message: "POST /api/v1/conversations/x/messages failed: the service could not be reached (HTTP 502), and the answer had no error details." });
  assert.equal(plainFailure(bare).message, "DM did not answer as it should.");
  const refused = new ApiError({ status: 403, code: "contact_not_allowed", message: "Scout does not take conversations from c:dmc-di.", hint: "Ask Scout's custodian to allow you." });
  assert.equal(plainFailure(refused), refused, "the service's own words stay");
  // A send kept in this browser's outbox is a plain object, maybe from an older version: a change, so "try again".
  const kept = plainFailure({ status: 0, code: "network_error", message: "Could not reach this site for POST /api/v1/…" }, false);
  assert.equal(kept.message, "DM can't reach its service right now.");
  assert.equal(kept.hint, "Check your connection, then try again.");
});

test("Message details say who sent a message, when and where, in names", () => {
  assert.equal(conversationPhrase({ whose: "your", others: ["Sol"], title: "Sol" }), "your conversation with Sol");
  assert.equal(conversationPhrase({ whose: "Sol's", others: ["Wren"], title: "Wren" }), "Sol's conversation with Wren");
  assert.equal(conversationPhrase({ whose: "your", others: ["Bo Okafor", "Cy Varga"], title: "Bo Okafor, Cy Varga" }), "your conversation with Bo Okafor and Cy Varga");
  assert.equal(conversationPhrase({ whose: "your", others: [], title: "Launch week" }), "Launch week");
  assert.equal(sentWhen("09:55", "Today"), "today at 09:55");
  assert.equal(sentWhen("09:55", null), "today at 09:55");
  assert.equal(sentWhen("23:10", "Yesterday"), "yesterday at 23:10");
  assert.equal(sentWhen("08:00", "Monday, Oct 12"), "on Monday, Oct 12 at 08:00");
  assert.equal(sentSentence({ time: "09:55", day: "Today", by: "Sol", where: "your conversation with Sol" }), "Sent today at 09:55 by Sol in your conversation with Sol.");
  assert.equal(sentSentence({ time: "10:02", day: "Yesterday", by: "you", where: "Launch week" }), "Sent yesterday at 10:02 by you in Launch week.");
});

test("photos keep their shape: rows that share a height, never thin, two always side by side", () => {
  // Two: side by side, whatever their shapes.
  assert.deepEqual(galleryRows([1.6, 1.6]), [[0, 1]]);
  assert.deepEqual(galleryRows([0.75, 1.78]), [[0, 1]]);
  // Three wide ones: one over two (a row of three would be too thin to read); three tall ones: one row.
  assert.deepEqual(galleryRows([1.6, 1.6, 1.6]), [[0], [1, 2]]);
  assert.deepEqual(galleryRows([0.75, 0.75, 0.75]), [[0, 1, 2]]);
  // Four wide: two over two; a wide one and three tall: one over three.
  assert.deepEqual(galleryRows([1.6, 1.6, 1.6, 1.6]), [[0, 1], [2, 3]]);
  assert.deepEqual(galleryRows([1.6, 0.75, 0.75, 0.75]), [[0], [1, 2, 3]]);
  // More than four: the first four are laid out (the fourth says +N).
  assert.deepEqual(galleryRows([1.6, 1.6, 1.6, 1.6, 1.6, 1.6]), [[0, 1], [2, 3]]);
  assert.deepEqual(galleryRows([1.2]), [[0]]);
  // Extreme shapes are cropped a little, never laid out as slivers.
  assert.equal(galleryRatio(8), 2.6);
  assert.equal(galleryRatio(0.1), 0.5);
  assert.equal(photoRatio(1.6), 1.6);
  assert.equal(photoRatio(4), 4);
  assert.equal(photoRatio(0.1), 0.25);
  assert.equal(photoRatio(9), 6);
});

test("the address a message was routed to: routed_to (in a group too), else an older service's recipient_id", () => {
  // DM 0.17: in a group the recipient is the group; routed_to names the ISI.
  assert.equal(routingOf({ recipient_id: "g:launch-week", routed_to: "release-notes@si:sol" }), "release-notes@si:sol");
  // Not routed: null, even when a direct conversation's recipient is an account.
  assert.equal(routingOf({ recipient_id: "c:mira", routed_to: null }), null);
  // An older service: only a direct conversation's recipient_id says it.
  assert.equal(routingOf({ recipient_id: "standup@si:wren" }), "standup@si:wren");
  assert.equal(routingOf({ recipient_id: "g:launch-week" }), null);
});
