import assert from "node:assert/strict";
import { test } from "node:test";
import { insertMention, mentionAt } from "../components/chat/composer/mentions";

test("mentions start at a word boundary and follow the caret, not email or a selection", () => {
  assert.deepEqual(mentionAt("Hi @sa!", 6, 6), { start: 3, end: 6, query: "sa" });
  assert.deepEqual(mentionAt("@saket later", 3, 3), { start: 0, end: 6, query: "sa" });
  assert.equal(mentionAt("mail@example", 12, 12), null);
  assert.equal(mentionAt("@sa", 1, 3), null);
  assert.deepEqual(mentionAt("(@स", 3, 3), { start: 1, end: 3, query: "स" });
});

test("choosing a member replaces the token while preserving the surrounding text", () => {
  assert.deepEqual(insertMention("Hi @sa!", { start: 3, end: 6, query: "sa" }, "c:saket"), { text: "Hi @c:saket!", caret: 11 });
  assert.deepEqual(insertMention("@saket later", { start: 0, end: 6, query: "sa" }, "si:scout"), { text: "@si:scout later", caret: 9 });
  assert.deepEqual(insertMention("Hi @", { start: 3, end: 4, query: "" }, "c:bo"), { text: "Hi @c:bo ", caret: 9 });
});
