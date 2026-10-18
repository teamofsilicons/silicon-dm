/**
 * The empty field's words (components/chat/composer/placeholder.ts): "Message <name>" whole when it fits the field,
 * else cut at a word, or for one long word at a character, with an ellipsis, never through a letter (a browser cuts a
 * placeholder at the field's edge, whatever its CSS says). Here every character is 8 px wide.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { fitPlaceholder } from "../components/chat/composer/placeholder";

const measure = (text: string) => Array.from(new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(text)).length * 8;

test("the name whole when it fits, and whole when the room is not known yet", () => {
  assert.equal(fitPlaceholder("Message ", "Wren", 200, measure), "Message Wren");
  assert.equal(fitPlaceholder("Message ", "Launch week", 152, measure), "Message Launch week", "exactly as wide as the room");
  assert.equal(fitPlaceholder("Message ", "Launch week", 0, measure), "Message Launch week");
});

test("too long for the room: cut at a word with an ellipsis, never through a letter (320 px phones)", () => {
  // "Message Launch week" is 152 px; with 147 px it was drawn "Message Launch w".
  assert.equal(fitPlaceholder("Message ", "Launch week", 147, measure), "Message Launch…");
  assert.equal(fitPlaceholder("Message ", "Product design", 150, measure), "Message Product…");
  assert.equal(fitPlaceholder("Message ", "Weekly standup notes", 170, measure), "Message Weekly…");
  for (const room of [100, 130, 160, 190, 220]) {
    const shown = fitPlaceholder("Message ", "Marketing team and partners", room, measure);
    assert.ok(measure(shown) <= room, `${shown} fits ${room} px`);
    assert.ok(shown.endsWith("…") && !/\s…$/.test(shown), shown);
  }
});

test("one long word: cut at a character; characters made of several code points stay whole", () => {
  assert.equal(fitPlaceholder("Message ", "Supercalifragilistic", 140, measure), "Message Supercal…");
  const family = fitPlaceholder("Message ", "Family 👨‍👩‍👧‍👦👨‍👩‍👧‍👦👨‍👩‍👧‍👦", 136, measure);
  assert.ok(!family.includes("\u200d…"), "no joiner left dangling before the ellipsis");
  assert.ok(measure(family) <= 136);
  assert.equal(fitPlaceholder("Message ", "Ada", 30, measure), "Message…", "no room even for one character of the name");
});
