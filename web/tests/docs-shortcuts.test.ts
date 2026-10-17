/**
 * The docs list every keyboard shortcut (docs/reference/keyboard-shortcuts.md, packaged as `dm docs shortcuts`): the page's
 * tables are exactly keyboardReferenceMarkdown(), which reads lib/shortcuts/registry.ts. A shortcut added, removed,
 * renamed or rebound in the registry fails this test until the page is updated: paste the tables printed below over the
 * page's tables (everything from "## General" to the end).
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { keyboardReferenceMarkdown } from "../lib/agent/files";
import { SHORTCUTS } from "../lib/shortcuts/registry";

const PAGE = new URL("../../docs/reference/keyboard-shortcuts.md", import.meta.url);

test("the keyboard reference in the docs matches the shortcut registry", () => {
  const page = readFileSync(PAGE, "utf8");
  const tables = keyboardReferenceMarkdown();
  assert.ok(
    page.includes(tables),
    `docs/reference/keyboard-shortcuts.md is out of date with lib/shortcuts/registry.ts. Replace its tables with:\n\n${tables}`,
  );
  const rows = page.split("\n").filter(line => line.startsWith("| `"));
  assert.equal(rows.length, SHORTCUTS.length, "one table row per shortcut, and no rows for shortcuts that are gone");
});

