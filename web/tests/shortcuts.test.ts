/** The keyboard (lib/shortcuts): keys from events, platform labels, matching by scope, sequences, conflicts. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { cleanOverrides, effectiveShortcuts, findConflicts } from "../lib/shortcuts/bindings";
import { ariaKeyShortcut, chordFromEvent, formatBinding, isPlainBinding, keyId, normalizeBinding, type KeyLike } from "../lib/shortcuts/keys";
import { IDLE, matchChord, SEQUENCE_TIMEOUT_MS } from "../lib/shortcuts/match";
import { isShortcutAvailable, provideShortcuts, setShortcutAvailability, shortcutOffReason } from "../lib/shortcuts/react";
import { SHORTCUTS, type ShortcutScope } from "../lib/shortcuts/registry";

const key = (overrides: Partial<KeyLike>): KeyLike => ({ key: "", code: "", metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...overrides });
const mac = effectiveShortcuts({}, { platform: "mac", sendWith: "enter" });
const scopes = (...extra: ShortcutScope[]) => new Set<ShortcutScope>(["global", ...extra]);

test("keys come from the physical key, with ⌘ as mod on a Mac and Ctrl elsewhere", () => {
  assert.equal(chordFromEvent(key({ key: "k", code: "KeyK", metaKey: true }), "mac"), "mod+k");
  assert.equal(chordFromEvent(key({ key: "k", code: "KeyK", ctrlKey: true }), "other"), "mod+k");
  assert.equal(chordFromEvent(key({ key: "k", code: "KeyK", ctrlKey: true }), "mac"), "ctrl+k", "Control is its own key on a Mac");
  assert.equal(chordFromEvent(key({ key: "˚", code: "KeyK", altKey: true }), "mac"), "alt+k", "⌥K is not ˚");
  assert.equal(chordFromEvent(key({ key: "?", code: "Slash", shiftKey: true }), "mac"), "shift+/");
  assert.equal(chordFromEvent(key({ key: "ArrowUp", code: "ArrowUp", altKey: true, shiftKey: true }), "other"), "alt+shift+up");
  assert.equal(chordFromEvent(key({ key: "Shift", code: "ShiftLeft", shiftKey: true }), "mac"), null, "a modifier alone is no chord");
  assert.equal(keyId({ key: "CapsLock", code: "CapsLock" }), null);
  assert.equal(normalizeBinding("Shift+Mod+L", "mac"), "mod+shift+l");
  assert.equal(normalizeBinding("ctrl+k", "other"), "mod+k");
});

test("labels follow the platform and say sequences and ? the way people read them", () => {
  assert.deepEqual(formatBinding("mod+shift+l", "mac"), { text: "⇧⌘L", spoken: "Shift Command L" });
  assert.deepEqual(formatBinding("mod+shift+l", "other"), { text: "Ctrl+Shift+L", spoken: "Control Shift L" });
  assert.deepEqual(formatBinding("g s", "mac"), { text: "G then S", spoken: "G, then S" });
  assert.deepEqual(formatBinding("shift+/", "mac"), { text: "?", spoken: "Question mark" });
  assert.equal(formatBinding("alt+up", "mac").text, "⌥↑");
  assert.equal(ariaKeyShortcut("mod+k", "mac"), "Meta+K");
  assert.equal(ariaKeyShortcut("mod+k", "other"), "Control+K");
  assert.equal(ariaKeyShortcut("g s", "mac"), null);
  assert.equal(isPlainBinding("n"), true);
  assert.equal(isPlainBinding("shift+n"), true);
  assert.equal(isPlainBinding("mod+k"), false);
});

test("a plain key never fires while typing in a field; a chord with ⌘ does", () => {
  assert.equal(matchChord(mac, IDLE, "n", 0, { scopes: scopes(), inField: false }).id, "conversation.new");
  assert.equal(matchChord(mac, IDLE, "n", 0, { scopes: scopes(), inField: true }).id, undefined);
  assert.equal(matchChord(mac, IDLE, "shift+/", 0, { scopes: scopes(), inField: true }).id, undefined, "? types a question mark");
  assert.equal(matchChord(mac, IDLE, "mod+k", 0, { scopes: scopes(), inField: true }).id, "palette.open");
  assert.equal(matchChord(mac, IDLE, "mod+/", 0, { scopes: scopes(), inField: true }).id, "shortcuts.open");
});

test("scopes decide what a key means: arrows move between messages only on a message, Escape means the layer you are in", () => {
  assert.equal(matchChord(mac, IDLE, "up", 0, { scopes: scopes(), inField: false }).id, undefined);
  assert.equal(matchChord(mac, IDLE, "up", 0, { scopes: scopes("thread", "message"), inField: false }).id, "message.previous");
  assert.equal(matchChord(mac, IDLE, "k", 0, { scopes: scopes("thread", "message"), inField: false }).id, "message.previous");
  assert.equal(matchChord(mac, IDLE, "up", 0, { scopes: scopes("thread", "composer"), inField: true }).id, "composer.editLast");
  assert.equal(matchChord(mac, IDLE, "escape", 0, { scopes: scopes("thread", "composer"), inField: true }).id, "composer.cancel");
  assert.equal(matchChord(mac, IDLE, "escape", 0, { scopes: scopes("thread", "message"), inField: false }).id, "message.back");
  assert.equal(matchChord(mac, IDLE, "enter", 0, { scopes: scopes("thread", "composer"), inField: true }).id, "composer.send");
  assert.equal(matchChord(mac, IDLE, "enter", 0, { scopes: scopes(), inField: true }).id, undefined, "Enter in another field types");
});

test("sequences wait for their next key, and give up after a pause", () => {
  const first = matchChord(mac, IDLE, "g", 1000, { scopes: scopes(), inField: false });
  assert.equal(first.pending, true);
  assert.equal(matchChord(mac, first.state, "s", 1500, { scopes: scopes(), inField: false }).id, "go.settings");
  assert.equal(matchChord(mac, first.state, "v", 1500, { scopes: scopes(), inField: false }).id, "go.voice");
  const late = matchChord(mac, first.state, "s", 1000 + SEQUENCE_TIMEOUT_MS + 1, { scopes: scopes(), inField: false });
  assert.equal(late.id, undefined);
  assert.equal(late.pending, false);
  const restart = matchChord(mac, first.state, "n", 1200, { scopes: scopes(), inField: false });
  assert.equal(restart.id, "conversation.new", "a key that continues nothing is read on its own");
});

test("Settings → Sending switches Enter and ⌘Enter", () => {
  const modEnter = effectiveShortcuts({}, { platform: "mac", sendWith: "mod-enter" });
  const field = { scopes: scopes("thread", "composer"), inField: true };
  assert.equal(matchChord(modEnter, IDLE, "mod+enter", 0, field).id, "composer.send");
  assert.equal(matchChord(modEnter, IDLE, "enter", 0, field).id, "composer.newline");
  assert.equal(matchChord(mac, IDLE, "shift+enter", 0, field).id, "composer.newline");
});

test("the defaults have no conflicts; a rebinding that clashes or shadows a sequence is reported", () => {
  for (const platform of ["mac", "other"] as const) {
    assert.deepEqual(findConflicts(effectiveShortcuts({}, { platform, sendWith: "enter" }), platform), [], platform);
    assert.deepEqual(findConflicts(effectiveShortcuts({}, { platform, sendWith: "mod-enter" }), platform), [], `${platform}, ⌘Enter`);
  }
  const clash = effectiveShortcuts({ "conversation.new": ["mod+k"] }, { platform: "mac", sendWith: "enter" });
  assert.deepEqual(findConflicts(clash, "mac"), [{ binding: "mod+k", ids: ["palette.open", "conversation.new"] }]);
  const shadow = effectiveShortcuts({ "conversation.new": ["g"] }, { platform: "mac", sendWith: "enter" });
  assert.deepEqual(findConflicts(shadow, "mac").map(conflict => conflict.binding), ["g"]);
  const apart = effectiveShortcuts({ "message.reply": ["m"], "composer.selectLast": ["mod+m"] }, { platform: "mac", sendWith: "enter" });
  assert.deepEqual(findConflicts(apart, "mac"), []);
});

test("every default shortcut has an id once, a label and keys; stored overrides are cleaned", () => {
  const ids = SHORTCUTS.map(shortcut => shortcut.id);
  assert.equal(new Set(ids).size, ids.length);
  for (const shortcut of SHORTCUTS) {
    assert.ok(shortcut.label && shortcut.keys.length, shortcut.id);
  }
  assert.deepEqual(cleanOverrides({ "conversation.new": ["mod+n", 3, ""], "no.such": ["x"], "group.new": "nope" }), { "conversation.new": ["mod+n"] });
  assert.deepEqual(cleanOverrides(null), {});
  const off = effectiveShortcuts({ "conversation.new": [] }, { platform: "mac", sendWith: "enter" });
  assert.equal(matchChord(off, IDLE, "n", 0, { scopes: scopes(), inField: false }).id, undefined, "an empty override switches it off");
});

test("a shortcut says whether it works here, and why not: uploads always do; voice waits for the voice module's answer", () => {
  // Uploads exist: when this website takes none, pressing ⌘⇧U says why, so the shortcut is never shown off.
  assert.equal(isShortcutAvailable("composer.upload"), true);
  assert.equal(shortcutOffReason("composer.upload"), null);
  // Voice depends on the service and the browser: off, with a true reason, until the shell's check answers.
  assert.equal(isShortcutAvailable("listen.mode"), false);
  assert.match(shortcutOffReason("listen.mode") ?? "", /^Checking/);
  setShortcutAvailability({ "listen.mode": "Speech is not set up on this DM service, so listening and dictation are off." });
  assert.equal(shortcutOffReason("listen.mode"), "Speech is not set up on this DM service, so listening and dictation are off.");
  setShortcutAvailability({ "listen.mode": null });
  assert.equal(isShortcutAvailable("listen.mode"), true);
  // Withheld again (uploads went away): the reason wins over an earlier "it works".
  provideShortcuts(["composer.record"]);
  setShortcutAvailability({ "composer.record": "Voice messages need uploads, which are off here." });
  assert.equal(isShortcutAvailable("composer.record"), false);
  assert.equal(shortcutOffReason("composer.record"), "Voice messages need uploads, which are off here.");
  // Nothing in the registry is waiting for a feature that has arrived: only the voice shortcuts are checked at run time.
  assert.deepEqual(SHORTCUTS.filter(shortcut => shortcut.planned).map(shortcut => shortcut.group), ["Voice", "Voice", "Voice", "Voice"]);
});
