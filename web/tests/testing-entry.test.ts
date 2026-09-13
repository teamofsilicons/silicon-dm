import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { Gateway } from "../server/gateway.ts";
import { configuration } from "../server/config.ts";
import { encodeRequest } from "../src/wire.ts";

const environment = "00000000-0000-4000-8000-000000000001";
const production = "00000000-0000-4000-8000-000000000002";
const testProfile = "00000000-0000-4000-8000-000000000003";
const rootKey = "A".repeat(32);
const conversation = "00000000-0000-4000-8000-000000000004";

async function listen(server: Server) {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert(address && typeof address !== "string");
  return `http://127.0.0.1:${address.port}`;
}

async function fixture(t: TestContext, saved = false) {
  const calls: {
    path: string;
    headers: Record<string, unknown>;
    body?: any;
  }[] = [];
  const state = {
    keyStatus: 200,
    discoveryStatus: 200,
    loginStatus: 200,
    meStatus: 200,
    key: rootKey,
  };
  const upstream = createServer(async (req, res) => {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(chunk);
    const body = chunks.length
      ? JSON.parse(Buffer.concat(chunks).toString())
      : undefined;
    calls.push({ path: req.url!, headers: req.headers, body });
    let data: unknown = {};
    if (req.url === "/api/v1/iam") {
      res.statusCode = state.discoveryStatus;
      data = { app_id: "tos>dm", testing_environment_id: environment, testing_environment: { name: "IAM sandbox" } };
    } else if (req.url?.endsWith("/key")) {
      res.statusCode = state.keyStatus;
      data = { environment_id: environment, root_key: state.key };
    } else if (req.url === "/api/v1/auth/login") {
      res.statusCode = state.loginStatus;
      data = {
        access_token: "test-access",
        refresh_token: "test-refresh",
        expires_in: 3600,
        actor: { id: "test-alice", type: "carbon" },
        organization_id: "test-org",
      };
    } else if (req.url === "/api/v1/auth/me") {
      res.statusCode = state.meStatus;
      data = { org_role: "owner", capabilities: [] };
    } else if (req.url?.includes("/messages")) {
      data = {
        id: "test-message",
        message: body?.data.message ?? "Sandbox only",
      };
    }
    res.setHeader("Content-Type", "application/json");
    res.end(JSON.stringify({ type: "response", data }));
  });
  const apiOrigin = await listen(upstream);
  const directory = await mkdtemp(join(tmpdir(), "dm-entry-test-"));
  let gateway: Gateway;
  const frontend = createServer((req, res) => void gateway.handle(req, res));
  const origin = await listen(frontend);
  gateway = new Gateway(
    configuration({
      DM_WEB_ORIGIN: origin,
      DM_API_ORIGIN: apiOrigin,
      DM_WEB_STATE_DIR: directory,
    }),
  );
  await gateway.initialize();
  const browser = await gateway.sessions.create();
  browser.value.selected = production;
  browser.value.profiles = [
    {
      profile_id: production,
      actor: { id: "real-alice", type: "carbon" },
      organization_id: "real-org",
      access_token: "production-access",
      refresh_token: "production-refresh",
      expires_at: Date.now() + 3600000,
    },
  ];
  if (saved)
    browser.value.profiles.push({
      profile_id: testProfile,
      actor: { id: "test-alice", type: "carbon" },
      organization_id: "test-org",
      access_token: "test-access",
      refresh_token: "test-refresh",
      expires_at: Date.now() + 3600000,
      testing_environment_id: environment,
      testing_key: rootKey,
    });
  await gateway.sessions.save(browser);
  t.after(async () => {
    await gateway.close();
    await Promise.all([
      new Promise<void>((r) => frontend.close(() => r())),
      new Promise<void>((r) => upstream.close(() => r())),
    ]);
    await rm(directory, { recursive: true, force: true });
  });
  const request = async (
    path: string,
    body?: unknown,
    profile = production,
    extra: Record<string, string> = {},
  ) => {
    const method = body === undefined ? "GET" : "POST";
    const response = await fetch(origin + path, {
      method,
      headers: {
        Origin: origin,
        Cookie: gateway.sessions.cookie(browser.id).split(";")[0],
        "X-DM-Profile": profile,
        "Content-Type": "application/json",
        ...extra,
      },
      body:
        body === undefined
          ? undefined
          : JSON.stringify(encodeRequest(method, path, body)),
    });
    return { status: response.status, value: (await response.json()).data };
  };
  const enter = (slt?: string, profile = production) =>
    request(
      "/api/testing-environments/enter",
      { environment_id: environment, ...(slt === undefined ? {} : { slt }) },
      profile,
    );
  return { gateway, browser, calls, state, request, enter };
}

