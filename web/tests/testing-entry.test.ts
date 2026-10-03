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
    loginActor: { id: "c:test-alice", type: "carbon" },
    meStatus: 200,
    beforeMeReply: undefined as (() => Promise<void>) | undefined,
    beforeLoginReply: undefined as (() => Promise<void>) | undefined,
    consentStatus: 200,
    loginOrganizations: ["test-org"],
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
      data = {
        app_id: "dm",
        testing_environment_id: environment,
        testing_environment: { name: "IAM sandbox" },
      };
    } else if (req.url?.endsWith("/key")) {
      res.statusCode = state.keyStatus;
      data = { environment_id: environment, root_key: state.key };
    } else if (req.url === "/api/v1/auth/login") {
      res.statusCode = state.loginStatus;
      data = {
        access_token: "test-access",
        refresh_token: "test-refresh",
        expires_in: 3600,
        actor: state.loginActor,
        organization_id: "test-org",
        organization_ids: state.loginOrganizations,
      };
    } else if (req.url === "/api/v1/auth/me") {
      res.statusCode = state.meStatus;
      data = {
        member: {
          id:
            req.headers.authorization === "Bearer production-access"
              ? "c:real-alice"
              : "c:test-alice",
          type: "carbon",
        },
        organization_id: req.headers["x-org-id"],
        org_role: "owner",
        capabilities: [],
      };
    } else if (req.url === "/api/v1/delivery/authorization") {
      res.statusCode = state.consentStatus;
      data =
        state.consentStatus === 401
          ? {
              error: {
                code: "ting_authorization_required",
                message: "Review permission",
              },
            }
          : {};
    } else if (req.url?.includes("/messages")) {
      data = {
        id: "test-message",
        message: body?.data.message ?? "Sandbox only",
      };
    }
    if (req.url === "/api/v1/auth/me") await state.beforeMeReply?.();
    if (req.url === "/api/v1/auth/login") await state.beforeLoginReply?.();
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
      actor: { id: "c:real-alice", type: "carbon" },
      organization_id: "real-org",
      access_token: "production-access",
      refresh_token: "production-refresh",
      expires_at: Date.now() + 3600000,
    },
  ];
  if (saved)
    browser.value.profiles.push({
      profile_id: testProfile,
      actor: { id: "c:test-alice", type: "carbon" },
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
  return {
    gateway,
    browser,
    calls,
    state,
    request,
    enter,
    restart: async () => {
      const config = gateway.config;
      await gateway.close();
      gateway = new Gateway(config);
      await gateway.initialize();
    },
  };
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
  assert.equal(result.value.actor.id, "c:test-alice");
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
  const send = f.calls.find((c) => c.body?.type === "message.create")!;
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

for (const status of [429, 503]) {
  test(`a profile switch rejected by upstream ${status} preserves the durable selection and can be retried`, async (t) => {
    const f = await fixture(t, true);
    f.state.meStatus = status;
    const rejected = await f.request("/api/profiles/select", {
      profile_id: testProfile,
    });
    assert.equal(rejected.status, 503);
    assert.equal(rejected.value.error.code, "session_unavailable");
    assert.equal(
      (await f.gateway.sessions.read(f.browser.id))!.value.selected,
      production,
    );
    f.state.meStatus = 200;
    const prior = await f.request("/api/session", undefined, "");
    assert.equal(prior.value.profile_id, production);
    const recovered = await f.request("/api/profiles/select", {
      profile_id: testProfile,
    });
    assert.equal(recovered.status, 200);
    assert.equal(recovered.value.profile_id, testProfile);
    assert.equal(
      (await f.gateway.sessions.read(f.browser.id))!.value.selected,
      testProfile,
    );
    const checks = f.calls.filter((c) => c.path === "/api/v1/auth/me");
    assert.deepEqual(
      checks.map((c) => [c.headers.authorization, c.headers["x-org-id"]]),
      [
        ["Bearer test-access", "test-org"],
        ["Bearer production-access", "real-org"],
        ["Bearer test-access", "test-org"],
      ],
    );
  });
}

for (const drift of ["actor", "organization", "world", "revoked"] as const) {
  test(`a profile ${drift} change during verification cannot commit a stale selection`, async (t) => {
    const f = await fixture(t, true);
    f.state.beforeMeReply = async () => {
      await f.gateway.sessions.locked(f.browser.id, async () => {
        const saved = (await f.gateway.sessions.read(f.browser.id))!;
        const target = saved.value.profiles.find(
          (profile) => profile.profile_id === testProfile,
        )!;
        if (drift === "actor") target.actor.id = "c:another-account";
        if (drift === "organization") target.organization_id = "another-org";
        if (drift === "world")
          target.testing_environment_id =
            "00000000-0000-4000-8000-000000000099";
        if (drift === "revoked") target.auth_required = true;
        await f.gateway.sessions.save(saved);
      });
    };
    const result = await f.request("/api/profiles/select", {
      profile_id: testProfile,
    });
    assert.equal(result.status, 409);
    assert.equal(result.value.error.code, "profile_changed");
    assert.equal(
      (await f.gateway.sessions.read(f.browser.id))!.value.selected,
      production,
    );
  });
}

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
  const result = await f.request("/api/login", {
    app_secret: secret,
    slt: "a",
  });
  assert.equal(result.status, 200);
  assert.equal(result.value.testing_environment_id, environment);
  assert.equal(result.value.testing_environment_name, "IAM sandbox");
  assert.equal(result.value.profiles.length, 2);
  assert(!JSON.stringify(result).includes(secret));
  const discovery = f.calls.find((c) => c.path === "/api/v1/iam")!;
  assert.equal(discovery.headers["x-testing-environment-key"], secret);
  assert.equal(discovery.headers.authorization, undefined);
  const login = f.calls.find((c) => c.path === "/api/v1/auth/login")!;
  assert.equal(login.headers["x-testing-environment-key"], secret);
  assert.equal(login.body.data.slt, "a");
});
test("revoked app secrets never exchange an identity or use production", async (t) => {
  const f = await fixture(t);
  f.state.discoveryStatus = 401;
  const result = await f.request("/api/login", {
    app_secret: "ask_" + "b".repeat(43),
    slt: "a",
  });
  assert.equal(result.status, 401);
  assert(!f.calls.some((c) => c.path === "/api/v1/auth/login"));
  assert.equal(
    (await f.gateway.sessions.read(f.browser.id))!.value.selected,
    production,
  );
});

