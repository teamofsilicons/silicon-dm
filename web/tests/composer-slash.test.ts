/**
 * The composer's slash commands (components/chat/composer/slash.ts), what a paste attaches (paste.ts), and the bridge
 * the microphone drives (voice-bridge.ts) with its controls faked: /voice and /dictate start what the microphone
 * registered, and a voice note sent with sendWhenReady goes exactly once, when its upload is done.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { nameParts } from "../components/chat/composer/name-parts";
import { filesToAttach, namedFile, pastedName } from "../components/chat/composer/paste";
import { SendLatch, type TrayState } from "../components/chat/composer/send-latch";
import { normalizeLink, runSlash, slashOptions, type SlashContext } from "../components/chat/composer/slash";
import { trayPayload, trayReducer, type TrayAction, type TrayItem } from "../components/chat/composer/tray";
import { createVoiceBridge, type ComposerVoiceControls } from "../components/chat/composer/voice-bridge";

const names = (text: string) => slashOptions(text).map(option => `${option.command.name}${option.argument ? `:${option.argument}` : ""}`);

test("a slash at the start lists the commands that match, and nothing else is a command", () => {
  assert.deepEqual(names("/"), ["gif", "voice", "dictate", "upload", "link"]);
  assert.deepEqual(names("/d"), ["dictate"]);
  assert.deepEqual(names("/GI"), ["gif"], "case does not matter");
  assert.deepEqual(names("/gif"), ["gif"]);
  assert.deepEqual(names("/gif cats"), ["gif:cats"]);
  assert.deepEqual(names("/gif   cute  cats "), ["gif:cute  cats"], "the words after it, trimmed");
  assert.deepEqual(names("/link example.com/x"), ["link:example.com/x"]);
  assert.deepEqual(names("/voice"), ["voice"]);
  assert.deepEqual(names("/voice please"), [], "a command without words is a message once words follow");
  assert.deepEqual(names("/gifs"), [], "an unknown command is a message");
  assert.deepEqual(names("/shrug"), []);
  assert.deepEqual(names("hello /gif"), [], "only at the start");
  assert.deepEqual(names(" /gif"), []);
  assert.deepEqual(names("/gif cats\nand dogs"), [], "a new line ends it");
  assert.deepEqual(names("/123"), []);
  assert.deepEqual(names(""), []);
});

test("links: https only, a bare address becomes https, anything else is refused", () => {
  assert.equal(normalizeLink("https://example.com/a?b=1#c"), "https://example.com/a?b=1#c");
  assert.equal(normalizeLink("  example.com/docs  "), "https://example.com/docs");
  assert.equal(normalizeLink("docs.example.co.uk"), "https://docs.example.co.uk/");
  assert.equal(normalizeLink("http://example.com"), null, "DM's service accepts https links only");
  assert.equal(normalizeLink("javascript:alert(1)"), null);
  assert.equal(normalizeLink("https://user:secret@example.com"), null, "never a link with credentials");
  assert.equal(normalizeLink("not a link"), null);
  assert.equal(normalizeLink("hello"), null);
  assert.equal(normalizeLink(""), null);
});

function context(overrides: Partial<SlashContext> = {}) {
  const calls: string[] = [];
  const value: SlashContext = {
    voice: null,
    voiceUnavailable: "Voice is not here yet.",
    openGifPicker: query => calls.push(`gif:${query}`),
    openFilePicker: () => calls.push("files"),
    uploadsOff: null,
    addLink: url => calls.push(`link:${url}`),
    openLinkBar: prefill => calls.push(`linkbar:${prefill}`),
    say: words => calls.push(`say:${words}`),
    ...overrides,
  };
  return { calls, value };
}

const option = (text: string) => slashOptions(text)[0]!;

test("running a command: GIF search, file picker, links, and what is said when something is off", () => {
  const { calls, value } = context();
  runSlash(option("/gif cats"), value);
  runSlash(option("/gif"), value);
  runSlash(option("/upload"), value);
  runSlash(option("/link example.com/a"), value);
  runSlash(option("/link not a link"), value);
  runSlash(option("/link"), value);
  runSlash(option("/voice"), value);
  runSlash(option("/dictate"), value);
  assert.deepEqual(calls, ["gif:cats", "gif:", "files", "link:https://example.com/a", "linkbar:not a link", "linkbar:", "say:Voice is not here yet.", "say:Voice is not here yet."]);
  const off = context({ uploadsOff: "Uploads are off: not connected." });
  runSlash(option("/upload"), off.value);
  assert.deepEqual(off.calls, ["say:Uploads are off: not connected."], "with uploads off, /upload says why instead of opening a picker");
});

/** The composer's glue around the bridge, as composer.tsx has it: a tray, the latch, and what a send does. */
function composerHarness(disabledReason: string | null = null) {
  let items: TrayItem[] = [];
  let words = "";
  const sent: Array<ReturnType<typeof trayPayload> & { message: string }> = [];
  const latch = new SendLatch();
  const state = (): TrayState => ({ busy: items.some(item => item.status === "uploading"), failed: items.some(item => item.status === "failed"), content: !!words.trim() || items.some(item => item.status === "ready") });
  const send = () => {
    sent.push({ message: words.trim(), ...trayPayload(items) });
    items = [];
    words = "";
  };
  const controls: { current: ComposerVoiceControls | null } = { current: null };
  const inserted: string[] = [];
  let focused = 0;
  const onTrayChange = () => {
    if (latch.settle(state()) === "send") send();
  };
  const bridge = createVoiceBridge("c:dmc-ada::c:dmc-bo", disabledReason, {
    dispatchTray: (action: TrayAction) => {
      items = trayReducer(items, action);
      onTrayChange();
    },
    requestSend: () => {
      if (latch.request(state()) === "send") send();
    },
    insertText: text => inserted.push(text),
    focusField: () => {
      focused += 1;
    },
    registerControls: next => {
      controls.current = next;
    },
  });
  return { bridge, controls, sent, inserted, latch, setWords: (value: string) => { words = value; }, focused: () => focused };
}