test("first entry requests a test identity without exposing the root key or switching production", async (t) => {
  const f = await fixture(t);
  const result = await f.enter();
  assert.equal(result.status, 409);
  assert.equal(result.value.error.code, "testing_login_required");
  assert(!JSON.stringify(result).includes(rootKey));
  assert.equal(
    (await f.gateway.sessions.read(f.browser.id))!.value.selected,
    production,
  );
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0].headers.authorization, "Bearer production-access");
  assert.equal(f.calls[0].headers["x-testing-environment-key"], undefined);
});

test("test login opens the existing DM API with isolated credentials and can return to production", async (t) => {
  const f = await fixture(t);
  const result = await f.enter("test-slt-token");
  assert.equal(result.status, 200);
  assert.equal(result.value.testing_environment_id, environment);
  assert.equal(result.value.actor.id, "test-alice");
  assert.equal(result.value.profiles.length, 2);
  for (const secret of [
    rootKey,
    "test-access",
    "test-refresh",
    "test-slt-token",
  ])
    assert(!JSON.stringify(result).includes(secret));
  const login = f.calls.find((c) => c.path.endsWith("/auth/login"))!;
  assert.equal(login.headers["x-testing-environment-key"], rootKey);
  assert.equal(login.body.data.slt, "test-slt-token");
  const selected = result.value.profile_id;
  const sent = await f.request(
    `/api/dm/conversations/${conversation}/messages`,
    { text: "Sandbox only", metadata: {} },
    selected,
    {
      "Idempotency-Key": "test-send-unique",
      "X-Testing-Environment-Generation": "7",
    },
  );
  assert.equal(sent.status, 200);
  await f.request(
    `/api/dm/conversations/${conversation}/messages`,
    undefined,
    selected,
  );
  for (const call of f.calls.filter((c) => c.path.includes("/messages"))) {
    assert.equal(call.headers.authorization, "Bearer test-access");
    assert.equal(call.headers["x-testing-environment-key"], rootKey);
    assert.equal(call.headers["x-org-id"], "test-org");
  }
  const send = f.calls.find((c) => c.body?.type === "new_message")!;
  assert.equal(send.headers["x-testing-environment-generation"], "7");
  assert.equal(send.body.data.message, "Sandbox only");
  assert.equal(
    (await f.gateway.sessions.read(f.browser.id))!.value.selected,
    selected,
  );
  const restored = await f.request(
    "/api/profiles/select",
    { profile_id: production },
    selected,
  );
  assert.equal(restored.status, 200);
  assert.equal(restored.value.profile_id, production);
  assert.equal(restored.value.testing_environment_id, undefined);
});

test("subsequent visits reuse the saved test profile without a login exchange", async (t) => {
  const f = await fixture(t, true);
  const result = await f.enter();
  assert.equal(result.status, 200);
  assert.equal(result.value.profile_id, testProfile);
  assert(!f.calls.some((c) => c.path.endsWith("/auth/login")));
});

test("rotated keys require a fresh test login and do not reuse the old session", async (t) => {
  const f = await fixture(t, true);
  f.state.key = "B".repeat(32);
  assert.equal((await f.enter()).value.error.code, "testing_login_required");
  assert.equal(
    (await f.gateway.sessions.read(f.browser.id))!.value.selected,
    production,
  );
  assert(!f.calls.some((c) => c.path.endsWith("/auth/me")));
});

