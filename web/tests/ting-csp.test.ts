import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import test from "node:test";
import { configuration } from "../server/config.ts";

const run = promisify(execFile);
test("static browser policy permits exact Ting connections and removes DM socket permission", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "dm-ting-csp-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  await mkdir(join(directory, "dist/client"), { recursive: true });
  await writeFile(join(directory, "dist/client/index.html"), "fixture");
  const build = new URL("../scripts/build-vercel.mjs", import.meta.url)
    .pathname;
  const env = {
    ...process.env,
    VITE_DM_GATEWAY_ORIGIN: "https://gateway.dm.example",
    VITE_DM_TING_BROWSER_ORIGIN: "https://ting.example",
  };
  await run(process.execPath, [build], { cwd: directory, env });
  const config = JSON.parse(
    await readFile(join(directory, ".vercel/output/config.json"), "utf8"),
  );
  const policy: string = config.routes[0].headers["Content-Security-Policy"];
  assert(
    policy.includes(
      "connect-src 'self' https://gateway.dm.example https://ting.example wss://ting.example;",
    ),
  );
  assert(!policy.includes("wss://gateway.dm.example"));
  assert(!policy.includes("https:;"));
  for (const invalid of [
    "https://user:secret@ting.example",
    "https://ting.example/path",
    "https://ting.example/?token=x",
    "http://ting.example",
  ]) {
    await assert.rejects(
      run(process.execPath, [build], {
        cwd: directory,
        env: { ...env, VITE_DM_TING_BROWSER_ORIGIN: invalid },
      }),
    );
    assert.throws(() =>
      configuration({
        DM_WEB_STATE_DIR: directory,
        DM_TING_BROWSER_ORIGIN: invalid,
      }),
    );
  }
});
