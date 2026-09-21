import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { configuration } from "../server/config.ts";
import { Gateway } from "../server/gateway.ts";
import type { BrowserSession, Profile } from "../server/session.ts";
import { encodeRequest } from "../src/wire.ts";

const primary = "00000000-0000-4000-8000-000000000011";
const sibling = "00000000-0000-4000-8000-000000000012";
const unrelated = "00000000-0000-4000-8000-000000000013";
const actor = { id: "refresh-alice", type: "carbon" as const };

async function listen(server: Server) {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert(address && typeof address !== "string");
  return `http://127.0.0.1:${address.port}`;
}

type PublicSession = {
  accepted?: boolean;
  authenticated?: boolean;
  profile_id?: string;
  profiles?: { profile_id: string; authenticated: boolean }[];
  error?: { code: string; message: string };
};

// Exercise real gateway routing, refresh rotation and durable session storage.
// Only the upstream IAM/DM HTTP service is simulated, on loopback with fake tokens.
async function fixture(t: TestContext, expiresIn = 15000) {
  const calls: {
    path: string;
    authorization?: string;
    organization?: string;
    idempotencyKey?: string;
    rawBody: string;
    body?: { type: string; data: { refresh_token?: string } };
  }[] = [];
  const state = {
    refreshStatus: 200,
    accessValid: true,
    acceptedMessages: 0,
    rotation: 0,
    accessToken: "fake-access-0",
    refreshToken: "fake-refresh-0",
  };
  const upstream = createServer(async (req, res) => {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(chunk);
    const rawBody = Buffer.concat(chunks).toString();
    const body = rawBody ? JSON.parse(rawBody) : undefined;
    calls.push({
      path: req.url!,
      authorization: req.headers.authorization,
      organization: req.headers["x-org-id"] as string | undefined,
      idempotencyKey: req.headers["idempotency-key"] as string | undefined,
      rawBody,
      body,
    });
    let data: unknown;
    if (req.url === "/api/v1/auth/refresh") {
      res.statusCode = state.refreshStatus;
      if (state.refreshStatus !== 200) {
        data = { error: { code: "mock_refresh_rejected" } };
      } else if (body?.data.refresh_token !== state.refreshToken) {
        // Reusing a consumed rotating token must fail, exposing refresh races.
        res.statusCode = 401;
        data = { error: { code: "refresh_token_reused" } };
      } else {
        state.rotation += 1;
        state.accessToken = `fake-access-${state.rotation}`;
        state.refreshToken = `fake-refresh-${state.rotation}`;
        state.accessValid = true;
        data = {
          access_token: state.accessToken,
          refresh_token: state.refreshToken,
          expires_in: 3600,
          actor,
          organization_id: "org-a",
          organization_ids: ["org-a", "org-b"],
        };
      }
    } else if (
      req.url === "/api/v1/auth/me" ||
      req.url?.endsWith("/messages")
    ) {
      if (
        !state.accessValid ||
        ![`Bearer ${state.accessToken}`, "Bearer unrelated-access"].includes(
          req.headers.authorization || "",
        )
      ) {
        res.statusCode = 401;
        data = { error: { code: "access_token_expired" } };
      } else if (req.url.endsWith("/messages")) {
        state.acceptedMessages += 1;
        data = { accepted: true };
      } else data = { org_role: "member", capabilities: [] };
    } else {
      res.statusCode = 404;
      data = { error: { code: "unexpected_endpoint" } };
    }
    res.setHeader("Content-Type", "application/json");
    res.end(JSON.stringify({ type: "response", data }));
  });
  const apiOrigin = await listen(upstream);
  const directory = await mkdtemp(join(tmpdir(), "dm-session-persistence-"));
  let gateway: Gateway;
  const frontend = createServer((req, res) => void gateway.handle(req, res));
  const origin = await listen(frontend);
  const config = configuration({
    DM_WEB_ORIGIN: origin,
    DM_API_ORIGIN: apiOrigin,
    DM_WEB_STATE_DIR: directory,
  });
  gateway = new Gateway(config);
  t.after(async () => {
    await gateway.close();
    await Promise.all([
      new Promise<void>((resolve) => frontend.close(() => resolve())),
      new Promise<void>((resolve) => upstream.close(() => resolve())),
    ]);
    await rm(directory, { recursive: true, force: true });
  });
  await gateway.initialize();
  const browser = await gateway.sessions.create();
  browser.value.selected = primary;
  browser.value.profiles = [
    ...[primary, sibling].map(
      (profile_id, index): Profile => ({
        profile_id,
        actor,
        organization_id: index ? "org-b" : "org-a",
        access_token: state.accessToken,
        refresh_token: state.refreshToken,
        // Default both organization views to the proactive renewal window.
        expires_at: Date.now() + expiresIn,
      }),
    ),
    {
      profile_id: unrelated,
      actor: { id: "refresh-bob", type: "carbon" },
      organization_id: "org-a",
      access_token: "unrelated-access",
      refresh_token: "unrelated-refresh",
      expires_at: Date.now() + 3600000,
    },
  ];
  await gateway.sessions.save(browser);
  const sessionPath = join(directory, `${browser.id}.json`);
  const cookie = gateway.sessions.cookie(browser.id).split(";")[0]!;
  const request = async (
    path: string,
    profileId = primary,
    body?: unknown,
    extraHeaders: Record<string, string> = {},
  ) => {
    const method = body === undefined ? "GET" : "POST";
    const response = await fetch(origin + path, {
      method,
      headers: {
        Origin: origin,
        Cookie: cookie,
        "X-DM-Profile": profileId,
        "Content-Type": "application/json",
        ...extraHeaders,
      },
      body:
        body === undefined
          ? undefined
          : JSON.stringify(encodeRequest(method, path, body)),
      signal: AbortSignal.timeout(5000),
    });
    return {
      status: response.status,
      headers: response.headers,
      value: (await response.json()).data as PublicSession,
    };
  };
  return {
    calls,
    state,
    directory,
    sessionPath,
    initialProfiles: structuredClone(browser.value.profiles),
    request,
    refreshCalls: () =>
      calls.filter((call) => call.path === "/api/v1/auth/refresh"),
    saved: async (): Promise<BrowserSession> =>
      JSON.parse(await readFile(sessionPath, "utf8")),
    restart: async () => {
      await gateway.close();
      gateway = new Gateway(config);
      await gateway.initialize();
    },
  };
}

