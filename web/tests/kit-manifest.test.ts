/**
 * The vendored Silicon UI kit (components/silicon-ui) is exactly what silicon-ui-registry.json records: every file is
 * listed with the sha256 of its bytes, and nothing unlisted hides beside them. A file changed here on purpose gets its
 * new hash and a `local_edit` that says what changed and why (keep such edits small: the next install from
 * https://ui.teamofsilicons.com/r/<name>.json must be able to carry them over).
 */
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { test } from "node:test";

const WEB = new URL("..", import.meta.url).pathname;
const KIT = join(WEB, "components", "silicon-ui");

interface Entry {
  registry: string;
  sha256: string;
  local_edit?: string;
}

const manifest = JSON.parse(readFileSync(join(WEB, "silicon-ui-registry.json"), "utf8")) as { files: Record<string, Entry> };

function filesUnder(directory: string): string[] {
  return readdirSync(directory).flatMap(name => {
    const path = join(directory, name);
    return statSync(path).isDirectory() ? filesUnder(path) : [relative(WEB, path)];
  });
}

test("every vendored kit file matches the hash the manifest records", () => {
  const changed: string[] = [];
  for (const [path, entry] of Object.entries(manifest.files)) {
    const hash = createHash("sha256").update(readFileSync(join(WEB, path))).digest("hex");
    if (hash !== entry.sha256) changed.push(`${path}: ${hash}`);
  }
  assert.deepEqual(changed, [], "these kit files changed without the manifest: record each new sha256 in silicon-ui-registry.json, with a local_edit saying what changed and why");
});

test("every file in components/silicon-ui is in the manifest, from the registry, and a local edit says why", () => {
  const listed = new Set(Object.keys(manifest.files));
  assert.deepEqual(filesUnder(KIT).filter(path => !listed.has(path)), [], "unlisted files in the vendored kit");
  for (const [path, entry] of Object.entries(manifest.files)) {
    assert.match(entry.registry, /^https:\/\/ui\.teamofsilicons\.com\/r\/[a-z0-9-]+\.json$/, path);
    if (entry.local_edit !== undefined) assert.ok(entry.local_edit.trim().length > 20, `${path}: say what the local edit changed and why`);
  }
});
