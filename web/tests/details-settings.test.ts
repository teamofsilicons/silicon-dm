/** Settings → Report a problem: the version reports quote, and the pull request links the DM service keeps (components/chat/settings/report.ts). */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { pullRequestProblem, WEB_VERSION } from "../components/chat/settings/report";

test("reports quote the website's own version", () => {
  const pkg = JSON.parse(readFileSync(join(process.cwd(), "package.json"), "utf8")) as { version: string };
  assert.equal(WEB_VERSION, pkg.version);
});

test("a pull request link is optional, and DM's own pull requests pass", () => {
  assert.equal(pullRequestProblem(""), null);
  assert.equal(pullRequestProblem("   "), null);
  assert.equal(pullRequestProblem("https://github.com/teamofsilicons/silicon-dm/pull/482"), null);
  assert.equal(pullRequestProblem(" https://github.com/teamofsilicons/silicon-dm/pull/7 "), null);
});

test("links the service would refuse are named before sending, with the shape it wants", () => {
  for (const link of [
    "github.com/teamofsilicons/silicon-dm/pull/482",
    "http://github.com/teamofsilicons/silicon-dm/pull/482",
    "https://github.com/someone/silicon-dm/pull/482",
    "https://github.com/teamofsilicons/silicon-dm/issues/482",
    "https://github.com/teamofsilicons/silicon-dm/pull/482/files",
    "https://github.com/teamofsilicons/silicon-dm/pull/482#discussion",
    "https://github.com/teamofsilicons/silicon-dm/pull/482?w=1",
    "https://github.com/teamofsilicons/silicon-dm/pull/",
    "https://user@github.com/teamofsilicons/silicon-dm/pull/482",
  ]) {
    const problem = pullRequestProblem(link);
    assert.ok(problem, link);
    assert.match(problem!, /https:\/\/github\.com\/teamofsilicons\/silicon-dm\/pull\/123/, link);
  }
});