test(
  "a streamed message rejection preserves HTTP 401 and permits an identical retry after refresh",
  { timeout: 10000 },
  async (t) => {
    const f = await fixture(t, 3600000);
    const path = "/api/dm/conversations/refresh-alice::peer/messages";
    const body = { text: "Preserve this exact message 👋", metadata: {} };
    const headers = { "Idempotency-Key": "streamed-expiry-retry" };
    f.state.accessValid = false;
    const rejected = await f.request(path, primary, body, headers);
    assert.equal(rejected.status, 401);
    assert.equal(rejected.value.error?.code, "access_token_expired");
    assert.equal(f.state.acceptedMessages, 0);
    assert.equal(f.refreshCalls().length, 0);
    const expired = (await f.saved()).profiles[0]!;
    assert.equal(expired.expires_at, 0);
    assert.equal(expired.auth_required, undefined);

    const renewed = await f.request("/api/refresh", primary, {
      profile_id: primary,
    });
    assert.equal(renewed.status, 200);
    assert.equal(renewed.value.authenticated, true);
    const retried = await f.request(path, primary, body, headers);
    assert.equal(retried.status, 200);
    assert.equal(retried.value.accepted, true);
    assert.equal(f.state.acceptedMessages, 1);
    assert.equal(f.refreshCalls().length, 1);
    const messages = f.calls.filter((call) => call.path.endsWith("/messages"));
    assert.equal(messages.length, 2);
    assert.equal(messages[0]!.authorization, "Bearer fake-access-0");
    assert.equal(messages[1]!.authorization, "Bearer fake-access-1");
    assert.equal(messages[0]!.rawBody, messages[1]!.rawBody);
    assert.equal(messages[0]!.idempotencyKey, headers["Idempotency-Key"]);
    assert.equal(messages[1]!.idempotencyKey, messages[0]!.idempotencyKey);
    assert.deepEqual(messages[0]!.body, {
      type: "message.create",
      data: { message: body.text, attachments: [] },
    });
  },
);

test(
  "concurrent near-expiry requests rotate once and persist every organization sibling",
  { timeout: 10000 },
  async (t) => {
    const f = await fixture(t);
    const responses = await Promise.all(
      Array.from({ length: 8 }, (_, index) =>
        f.request("/api/dm/auth/me", index % 2 ? sibling : primary),
      ),
    );
    assert(responses.every((response) => response.status === 200));
    assert.equal(f.refreshCalls().length, 1);
    assert.equal(
      f.refreshCalls()[0]!.body?.data.refresh_token,
      "fake-refresh-0",
    );
    assert.match(f.refreshCalls()[0]!.idempotencyKey!, /^dm-web-[a-f0-9]{64}$/);
    const identityCalls = f.calls.filter(
      (call) => call.path === "/api/v1/auth/me",
    );
    assert.equal(identityCalls.length, 8);
    assert(
      identityCalls.every(
        (call) => call.authorization === "Bearer fake-access-1",
      ),
    );
    assert.deepEqual(
      new Set(identityCalls.map((call) => call.organization)),
      new Set(["org-a", "org-b"]),
    );

    const saved = await f.saved();
    assert.equal(saved.selected, primary);
    for (const profile of saved.profiles.slice(0, 2)) {
      assert.equal(profile.access_token, "fake-access-1");
      assert.equal(profile.refresh_token, "fake-refresh-1");
      assert.equal(profile.auth_required, false);
      assert(profile.expires_at > Date.now() + 3500000);
    }
    assert.deepEqual(saved.profiles[2], f.initialProfiles[2]);
  },
);

