/**
 * llms.txt and llms-full.txt (lib/agent/files.ts) only name `dm` commands and `dm docs` topics that exist: every command
 * is in the CLI reference (docs/reference/cli.md, kept to the CLI's own --help), and every topic is one of its packaged
 * guides. A Silicon that copies a command from these files must be able to run it.
 */
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { test } from "node:test";
import { llmsFullTxt } from "../lib/agent/files";

const REFERENCE = new URL("../../docs/reference/cli.md", import.meta.url);
// The website's image is built from web/ alone (web/Dockerfile.gateway), where the repository's docs are not present;
// the repository's own CI runs these tests with them.
const skip = existsSync(REFERENCE) ? false : "docs/ is not beside web/ here (the website's image build)";
const reference = skip ? "" : readFileSync(REFERENCE, "utf8");

/** "dm messages send", "dm login status", … : the first column of the reference's command tables. */
const COMMANDS = new Set([...reference.matchAll(/^\| `(dm [a-z][a-z -]*?)` \|/gm)].map(match => match[1]!.trim()));
/** The topics of `dm docs TOPIC` (and their aliases), from the reference's table of packaged guides. */
const guides = reference.slice(reference.indexOf("## Packaged guides"), reference.indexOf("\n## ", reference.indexOf("## Packaged guides") + 1));
const TOPICS = new Set(
  [...guides.matchAll(/^\| `([a-z-]+)`(?: \(also `([a-z-]+)`\))? \| /gm)].flatMap(match => [match[1]!, match[2]].filter((topic): topic is string => !!topic)),
);

/** Every `dm …` invocation in the text: inline code spans and lines of code blocks. */
function invocations(text: string): string[] {
  const found: string[] = [];
  for (const match of text.matchAll(/`(dm [^`]+)`/g)) found.push(match[1]!);
  for (const line of text.split("\n")) if (line.startsWith("dm ")) found.push(line);
  return found;
}

test("the CLI reference lists the commands and topics this test reads", { skip }, () => {
  for (const command of ["dm messages send", "dm voices list", "dm receipts read", "dm inbox", "dm docs"]) assert.ok(COMMANDS.has(command), command);
  for (const topic of ["voice", "expressive", "building", "receiving"]) assert.ok(TOPICS.has(topic), topic);
});

test("every dm command llms.txt and llms-full.txt name exists", { skip }, () => {
  const text = llmsFullTxt("https://dm.example");
  const named = invocations(text);
  assert.ok(named.length > 10, "the files name the commands a Silicon needs");
  for (const invocation of named) {
    const words = invocation.split(/\s+/);
    if (["--help", "-h", "--version", "-V"].includes(words[1]!)) continue;
    const command = [3, 2].map(count => words.slice(0, count).join(" ")).find(candidate => COMMANDS.has(candidate));
    assert.ok(command, `\`${invocation}\` is not a dm command in docs/reference/cli.md`);
    if (command === "dm docs" && words[2] && !words[2].startsWith("-")) assert.ok(TOPICS.has(words[2]), `dm docs ${words[2]} is not a packaged guide`);
  }
});