test("the bridge: /voice and /dictate start what the microphone registered, and nothing once it unregistered", () => {
  const harness = composerHarness();
  const started: string[] = [];
  harness.bridge.registerControls({ record: () => started.push("record"), dictate: () => started.push("dictate") });
  const { calls, value } = context({ voice: harness.controls.current });
  runSlash(option("/voice"), value);
  runSlash(option("/dictate"), value);
  assert.deepEqual(started, ["record", "dictate"]);
  assert.deepEqual(calls, []);
  harness.bridge.registerControls(null);
  const after = context({ voice: harness.controls.current });
  runSlash(option("/voice"), after.value);
  assert.deepEqual(started, ["record", "dictate"], "an unmounted microphone is never called");
  assert.deepEqual(after.calls, ["say:Voice is not here yet."]);
});

test("the bridge: a voice note asked to send twice while it uploads goes exactly once, with its transcript", () => {
  const { bridge, sent, latch } = composerHarness();
  const voice = { permanent_url: "https://api.briefcase.test/api/v1/public/u/apps/dm/note.webm?view=inline", content_type: "audio/webm", size: 2048, duration_milliseconds: 4200 };
  bridge.dispatchTray({ type: "add", item: { id: "voice-1", kind: "voice", status: "uploading", progress: 0 } });
  bridge.sendWhenReady();
  bridge.sendWhenReady();
  assert.equal(sent.length, 0, "nothing goes while the note uploads");
  assert.equal(latch.pending, true);
  bridge.dispatchTray({ type: "progress", id: "voice-1", progress: 0.5 });
  assert.equal(sent.length, 0);
  bridge.dispatchTray({ type: "ready", id: "voice-1", voice, transcript: "See you at four." });
  assert.equal(sent.length, 1, "it goes the moment the note is ready");
  assert.deepEqual(sent[0], { message: "", voice, voice_transcript: "See you at four." });
  bridge.dispatchTray({ type: "clear" });
  bridge.sendWhenReady();
  assert.equal(sent.length, 1, "exactly one send: asking again with nothing left sends nothing");
  assert.equal(latch.pending, false);
});

test("the bridge: a failed voice note holds the send (the Carbon decides), and words go with a ready note", () => {
  const harness = composerHarness();
  harness.bridge.dispatchTray({ type: "add", item: { id: "voice-2", kind: "voice", status: "uploading", progress: 0 } });
  harness.bridge.sendWhenReady();
  harness.bridge.dispatchTray({ type: "fail", id: "voice-2", error: "Silicon Briefcase did not answer." });
  assert.equal(harness.sent.length, 0, "a failure never sends without the note");
  assert.equal(harness.latch.pending, false, "and lets the wish go");
  harness.bridge.dispatchTray({ type: "remove", id: "voice-2" });
  harness.setWords("Running late");
  const voice = { permanent_url: "https://api.briefcase.test/api/v1/public/u/apps/dm/n2.webm?view=inline", duration_milliseconds: 900 };
  harness.bridge.dispatchTray({ type: "add", item: { id: "voice-3", kind: "voice", status: "ready", progress: 1, voice, transcript: "Running late." } });
  harness.bridge.sendWhenReady();
  assert.deepEqual(harness.sent, [{ message: "Running late", voice, voice_transcript: "Running late." }]);
});