test("exit restores production or sign-in without revoking saved testing sessions", async (t) => {
  const f = await fixture(t, true);
  await f.enter();
  const result = await f.request(
    "/api/testing-environments/exit",
    {},
    testProfile,
  );
  assert.equal(result.status, 200);
  assert.equal(result.value.profile_id, production);
  const browser = (await f.gateway.sessions.read(f.browser.id))!;
  browser.value.profiles = browser.value.profiles.filter(
    (p) => p.testing_environment_id,
  );
  browser.value.selected = testProfile;
  await f.gateway.sessions.save(browser);
  const signedOut = await f.request(
    "/api/testing-environments/exit",
    {},
    testProfile,
  );
  assert.equal(signedOut.status, 200);
  assert.equal(signedOut.value.authenticated, false);
  assert.equal(signedOut.value.profiles.length, 1);
});

test("groups use the authenticated gateway allowlist and selected sandbox credentials", async (t) => {
  const f = await fixture(t);
  const entered = await f.enter("test-slt-token");
  const response = await f.request(
    "/api/dm/groups",
    { name: "Research", member_ids: [] },
    entered.value.profile_id,
  );
  assert.equal(response.status, 200);
  const sent = f.calls.find((call) => call.path === "/api/v1/groups");
  assert(sent);
  assert.equal(sent.headers.authorization, "Bearer test-access");
  assert.equal(sent.headers["x-testing-environment-key"], rootKey);
  assert.deepEqual(sent.body, {
    type: "create_group",
    data: { name: "Research", member_ids: [] },
  });
});

test("group addresses pass gateway routing and preserve sandbox credentials", async (t) => {
  const f = await fixture(t);
  const entered = await f.enter("test-slt-token");
  for (const path of [
    "groups/g:tos:product-design/members",
    "conversations/g:tos:product-design/messages",
    "conversations/g%3Atos%3Aproduct-design/messages",
  ]) {
    const response = await f.request(
      `/api/dm/${path}`,
      { member_ids: [], message: "Hello", metadata: {} },
      entered.value.profile_id,
    );
    assert.equal(response.status, 200);
    const sent = f.calls.find(
      (call) => call.path === `/api/v1/${path.replaceAll("%3A", ":")}`,
    );
    assert(sent);
    assert.equal(sent.headers.authorization, "Bearer test-access");
    assert.equal(sent.headers["x-testing-environment-key"], rootKey);
  }
  for (const id of [
    "g:tos:bad--slug",
    "g:tos:-bad",
    "g:tos:UPPER",
    "g:tos:bad%2Fmembers",
  ]) {
    const response = await f.request(
      `/api/dm/groups/${id}/members`,
      { member_ids: [] },
      entered.value.profile_id,
    );
    assert.equal(response.status, id.includes("%2F") ? 400 : 404);
  }
});