test("environment authorization failures never exchange a login token", async (t) => {
  const f = await fixture(t);
  for (const status of [403, 404, 401]) {
    f.state.keyStatus = status;
    const result = await f.enter("test-slt-token");
    assert.equal(result.status, status);
    assert(!f.calls.some((c) => c.path.endsWith("/auth/login")));
    assert.equal(
      (await f.gateway.sessions.read(f.browser.id))!.value.selected,
      production,
    );
  }
});

test("a rejected test token never falls back to production login", async (t) => {
  const f = await fixture(t);
  f.state.loginStatus = 401;
  assert.equal((await f.enter("production-slt-token")).status, 401);
  const attempts = f.calls.filter((c) => c.path.endsWith("/auth/login"));
  assert.equal(attempts.length, 1);
  assert.equal(attempts[0].headers["x-testing-environment-key"], rootKey);
  assert.equal(
    (await f.gateway.sessions.read(f.browser.id))!.value.profiles.length,
    1,
  );
});

test("failed session verification keeps the previous workspace selected", async (t) => {
  const f = await fixture(t);
  f.state.meStatus = 503;
  assert.equal((await f.enter("test-slt-token")).status, 503);
  assert.equal(
    (await f.gateway.sessions.read(f.browser.id))!.value.selected,
    production,
  );
});

test("entry requires production authentication, a valid environment, and the trusted frontend", async (t) => {
  const f = await fixture(t, true);
  assert.equal((await f.enter(undefined, testProfile)).status, 403);
  assert.equal(
    (
      await f.request("/api/testing-environments/enter", {
        environment_id: "../other",
      })
    ).status,
    400,
  );
  assert.equal(
    (
      await f.request(
        "/api/testing-environments/enter",
        { environment_id: environment },
        production,
        { Origin: "https://untrusted.example" },
      )
    ).status,
    403,
  );
  assert.equal(
    (
      await f.request(
        "/api/testing-environments/enter",
        { environment_id: environment },
        production,
        { Cookie: "" },
      )
    ).status,
    401,
  );
  assert.equal(f.calls.length, 0);
});


test("app-secret entry discovers IAM sandbox, accepts a short test ID and keeps the secret server-side", async (t) => {
  const f = await fixture(t);
  const secret = "ask_" + "a".repeat(43);
  const result = await f.request("/api/login", { app_secret: secret, slt: "a" });
  assert.equal(result.status, 200);
  assert.equal(result.value.testing_environment_id, environment);
  assert.equal(result.value.testing_environment_name, "IAM sandbox");
  assert.equal(result.value.profiles.length, 2);
  assert(!JSON.stringify(result).includes(secret));
  const discovery=f.calls.find(c=>c.path==="/api/v1/iam")!;
  assert.equal(discovery.headers["x-testing-environment-key"],secret);
  assert.equal(discovery.headers.authorization,undefined);
  const login=f.calls.find(c=>c.path==="/api/v1/auth/login")!;
  assert.equal(login.headers["x-testing-environment-key"],secret);
  assert.equal(login.body.data.slt,"a");
});
test("revoked app secrets never exchange an identity or use production", async (t) => {
  const f = await fixture(t);
  f.state.discoveryStatus=401;
  const result = await f.request("/api/login", { app_secret: "ask_"+"b".repeat(43), slt: "a" });
  assert.equal(result.status,401);
  assert(!f.calls.some(c=>c.path==="/api/v1/auth/login"));
  assert.equal((await f.gateway.sessions.read(f.browser.id))!.value.selected,production);
});

test("exit restores production or sign-in without revoking saved testing sessions", async (t) => {
  const f=await fixture(t,true);
  await f.enter();
  const result=await f.request("/api/testing-environments/exit",{},testProfile);
  assert.equal(result.status,200);
  assert.equal(result.value.profile_id,production);
  const browser=(await f.gateway.sessions.read(f.browser.id))!;
  browser.value.profiles=browser.value.profiles.filter(p=>p.testing_environment_id);
  browser.value.selected=testProfile;
  await f.gateway.sessions.save(browser);
  const signedOut=await f.request("/api/testing-environments/exit",{},testProfile);
  assert.equal(signedOut.status,200);
  assert.equal(signedOut.value.authenticated,false);
  assert.equal(signedOut.value.profiles.length,1);
});
