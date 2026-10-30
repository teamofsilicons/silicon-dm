import assert from "node:assert/strict";
import { afterEach, beforeEach, test } from "node:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { endSession, locateSession } from "../lib/server/refresh-state";
import { setRefreshStoreForTests } from "../lib/server/refresh-store";
import { exchangeCode, type TokenResult } from "../lib/server/tokens";
import { applyTestEnv, json, memoryRefreshStore, mockFetch, tokenBody, TEST_ENV } from "./env";

let restore = () => {};
beforeEach(() => applyTestEnv());
afterEach(() => restore());

test("a lost exchange response is never retried with the same single-use code", async () => {
  const mock = mockFetch(() => { throw new TypeError("connection closed after Accounts consumed the code"); });
  restore = mock.restore;
  const first = await exchangeCode("sac_lost", "verifier", {});
  assert.equal(first.ok, false);
  assert.deepEqual(await exchangeCode("sac_lost", "verifier", {}), first);
  assert.equal(mock.calls.length, 1);
});

test("a saved exchange cannot disclose its session to another PKCE verifier or restore sign-out", async () => {
  const mock = mockFetch(() => json(200, tokenBody(61)));
  restore = mock.restore;
  const first = await exchangeCode("sac_bound", "correct-verifier", {});
  assert.ok(first.ok);
  assert.equal((await exchangeCode("sac_bound", "different-verifier", {})).ok, false);
  await endSession(first.session);
  const replay = await exchangeCode("sac_bound", "correct-verifier", {});
  assert.ok(!replay.ok && replay.error === "invalid_grant");
  assert.equal(mock.calls.length, 1);
});

test("a late callback gets the current durable session instead of an older token", async () => {
  const mock = mockFetch(() => json(200, tokenBody(62)));
  restore = mock.restore;
  const first = await exchangeCode("sac_rotated", "verifier", {});
  assert.ok(first.ok);
  const { store, key, entry } = await locateSession(first.session);
  const newer = { ...first.session, rt: "sar_newer", at: "newer_access" };
  assert.ok(await store.write(key, entry.version, { session: newer }, newer.re));
  const replay = await exchangeCode("sac_rotated", "verifier", {});
  assert.ok(replay.ok && replay.session.rt === newer.rt && replay.session.sid === first.session.sid);
  assert.equal(mock.calls.length, 1);
});

test("storage failure prevents sending the code", async () => {
  const mock = mockFetch(() => { throw new Error("must not be called"); });
  restore = mock.restore;
  setRefreshStoreForTests({ async read() { throw new Error("unavailable"); }, async write() { throw new Error("unavailable"); } });
  const result = await exchangeCode("sac_unsaved", "verifier", {});
  assert.ok(!result.ok && result.error === "session_storage_unavailable");
  assert.equal(mock.calls.length, 0);
});

test("an abandoned claim never gives a later process permission to reuse the code", async t => {
  t.mock.timers.enable({ apis: ["Date"], now: Date.now() });
  const store = memoryRefreshStore();
  setRefreshStoreForTests({
    read: store.read,
    async write(key, previous, value, expires) {
      if (key.includes("#session#")) throw new Error("process lost storage after consuming the code");
      return store.write(key, previous, value, expires);
    },
  });
  const mock = mockFetch(() => json(200, tokenBody(64)));
  restore = mock.restore;
  assert.equal((await exchangeCode("sac_interrupted", "verifier", {})).ok, false);
  setRefreshStoreForTests(store);
  t.mock.timers.tick(30_000);
  const retry = await exchangeCode("sac_interrupted", "verifier", {});
  assert.ok(!retry.ok && retry.error === "invalid_grant");
  assert.equal(mock.calls.length, 1);
});

test("independent Node processes exchange once, and a restarted process recovers the same session", async () => {
  const dir = await mkdtemp(join(tmpdir(), "code-exchange-"));
  let calls = 0;
  const server = createServer(async (req, res) => {
    for await (const chunk of req) { void chunk; }
    const first = ++calls === 1;
    await new Promise(resolve => setTimeout(resolve, 200));
    res.statusCode = first ? 200 : 400;
    res.setHeader("Content-Type", "application/json");
    res.end(JSON.stringify(first ? tokenBody(63) : { error: "invalid_grant", error_description: "code reused; session revoked" }));
  });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  const run = () => new Promise<TokenResult>((resolve, reject) => {
    const script = "import { exchangeCode } from './lib/server/tokens.ts'; console.log(JSON.stringify(await exchangeCode('sac_shared', 'shared-verifier', {})));";
    const child = spawn(process.execPath, ["--import", "tsx", "--input-type=module", "-e", script], {
      cwd: process.cwd(), env: { ...process.env, ...TEST_ENV, ACCOUNTS_API_URL: `http://127.0.0.1:${address.port}`, REFRESH_STATE_TABLE: "", REFRESH_STATE_PATH: join(dir, "state.sqlite") }, stdio: ["ignore", "pipe", "pipe"],
    });
    let output = ""; let errors = "";
    child.stdout.on("data", b => { output += b; });
    child.stderr.on("data", b => { errors += b; });
    child.on("error", reject);
    child.on("close", code => { if (code) reject(new Error(errors)); else { try { resolve(JSON.parse(output)); } catch (error) { reject(error); } } });
  });
  try {
    const results = await Promise.all([run(), run(), run()]);
    results.push(await run());
    assert.ok(results.every(result => result.ok));
    assert.equal(new Set(results.map(result => result.ok && result.session.sid)).size, 1);
    assert.equal(calls, 1, "only the durable owner sends the code, even across processes");
  } finally {
    await new Promise<void>(resolve => server.close(() => resolve()));
    await rm(dir, { recursive: true, force: true });
  }
});
