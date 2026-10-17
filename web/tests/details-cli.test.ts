/** The details panel's CLI commands fit a narrow column and still paste into a shell (components/chat/details/cli.ts). */
import assert from "node:assert/strict";
import { test } from "node:test";
import { conversationCommands, layoutCommand } from "../components/chat/details/cli";

test("a command that fits stays on one line", () => {
  assert.equal(layoutCommand(["dm messages list", "g:team"]), "dm messages list g:team");
  // The last word needs no room for a " \" after it.
  assert.equal(layoutCommand(["dm messages list", "c:dmc-ada::si:dmc-scout"], 40), "dm messages list c:dmc-ada::si:dmc-scout");
});

test("a longer one continues after a backslash, indented, and no line is wider than the column", () => {
  const text = layoutCommand(["dm messages send", "--text 'Hello'", "c:dmc-ada::si:dmc-scout"], 34);
  assert.equal(text, "dm messages send --text 'Hello' \\\n  c:dmc-ada::si:dmc-scout");
  for (const line of text.split("\n")) assert.ok(line.length <= 34, line);
});

test("a word too long for an indented line starts at the edge, so a conversation's UUID fits a narrow column", () => {
  const uuid = "01a14b42-a8a9-7662-8c6d-fc9b9d7f596d";
  const text = layoutCommand(["dm messages send", "--text 'Hello'", uuid], 37);
  assert.equal(text, `dm messages send --text 'Hello' \\\n${uuid}`);
  for (const line of text.split("\n")) assert.ok(line.length <= 37, line);
});

test("a conversation's commands: read, send, and a group's members", () => {
  const direct = conversationCommands("c:ada::si:scout", { group: false, width: 60 });
  assert.equal(direct, "dm messages list c:ada::si:scout\ndm messages send --text 'Hello' c:ada::si:scout");
  const group = conversationCommands("g:product-design", { group: true, width: 60 });
  assert.match(group, /^dm groups show g:product-design$/m);
});

test("a custodian's view offers only what works read only: reading, the group, the Silicon's conversations", () => {
  const view = conversationCommands("c:bo::si:scout", { group: false, view: "si:scout", width: 60 });
  assert.equal(view, "dm messages list c:bo::si:scout\ndm conversations list --for si:scout");
  assert.doesNotMatch(view, /messages send/);
  const group = conversationCommands("g:team", { group: true, view: "si:scout", width: 60 });
  assert.equal(group, "dm messages list g:team\ndm groups show g:team\ndm conversations list --for si:scout");
});

test("an address that needs quoting is quoted", () => {
  assert.match(conversationCommands("weird address", { group: false, width: 80 }), /dm messages list 'weird address'/);
});