test("public conversation addresses and short message IDs reach drafts and messages", async (t) => {
  const f = await fixture(t);
  const entered = await f.enter("test-slt-token");
  for (const path of [
    "conversations/alice%3A%3Abob/draft",
    "conversations/alice%40example.com%3A%3Abob/draft",
    "conversations/alice%2Bwork%40example.com%3A%3Abob/draft",
    "conversations/alice%3A%3Abob/messages/000",
    "conversations/alice%3A%3Abob/bundles/001",
  ]) {
    const result = await f.request(
      `/api/dm/${path}`,
      undefined,
      entered.value.profile_id,
    );
    assert.equal(result.status, 200, path);
    const sent = f.calls.find(
      (c) => c.path === `/api/v1/${decodeURIComponent(path)}`,
    );
    assert(sent, path);
    assert.equal(sent.headers.authorization, "Bearer test-access");
    assert.equal(sent.headers["x-testing-environment-key"], rootKey);
  }
  for (const path of [
    "conversations/alice%2Fauth/draft",
    "conversations/alice/messages/../auth",
    "conversations/alice%253Abob/draft",
  ]) {
    const before = f.calls.length;
    const result = await f.request(
      `/api/dm/${path}`,
      undefined,
      entered.value.profile_id,
    );
    assert([400, 404].includes(result.status), path);
    assert.equal(f.calls.length, before);
  }
});

test("explicit Ting consent routes retain the original sandbox profile and retry identity", async (t) => {
  const f = await fixture(t, true);
  for (const [path, body] of [
    ["/delivery/authorization", undefined],
    ["/delivery/authorization", {}],
    [
      "/delivery/authorization/complete",
      { authorization_id: production, authorization_code: "one-use-code" },
    ],
    ["/delivery/authorization/disconnect", {}],
  ] as const) {
    const response = await f.request("/api/dm" + path, body, testProfile, {
      "Idempotency-Key": "stable-consent-key",
    });
    assert.equal(response.status, 200);
    const call = f.calls.at(-1)!;
    assert.equal(call.path, "/api/v1" + path);
    assert.equal(call.headers.authorization, "Bearer test-access");
    assert.equal(call.headers["x-org-id"], "test-org");
    assert.equal(call.headers["x-testing-environment-key"], rootKey);
    assert.equal(call.headers["idempotency-key"], "stable-consent-key");
  }
});

test("provider consent errors do not expire the saved DM login", async (t) => {
  const f = await fixture(t, true);
  f.state.consentStatus = 401;
  const before = (await f.gateway.sessions.read(
    f.browser.id,
  ))!.value.profiles.find((p) => p.profile_id === testProfile)!;
  const response = await f.request(
    "/api/dm/delivery/authorization",
    {},
    testProfile,
    { "Idempotency-Key": "feature-consent-key" },
  );
  assert.equal(response.status, 401);
  const after = (await f.gateway.sessions.read(
    f.browser.id,
  ))!.value.profiles.find((p) => p.profile_id === testProfile)!;
  assert.equal(after.expires_at, before.expires_at);
  assert.equal(after.refresh_token, before.refresh_token);
});

test("an IAM login can save only one organization and rejects legacy multi-org responses", async (t) => {
  const f = await fixture(t);
  f.state.loginOrganizations = ["test-org", "other-org"];
  const before = (await f.gateway.sessions.read(f.browser.id))!.value;
  const failed = await f.request("/api/login", { slt: "test-slt" });
  assert.equal(failed.status, 502);
  assert.deepEqual(
    (await f.gateway.sessions.read(f.browser.id))!.value,
    before,
  );
  f.state.loginOrganizations = ["test-org"];
  const good = await f.request("/api/login", { slt: "single-org-slt" });
  assert.equal(good.status, 200);
  assert.equal(good.value.organization_id, "test-org");
  assert.equal(good.value.profiles.length, 2);
});