test("the bridge in a read-only view takes nothing: no tray, no send, no text", () => {
  const harness = composerHarness("You are reading si:scout's conversation as its custodian.");
  harness.bridge.dispatchTray({ type: "add", item: { id: "voice-4", kind: "voice", status: "ready", progress: 1, voice: { permanent_url: "https://x.test/a", duration_milliseconds: 5 } } });
  harness.bridge.sendWhenReady();
  harness.bridge.insertText("hello");
  assert.deepEqual(harness.sent, []);
  assert.deepEqual(harness.inserted, []);
  assert.equal(harness.bridge.disabledReason, "You are reading si:scout's conversation as its custodian.");
  harness.bridge.focusField();
  assert.equal(harness.focused(), 1, "the keyboard can still go back to the field");
});

test("dictation: insertText hands the words to the composer", () => {
  const harness = composerHarness();
  harness.bridge.insertText("meet at the harbour");
  harness.bridge.insertText("");
  assert.deepEqual(harness.inserted, ["meet at the harbour"], "empty dictation inserts nothing");
});

const file = (name: string, type: string) => new File(["data"], name, { type, lastModified: 1 });
const clipboard = (files: File[], text = "") => ({ files, getData: (format: string) => (format === "text/plain" ? text : "") });

test("a paste attaches files when it has no other text than their names; text otherwise", () => {
  const shot = file("image.png", "image/png");
  assert.equal(filesToAttach(clipboard([shot])).length, 1, "a screenshot");
  const finder = file("report.pdf", "application/pdf");
  assert.equal(filesToAttach(clipboard([finder], "report.pdf")).length, 1, "a file copied in the Finder (its name as text)");
  assert.equal(filesToAttach(clipboard([file("image.png", "image/png")], "Q3\t1200\nQ4\t1450")).length, 0, "spreadsheet cells paste as text");
  assert.equal(filesToAttach(clipboard([], "just words")).length, 0);
});

test("a pasted picture with a generic name gets a name of its own", () => {
  const now = new Date(2026, 9, 17, 15, 32, 8);
  assert.equal(pastedName({ name: "image.png", type: "image/png" }, now), "Pasted image 2026-10-17 at 15.32.08.png");
  assert.equal(pastedName({ name: "", type: "image/jpeg" }, now), "Pasted image 2026-10-17 at 15.32.08.jpg");
  assert.equal(namedFile(file("image.png", "image/png"), now).name, "Pasted image 2026-10-17 at 15.32.08.png");
  assert.equal(namedFile(file("Harbour.png", "image/png"), now).name, "Harbour.png", "a real name stays");
  assert.equal(namedFile(file("notes.txt", "text/plain"), now).name, "notes.txt");
});

test("a long file name is cut in its middle: the end and the extension always show", () => {
  assert.deepEqual(nameParts("note-07-mvcstesa.txt"), { head: "note-07-mvcs", end: "tesa.txt" });
  assert.deepEqual(nameParts("Screenshot 2026-10-17 at 15.32.08.png"), { head: "Screenshot 2026-10-17 at 15.3", end: "2.08.png" });
  assert.deepEqual(nameParts("plan.pdf"), { head: "plan.pdf", end: "" }, "a short name is one part");
  assert.deepEqual(nameParts("a-name-without-an-extension"), { head: "a-name-without-an-exten", end: "sion" });
  assert.deepEqual(nameParts("archive.tar.gz"), { head: "archive", end: ".tar.gz" }, "a double extension stays whole");
  const party = nameParts("celebration 👩‍👩‍👧‍👦👩‍👩‍👧‍👦👩‍👩‍👧‍👦👩‍👩‍👧‍👦.png");
  assert.equal(party.end, "👩‍👩‍👧‍👦👩‍👩‍👧‍👦👩‍👩‍👧‍👦👩‍👩‍👧‍👦.png", "never inside an emoji");
  assert.equal(`${party.head}${party.end}`, "celebration 👩‍👩‍👧‍👦👩‍👩‍👧‍👦👩‍👩‍👧‍👦👩‍👩‍👧‍👦.png");
});

test("the bridge: while an edit is open a voice message waits (it would go as a new message); dictation still types", () => {
  const host = { dispatchTray: () => undefined, requestSend: () => undefined, insertText: () => undefined, focusField: () => undefined, registerControls: () => undefined };
  const editing = createVoiceBridge("c:dmc-ada::c:dmc-bo", null, host, "Finish or cancel the edit first: a voice message goes as a new message. Dictation types into your edit.");
  assert.match(editing.notesOffReason ?? "", /^Finish or cancel the edit first/);
  assert.equal(createVoiceBridge("c:dmc-ada::c:dmc-bo", null, host).notesOffReason, null, "no edit: voice messages work");
  assert.equal(createVoiceBridge("c:dmc-ada::c:dmc-bo", "You are reading Scout's conversation as its custodian.", host, "Finish the edit first").notesOffReason, null, "a read-only view says its own reason");
});
