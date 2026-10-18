/**
 * The ⌘K palette's order (lib/command-rank.ts): a conversation named by what was typed comes before an action that
 * merely contains the letters, the groups stay whole (the arrow keys walk the rows as drawn), and the empty palette
 * lists the open conversation's actions, a few recent conversations and the rest.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { normalizeForSearch, rankCommands, RECENT_GROUP, type RankEntry } from "../lib/command-rank";

/** Mira's palette in her conversation with Sol, registered in the order components/chat/shell/global-commands.tsx uses. */
const palette: RankEntry[] = [
  { id: "conversation.copyAddress", label: "Copy this conversation's address", description: "c:mira::si:sol", group: "This conversation", keywords: ["address", "id", "link"] },
  { id: "conversation.copyLink", label: "Copy its link", description: "http://127.0.0.1:4730/c/c:mira::si:sol", group: "This conversation", keywords: ["url", "share"] },
  { id: "details.toggle", label: "Show the details panel", group: "This conversation" },
  { id: "conversation.new", label: "New conversation", group: "Actions", keywords: ["start", "message", "write"] },
  { id: "conversations.markAllRead", label: "Mark everything read", group: "Actions", keywords: ["clear", "unread"] },
  { id: "shortcuts.open", label: "Keyboard shortcuts", group: "Actions", keywords: ["keys", "cheatsheet", "help"] },
  { id: "go.settings", label: "Settings", group: "Go to", keywords: ["preferences"] },
  { id: "go.voice", label: "Voice settings", group: "Go to", keywords: ["voices"] },
  { id: "go.keyboard", label: "Change keyboard shortcuts", group: "Go to", keywords: ["rebind", "keys"] },
  { id: "account.signout", label: "Sign out", description: "Sign this browser out of DM", group: "Account", keywords: ["log out", "logout"] },
  // Registered after Account, drawn under "This conversation" (the voice actions).
  { id: "listen.mode", label: "Listen mode", description: "Speak new messages from Silicons in this conversation", group: "This conversation", keywords: ["voice"] },
  { id: "view.si:sol", label: "View as si:sol", description: "Sol's conversations, read only", group: "Viewing as", keywords: ["custodian", "silicon", "Sol"] },
  { id: "view.si:wren", label: "View as si:wren", description: "Wren's conversations, read only", group: "Viewing as", keywords: ["custodian", "silicon", "Wren"] },
  { id: "open.g:launch-week", label: "Launch week", description: "Group · g:launch-week", group: "Conversations", conversation: true, names: ["g:launch-week"], members: ["c:dmc-bo", "Bo Okafor", "si:sol", "Sol", "si:wren", "Wren"] },
  { id: "open.c:mira::si:sol", label: "Sol", description: "Silicon · c:mira::si:sol", group: "Conversations", conversation: true, names: ["c:mira::si:sol", "si:sol"] },
  { id: "open.c:mira::si:wren", label: "Wren", description: "Silicon · c:mira::si:wren", group: "Conversations", conversation: true, names: ["c:mira::si:wren", "si:wren"] },
  { id: "open.c:dmc-bo::c:mira", label: "Bo Okafor", description: "Carbon · c:dmc-bo::c:mira", group: "Conversations", conversation: true, names: ["c:dmc-bo::c:mira", "c:dmc-bo"] },
];

const ids = (query: string, options = {}) => rankCommands(palette, query, options).map(entry => entry.id);
const first = (query: string) => ids(query)[0];

test("a conversation's own name wins over an action that contains the letters", () => {
  assert.equal(first("wren"), "open.c:mira::si:wren", "not the read-only View as si:wren");
  assert.equal(first("bo"), "open.c:dmc-bo::c:mira", "not Keyboard shortcuts");
  assert.equal(first("sol"), "open.c:mira::si:sol", "not Copy this conversation's address");
  assert.equal(first("Wren"), "open.c:mira::si:wren", "case does not matter");
  assert.equal(first("okafor"), "open.c:dmc-bo::c:mira", "a word of the name");
  assert.equal(first("launch"), "open.g:launch-week");
  assert.equal(first("si:wren"), "open.c:mira::si:wren", "an id of someone in it");
  assert.equal(first("c:mira::si:sol"), "open.c:mira::si:sol", "its address");
  // Among conversations, the one named so comes before one the account is merely in.
  assert.deepEqual(ids("sol").filter(id => id.startsWith("open.")), ["open.c:mira::si:sol", "open.g:launch-week"]);
  // Groups stay whole, ordered by their best match: the conversations, then the view.
  assert.deepEqual(ids("wren"), ["open.c:mira::si:wren", "open.g:launch-week", "view.si:wren"]);
});

test("actions are found by their names first, then their extra words", () => {
  assert.equal(first("settings"), "go.settings");
  assert.equal(first("Settings"), "go.settings");
  assert.equal(first("Copy this conversation's address"), "conversation.copyAddress");
  assert.equal(first("view as"), "view.si:sol");
  assert.equal(first("log"), "account.signout", "its extra words, when nothing is named so");
  assert.equal(first("unread"), "conversations.markAllRead");
  assert.deepEqual(ids("zzz"), []);
});

test("a contact named like an action comes first on a tie", () => {
  const withMark = [...palette, { id: "open.c:mark::c:mira", label: "Mark Twain", group: "Conversations", conversation: true, names: ["c:mark::c:mira", "c:mark"] }];
  assert.equal(rankCommands(withMark, "mark")[0]!.id, "open.c:mark::c:mira", "not Mark everything read");
});

test("a conversation's name letters in order, from a word's start, are a last resort", () => {
  assert.ok(ids("lnch wk").includes("open.g:launch-week"));
  assert.ok(ids("lnch").includes("open.g:launch-week"));
  assert.ok(!ids("ach").includes("open.g:launch-week"), "the first letter starts a word");
  assert.ok(!ids("lw").includes("open.g:launch-week"), "two letters are too few to guess from");
  assert.equal(first("jose"), undefined);
  const accented = rankCommands([{ id: "open.x", label: "José Núñez", group: "Conversations", conversation: true }], "jose nunez");
  assert.equal(accented[0]?.id, "open.x", "accents do not matter");
  assert.equal(normalizeForSearch("Zoë"), "zoe");
});

test("each group stays whole, so the arrow keys walk the rows in the order they are drawn", () => {
  for (const query of ["", "s", "o", "voice", "c:"]) {
    const groups = rankCommands(palette, query).map(entry => entry.group);
    const seen: Array<string | undefined> = [];
    for (const group of groups) {
      if (seen.at(-1) === group) continue;
      assert.ok(!seen.includes(group), `"${query}": the group ${group} comes back after another`);
      seen.push(group);
    }
  }
  // The voice action registered after Account is drawn, and walked, with the other actions of this conversation.
  const empty = ids("");
  assert.deepEqual(empty.slice(0, 4), ["conversation.copyAddress", "conversation.copyLink", "details.toggle", "listen.mode"]);
});

test("the empty palette lists this conversation's actions, a few recent conversations, then the other actions", () => {
  const entries = rankCommands(palette, "", { current: "open.c:mira::si:sol", recent: 2 });
  const recent = entries.filter(entry => entry.group === RECENT_GROUP).map(entry => entry.id);
  assert.deepEqual(recent, ["open.g:launch-week", "open.c:mira::si:wren"], "the most recent ones, without the one on screen");
  assert.deepEqual([...new Set(entries.map(entry => entry.group))], ["This conversation", RECENT_GROUP, "Actions", "Go to", "Account", "Viewing as"]);
  assert.ok(!entries.some(entry => entry.group === "Conversations"), "the rest are found by typing");
});