test(
  "renewed credentials and refresh idempotency survive a gateway restart",
  { timeout: 10000 },
  async (t) => {
    const f = await fixture(t);
    const first = await f.request("/api/session");
    assert.equal(first.status, 200);
    assert.equal(first.value.authenticated, true);
    const saved = await f.saved();
    assert.equal(saved.profiles[0]!.refresh_token, "fake-refresh-1");
    assert.equal((await stat(f.directory)).mode & 0o777, 0o700);
    assert.equal((await stat(f.sessionPath)).mode & 0o777, 0o600);

    // An unavailable renewal retains this token and its retry key across restart.
    f.state.refreshStatus = 503;
    const unavailable = await f.request("/api/refresh", primary, {
      profile_id: primary,
    });
    assert.equal(unavailable.status, 503);
    const retryKey = f.refreshCalls().at(-1)!.idempotencyKey;
    const pending = await f.saved();
    const started = pending.profiles[0]!.refresh_started_at;
    assert.equal(typeof started, "number");
    assert.equal(pending.profiles[1]!.refresh_started_at, started);
    assert.equal(
      pending.profiles[0]!.refresh_token,
      saved.profiles[0]!.refresh_token,
    );
    await f.restart();
    assert.deepEqual(await f.saved(), pending);
    const reused = await f.request("/api/session", sibling);
    assert.equal(reused.status, 503);
    assert.equal(f.refreshCalls().length, 3);
    assert.equal(f.refreshCalls().at(-1)!.idempotencyKey, retryKey);

    f.state.refreshStatus = 200;
    const renewed = await f.request("/api/refresh", primary, {
      profile_id: primary,
    });
    assert.equal(renewed.status, 200);
    assert.equal(renewed.value.authenticated, true);
    assert.equal(
      f.refreshCalls().at(-1)!.body?.data.refresh_token,
      "fake-refresh-1",
    );
    assert.equal(f.refreshCalls().at(-1)!.idempotencyKey, retryKey);
    const completed = await f.saved();
    assert.equal(completed.profiles[0]!.expires_at, started! + 3600000);
    assert.equal(completed.profiles[0]!.refresh_started_at, undefined);
    assert(
      (await f.saved()).profiles
        .slice(0, 2)
        .every((profile) => profile.refresh_token === "fake-refresh-2"),
    );
    for (const response of [first, reused, renewed]) {
      const serialized = JSON.stringify(response.value);
      assert(!serialized.includes("fake-access-"));
      assert(!serialized.includes("fake-refresh-"));
    }
  },
);

test(
  "temporary refresh failure retains saved authentication and a later request recovers",
  { timeout: 10000 },
  async (t) => {
    const f = await fixture(t);
    const before = await f.saved();
    f.state.refreshStatus = 503;
    for (const path of ["/api/dm/auth/me", "/api/session"]) {
      const failed = await f.request(path);
      assert.equal(failed.status, 503);
      assert.equal(failed.value.error?.code, "authentication_unavailable");
      assert.equal(failed.headers.get("set-cookie"), null);
      const pending = await f.saved();
      assert.equal(typeof pending.profiles[0]!.refresh_started_at, "number");
      for (const profile of pending.profiles) delete profile.refresh_started_at;
      assert.deepEqual(pending, before);
    }
    assert.equal(
      f.calls.filter((call) => call.path === "/api/v1/auth/me").length,
      0,
    );
    assert.equal(f.refreshCalls().length, 2);
    assert.equal(
      f.refreshCalls()[0]!.idempotencyKey,
      f.refreshCalls()[1]!.idempotencyKey,
    );
    f.state.refreshStatus = 200;
    const recovered = await f.request("/api/session");
    assert.equal(recovered.status, 200);
    assert.equal(recovered.value.authenticated, true);
    assert.equal(recovered.value.profile_id, primary);
    assert.equal(f.refreshCalls().length, 3);
    assert.equal(
      f.refreshCalls()[2]!.idempotencyKey,
      f.refreshCalls()[0]!.idempotencyKey,
    );
    assert.equal((await f.saved()).profiles[0]!.auth_required, false);
  },
);

test(
  "revoked refresh marks the affected profile unauthenticated and stops renewal loops",
  { timeout: 10000 },
  async (t) => {
    const f = await fixture(t);
    f.state.refreshStatus = 401;
    const failed = await f.request("/api/refresh", primary, {
      profile_id: primary,
    });
    assert.equal(failed.status, 401);
    assert.equal(failed.value.error?.code, "login_required");
    assert.equal((await f.saved()).profiles[0]!.auth_required, true);
    const session = await f.request("/api/session");
    assert.equal(session.status, 200);
    assert.equal(session.value.authenticated, false);
    assert.equal(session.value.profile_id, primary);
    assert.equal(
      session.value.profiles!.find((profile) => profile.profile_id === primary)!
        .authenticated,
      false,
    );
    const retry = await f.request("/api/dm/auth/me");
    assert.equal(retry.status, 401);
    assert.equal(f.refreshCalls().length, 1);
    const unaffected = await f.request("/api/session", unrelated);
    assert.equal(unaffected.status, 200);
    assert.equal(unaffected.value.authenticated, true);
    assert.deepEqual((await f.saved()).profiles[2], f.initialProfiles[2]);
  },
);
