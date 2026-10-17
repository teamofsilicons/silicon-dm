/** Markdown-lite (lib/chat/markdown.ts): what chat text means, safely, and linear in its length. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { inlineText, parseInline, parseMarkdown, plainPreview } from "../lib/chat/markdown";

test("paragraphs split on blank lines and keep their line breaks", () => {
  assert.deepEqual(parseMarkdown("first line\nsecond line\n\nnext paragraph"), [
    { type: "paragraph", children: [{ type: "text", text: "first line" }, { type: "break" }, { type: "text", text: "second line" }] },
    { type: "paragraph", children: [{ type: "text", text: "next paragraph" }] },
  ]);
  assert.deepEqual(parseMarkdown(""), []);
  assert.deepEqual(parseMarkdown("a\r\nb"), [{ type: "paragraph", children: [{ type: "text", text: "a" }, { type: "break" }, { type: "text", text: "b" }] }]);
});

test("bold, italic and inline code", () => {
  assert.deepEqual(parseInline("a **bold** and *soft* and `x * y`"), [
    { type: "text", text: "a " },
    { type: "strong", children: [{ type: "text", text: "bold" }] },
    { type: "text", text: " and " },
    { type: "em", children: [{ type: "text", text: "soft" }] },
    { type: "text", text: " and " },
    { type: "code", text: "x * y" },
  ]);
  assert.deepEqual(parseInline("__strong__ _em_"), [{ type: "strong", children: [{ type: "text", text: "strong" }] }, { type: "text", text: " " }, { type: "em", children: [{ type: "text", text: "em" }] }]);
  assert.deepEqual(parseInline("**bold with *italic* inside**"), [
    { type: "strong", children: [{ type: "text", text: "bold with " }, { type: "em", children: [{ type: "text", text: "italic" }] }, { type: "text", text: " inside" }] },
  ]);
});

test("markers that do not open or close stay text: snake_case, spaced stars, unclosed code", () => {
  assert.deepEqual(parseInline("call snake_case_name now"), [{ type: "text", text: "call snake_case_name now" }]);
  assert.deepEqual(parseInline("2 * 3 * 4"), [{ type: "text", text: "2 * 3 * 4" }]);
  assert.deepEqual(parseInline("a `b"), [{ type: "text", text: "a `b" }]);
  assert.deepEqual(parseInline("\\*not em\\*"), [{ type: "text", text: "*not em*" }]);
});

test("bare links become http(s) links without the sentence's punctuation", () => {
  assert.deepEqual(parseInline("see https://dm.teamofsilicons.com/c, ok?"), [
    { type: "text", text: "see " },
    { type: "link", href: "https://dm.teamofsilicons.com/c", text: "https://dm.teamofsilicons.com/c" },
    { type: "text", text: ", ok?" },
  ]);
  assert.deepEqual(parseInline("(https://en.wikipedia.org/wiki/Foo_(bar))"), [
    { type: "text", text: "(" },
    { type: "link", href: "https://en.wikipedia.org/wiki/Foo_(bar)", text: "https://en.wikipedia.org/wiki/Foo_(bar)" },
    { type: "text", text: ")" },
  ]);
  assert.deepEqual(parseInline("www.example.com."), [{ type: "link", href: "https://www.example.com", text: "www.example.com" }, { type: "text", text: "." }]);
});

test("nothing but http and https is ever a link", () => {
  for (const text of ["javascript:alert(1)", "data:text/html,<b>x</b>", "file:///etc/passwd", "<script>alert(1)</script>"]) {
    assert.ok(parseInline(text).every(node => node.type !== "link"), text);
  }
  assert.deepEqual(parseInline("<b>hi</b>"), [{ type: "text", text: "<b>hi</b>" }], "HTML stays text");
});

test("fenced code keeps its text and language; lists keep their numbering", () => {
  assert.deepEqual(parseMarkdown("before\n```ts\nconst a = **1**;\n\nlet b;\n```\nafter"), [
    { type: "paragraph", children: [{ type: "text", text: "before" }] },
    { type: "code", language: "ts", text: "const a = **1**;\n\nlet b;" },
    { type: "paragraph", children: [{ type: "text", text: "after" }] },
  ]);
  assert.deepEqual(parseMarkdown("```\nunclosed"), [{ type: "code", language: null, text: "unclosed" }]);
  assert.deepEqual(parseMarkdown("- one\n- **two**\n\n3. three\n4. four"), [
    { type: "list", ordered: false, start: 1, items: [[{ type: "text", text: "one" }], [{ type: "strong", children: [{ type: "text", text: "two" }] }]] },
    { type: "list", ordered: true, start: 3, items: [[{ type: "text", text: "three" }], [{ type: "text", text: "four" }]] },
  ]);
});

test("previews are one plain line, cut at a word", () => {
  assert.equal(plainPreview("**Deploy** done:\n\n- `api` ok\n- web ok"), "Deploy done: api ok web ok");
  assert.equal(plainPreview("a".repeat(200), 20), `${"a".repeat(19)}…`);
  assert.equal(inlineText(parseInline("x **y** [z]")), "x y [z]");
});

test("hostile input stays linear: thousands of stray markers parse quickly", () => {
  const started = Date.now();
  parseMarkdown("*a _b `c ".repeat(20_000));
  parseMarkdown(`${"**".repeat(30_000)}`);
  assert.ok(Date.now() - started < 3000, `took ${Date.now() - started} ms`);
});