async function navigateLogin(
  f: Awaited<ReturnType<typeof fixture>>,
  path: string,
) {
  return fetch(new URL(path, f.gateway.config.origin), {
    redirect: "manual",
    headers: { Cookie: f.gateway.sessions.cookie(f.browser.id).split(";")[0] },
  });
}
async function startLogin(
  f: Awaited<ReturnType<typeof fixture>>,
  kind: "carbon" | "silicon",
  popup = true,
) {
  const response = await navigateLogin(
    f,
    `/auth/login?identity_kind=${kind}&profile_id=${production}${popup ? `&popup_nonce=${"a".repeat(64)}` : ""}`,
  );
  assert.equal(response.status, 303);
  const target = new URL(response.headers.get("location")!);
  assert.equal(target.searchParams.get("identity_kind"), kind);
  assert.equal(target.searchParams.get("display"), popup ? "popup" : null);
  const callback = new URL(target.searchParams.get("redirect_uri")!);
  assert(callback.searchParams.get("state"));
  callback.searchParams.set("slt", "callback-test-slt");
  return callback;
}
for (const kind of ["carbon", "silicon"] as const) {
  for (const popup of [true, false]) {
    test(`${kind} ${popup ? "popup" : "full-page fallback"} binds callback and returns only the saved profile`, async (t) => {
      const f = await fixture(t);
      f.state.loginActor = {
        id: kind === "carbon" ? "c:new-alice" : "si:new-alice",
        type: kind,
      };
      const callback = await startLogin(f, kind, popup);
      const result = await navigateLogin(f, callback.href);
      assert.equal(result.status, 303);
      const target = new URL(result.headers.get("location")!);
      const stored = (await f.gateway.sessions.read(f.browser.id))!.value;
      assert.equal(stored.profiles.length, 2);
      assert.equal(
        stored.profiles.find((p) => p.profile_id === stored.selected)?.actor
          .type,
        kind,
      );
      assert.equal(stored.flow?.completed, stored.selected);
      assert.equal(
        target.searchParams.get("profile_id"),
        popup ? stored.selected : null,
      );
      assert.equal(
        target.searchParams.get("nonce"),
        popup ? "a".repeat(64) : null,
      );
      assert.equal(
        target.searchParams.get("iam_popup"),
        popup ? "complete" : null,
      );
      assert(!target.href.includes("slt"));
      assert(!target.href.includes("access"));
    });
  }
}
test("popup rejects mismatched returned identity without replacing the previous account", async (t) => {
  const f = await fixture(t);
  const callback = await startLogin(f, "silicon");
  const result = await navigateLogin(f, callback.href);
  assert.equal(result.status, 403);
  const saved = (await f.gateway.sessions.read(f.browser.id))!.value;
  assert.equal(saved.selected, production);
  assert.equal(saved.profiles.length, 1);
});
for (const status of [429, 503]) {
  test(`popup ${status} retains the callback attempt and replays the same exchange key`, async (t) => {
    const f = await fixture(t);
    const callback = await startLogin(f, "carbon");
    f.state.loginStatus = status;
    const result = await navigateLogin(f, callback.href);
    assert.equal(result.status, 503);
    assert.equal(result.headers.get("referrer-policy"), "no-referrer");
    const text = await result.text();
    assert(text.includes("Retry sign-in"));
    assert(!text.includes("callback-test-slt"));
    const saved = (await f.gateway.sessions.read(f.browser.id))!.value;
    assert.equal(saved.selected, production);
    assert(saved.flow);
    f.state.loginStatus = 200;
    assert.equal((await navigateLogin(f, callback.href)).status, 303);
    const exchanges = f.calls.filter((c) => c.path === "/api/v1/auth/login");
    assert.equal(exchanges.length, 2);
    assert.equal(
      exchanges[0].headers["idempotency-key"],
      exchanges[1].headers["idempotency-key"],
    );
    assert.deepEqual(exchanges[0].body, exchanges[1].body);
  });
}
test("account switch away and back invalidates a pending popup before any token exchange", async (t) => {
  const f = await fixture(t, true);
  const callback = await startLogin(f, "carbon");
  assert.equal(
    (await f.request("/api/profiles/select", { profile_id: testProfile }))
      .status,
    200,
  );
  assert.equal(
    (
      await f.request(
        "/api/profiles/select",
        { profile_id: production },
        testProfile,
      )
    ).status,
    200,
  );
  assert.equal((await navigateLogin(f, callback.href)).status, 400);
  assert(!f.calls.some((c) => c.path === "/api/v1/auth/login"));
});
test("popup rejects malformed kind, nonce, duplicate fields and stale initiating profile", async (t) => {
  const f = await fixture(t);
  for (const query of [
    "identity_kind=robot",
    "popup_nonce=" + "a".repeat(64),
    "identity_kind=carbon&popup_nonce=short",
    "identity_kind=carbon&identity_kind=silicon",
    "identity_kind=carbon&profile_id=bad",
  ])
    assert.equal((await navigateLogin(f, "/auth/login?" + query)).status, 400);
  assert.equal(
    (
      await navigateLogin(
        f,
        `/auth/login?identity_kind=carbon&profile_id=${testProfile}`,
      )
    ).status,
    409,
  );
  assert.equal(f.calls.length, 0);
});
test("completed popup callback replays after restart without issuing another login", async (t) => {
  const f = await fixture(t);
  const callback = await startLogin(f, "carbon");
  const first = await navigateLogin(f, callback.href);
  assert.equal(first.status, 303);
  const destination = first.headers.get("location");
  await f.restart();
  const replay = await navigateLogin(f, callback.href);
  assert.equal(replay.status, 303);
  assert.equal(replay.headers.get("location"), destination);
  assert.equal(
    f.calls.filter((c) => c.path === "/api/v1/auth/login").length,
    1,
  );
  callback.searchParams.set("slt", "different-slt");
  assert.equal((await navigateLogin(f, callback.href)).status, 409);
  assert.equal(
    f.calls.filter((c) => c.path === "/api/v1/auth/login").length,
    1,
  );
});
test("nonce cancellation prevents a delayed callback from exchanging credentials", async (t) => {
  const f = await fixture(t);
  const callback = await startLogin(f, "carbon");
  assert.equal(
    (await f.request("/api/login/cancel", { nonce: "a".repeat(64) })).status,
    200,
  );
  assert.equal((await navigateLogin(f, callback.href)).status, 400);
  assert.equal(
    (await f.gateway.sessions.read(f.browser.id))!.value.selected,
    production,
  );
  assert(!f.calls.some((c) => c.path === "/api/v1/auth/login"));
});
test("late cancellation restores initiating selection and never reactivates completed login", async (t) => {
  const f = await fixture(t);
  const callback = await startLogin(f, "carbon");
  assert.equal((await navigateLogin(f, callback.href)).status, 303);
  const completed = (await f.gateway.sessions.read(f.browser.id))!.value
    .selected;
  assert.notEqual(completed, production);
  assert.equal(
    (await f.request("/api/login/cancel", { nonce: "a".repeat(64) })).status,
    200,
  );
  const saved = (await f.gateway.sessions.read(f.browser.id))!.value;
  assert.equal(saved.selected, production);
  assert.equal(saved.profiles.length, 2);
  assert.equal((await navigateLogin(f, callback.href)).status, 400);
});
test("cancelling an older popup cannot cancel a newer attempt", async (t) => {
  const f = await fixture(t);
  await startLogin(f, "carbon");
  const started = await navigateLogin(
    f,
    `/auth/login?identity_kind=carbon&popup_nonce=${"b".repeat(64)}`,
  );
  const callback = new URL(
    new URL(started.headers.get("location")!).searchParams.get("redirect_uri")!,
  );
  callback.searchParams.set("slt", "callback-test-slt");
  assert.equal(
    (await f.request("/api/login/cancel", { nonce: "a".repeat(64) })).status,
    200,
  );
  assert.equal((await navigateLogin(f, callback.href)).status, 303);
});
test("cancellation waits for an in-flight exchange then restores selection atomically", async (t) => {
  const f = await fixture(t);
  const callback = await startLogin(f, "carbon");
  let entered!: () => void, release!: () => void;
  const waiting = new Promise<void>((resolve) => {
    entered = resolve;
  });
  const released = new Promise<void>((resolve) => {
    release = resolve;
  });
  f.state.beforeLoginReply = async () => {
    entered();
    await released;
  };
  const completion = navigateLogin(f, callback.href);
  await waiting;
  const cancellation = f.request("/api/login/cancel", {
    nonce: "a".repeat(64),
  });
  release();
  assert.equal((await completion).status, 303);
  assert.equal((await cancellation).status, 200);
  const saved = (await f.gateway.sessions.read(f.browser.id))!.value;
  assert.equal(saved.selected, production);
  assert.equal(saved.flow, undefined);
  assert.equal((await navigateLogin(f, callback.href)).status, 400);
});
test("completed callback cannot reactivate a session after logout", async (t) => {
  const f = await fixture(t),
    callback = await startLogin(f, "carbon");
  assert.equal((await navigateLogin(f, callback.href)).status, 303);
  const selected = (await f.gateway.sessions.read(f.browser.id))!.value
    .selected!;
  assert.equal(
    (await f.request("/api/logout", { profile_id: selected }, selected)).status,
    200,
  );
  assert.equal((await navigateLogin(f, callback.href)).status, 400);
  assert.equal(
    (await f.gateway.sessions.read(f.browser.id))!.value.selected,
    production,
  );
});
