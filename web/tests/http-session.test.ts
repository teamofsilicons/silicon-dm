import assert from "node:assert/strict";
import test from "node:test";
import { build } from "esbuild";
import vm from "node:vm";

// Exercise the real HTTP and wire adapters, replacing browser/storage I/O only.
const bundle = await build({
  entryPoints: [new URL("../src/api.ts", import.meta.url).pathname],
  bundle: true,
  write: false,
  format: "iife",
  globalName: "http",
  define: {
    "import.meta.env.VITE_DM_GATEWAY_ORIGIN": '"https://dm.example"',
  },
  plugins: [
    {
      name: "browser-io",
      setup(builder) {
        builder.onResolve({ filter: /^\.\/(storage|telemetry)$/ }, (args) => ({
          path: args.path,
          namespace: "test",
        }));
        builder.onLoad({ filter: /.*/, namespace: "test" }, (args) => ({
          contents:
            args.path === "./telemetry"
              ? `export const telemetryEnabled=()=>false, recordTelemetry=()=>{};`
              : `export class StorageError extends Error {}
        export const getGeneration=(...args)=>globalThis.harness.getGeneration(...args);
        export const authenticated=()=>{}, claimOutbox=()=>{}, completeOutbox=()=>{}, failOutbox=()=>{};
        export const getDeviceId=()=>{}, listOutbox=()=>{}, queueMessage=()=>{}, removeOutbox=()=>{};
        export const scopeFor=()=>{}, broadcastUpdate=()=>{};`,
        }));
      },
    },
  ],
});

interface Call {
  url: URL;
  options: RequestInit;
  headers: Headers;
}
type Step = Response | Error | ((call: Call) => Response | Promise<Response>);

function response(status: number, data: unknown = {}) {
  return new Response(JSON.stringify({ type: "response", data }), { status });
}

function failure(status: number) {
  return response(status, {
    error: { code: `http_${status}`, message: `Request failed (${status}).` },
  });
}

function fixture(steps: Step[]) {
  const calls: Call[] = [];
  const events: CustomEvent[] = [];
  const session = {
    authenticated: true,
    profile_id: "profile",
    actor: { id: "alice", type: "carbon" },
    profiles: [],
  };
  let generation = 23;
  let generationReads = 0;
  const context = vm.createContext({
    URL,
    URLSearchParams,
    Headers,
    Response,
    DOMException,
    CustomEvent,
    crypto,
    performance,
    console,
    window: {
      location: { origin: "https://dm.example" },
      addEventListener() {},
      dispatchEvent(event: CustomEvent) {
        events.push(event);
        return true;
      },
    },
    fetch: async (url: URL, options: RequestInit) => {
      const call = { url, options, headers: new Headers(options.headers) };
      calls.push(call);
      const step = steps.shift();
      assert(step, `Unexpected request to ${url.pathname}`);
      if (step instanceof Error) throw step;
      return typeof step === "function" ? step(call) : step;
    },
    harness: {
      getGeneration: async () => {
        generationReads += 1;
        return generation;
      },
    },
  });
  vm.runInContext(bundle.outputFiles[0]!.text, context);
  context.http.setSession(session);
  return {
    api: context.http.api,
    session,
    calls,
    events,
    setSession: context.http.setSession,
    currentSession: context.http.currentSession,
    setGeneration: (value: number) => (generation = value),
    generationReads: () => generationReads,
  };
}

test("expired access token refreshes its profile and replays the identical fenced mutation once", async () => {
  const controller = new AbortController();
  const body = {
    text: "Keep this exact message",
    reply_to_message_id: "conversation#abc",
    attachments: [{ permanent_url: "https://files.example/attachment" }],
  };
  const f = fixture([
    failure(401),
    (call) => {
      assert.equal(call.url.pathname, "/api/refresh");
      // The retry must use the original request snapshot, even if UI state changes.
      body.text = "Changed while refreshing";
      f.setSession({ ...f.session, profile_id: "other-profile" });
      f.setGeneration(99);
      return response(200, f.session);
    },
    response(200, { accepted: true }),
  ]);
  f.setSession({ ...f.session, testing_environment_id: "environment" });
  const value = await f.api("/conversations/conversation/messages", {
    method: "POST",
    body,
    idempotencyKey: "original-request-key",
    version: 7,
    headers: { "X-Request-Context": "keep-me" },
    signal: controller.signal,
  });

  assert.equal(value.accepted, true);
  assert.equal(f.calls.length, 3);
  const [original, refresh, retry] = f.calls;
  assert.equal(
    original.url.pathname,
    "/api/dm/conversations/conversation/messages",
  );
  assert.equal(refresh.options.method, "POST");
  assert.deepEqual(JSON.parse(refresh.options.body as string), {
    type: "refresh",
    data: { profile_id: "profile" },
  });
  assert.equal(refresh.headers.get("X-DM-Profile"), "profile");
  assert.equal(retry.url.href, original.url.href);
  assert.deepEqual(JSON.parse(original.options.body as string), {
    type: "message.create",
    data: {
      message: "Keep this exact message",
      attachments: ["https://files.example/attachment"],
      reply: { "message-id": "abc" },
    },
  });
  assert.equal(retry.options.body, original.options.body);
  assert.equal(retry.options.method, original.options.method);
  assert.deepEqual([...retry.headers], [...original.headers]);
  assert.equal(retry.headers.get("Idempotency-Key"), "original-request-key");
  assert.equal(retry.headers.get("If-Match"), "7");
  assert.equal(retry.headers.get("X-Testing-Environment-Generation"), "23");
  assert.equal(f.generationReads(), 1);
  for (const call of f.calls) {
    assert.equal(call.options.signal, controller.signal);
    assert.equal(call.options.credentials, "include");
    assert.equal(call.options.cache, "no-store");
    assert.equal(call.options.redirect, "error");
  }
  assert.equal(f.events.length, 0);
  assert.equal(f.currentSession().profile_id, "other-profile");
});

for (const rejectedAt of ["refresh", "retry"] as const) {
  test(`a 401 from ${rejectedAt} ends recovery and signs out only the affected profile`, async () => {
    const f = fixture([
      failure(401),
      ...(rejectedAt === "retry"
        ? [
            response(200, {
              authenticated: true,
              profile_id: "requested-profile",
            }),
          ]
        : []),
      failure(401),
    ]);
    await assert.rejects(
      f.api("/auth/me", { profileId: "requested-profile" }),
      (error: any) => error.status === 401,
    );
    assert.deepEqual(
      f.calls.map((call) => call.url.pathname),
      rejectedAt === "refresh"
        ? ["/api/dm/auth/me", "/api/refresh"]
        : ["/api/dm/auth/me", "/api/refresh", "/api/dm/auth/me"],
    );
    assert.equal(f.events.length, 1);
    assert.equal(f.events[0].type, "dm:unauthorized");
    assert.equal(f.events[0].detail.profile_id, "requested-profile");
  });
}

for (const refreshFailure of ["outage", "network"] as const) {
  test(`a refresh ${refreshFailure} preserves the session and does not replay the request`, async () => {
    const f = fixture([
      failure(401),
      refreshFailure === "outage"
        ? failure(503)
        : new TypeError("Network unavailable"),
    ]);
    await assert.rejects(
      f.api("/auth/me"),
      (error: any) => error.status === (refreshFailure === "outage" ? 503 : 0),
    );
    assert.equal(f.calls.length, 2);
    assert.equal(f.events.length, 0);
    assert.equal(f.currentSession(), f.session);
  });
}

for (const refreshedSession of [
  { authenticated: false, profile_id: "profile" },
  { authenticated: true, profile_id: "different-profile" },
]) {
  test(`refresh must restore authenticated access for the requested profile (${JSON.stringify(refreshedSession)})`, async () => {
    const f = fixture([failure(401), response(200, refreshedSession)]);
    await assert.rejects(
      f.api("/auth/me"),
      (error: any) => error.status === 401,
    );
    assert.equal(f.calls.length, 2);
    assert.equal(f.events.length, 1);
    assert.equal(f.events[0].type, "dm:unauthorized");
    assert.equal(f.events[0].detail.profile_id, "profile");
  });
}

test("canceling refresh preserves AbortError and never signs out or retries", async () => {
  const controller = new AbortController();
  const aborted = new DOMException("Request canceled", "AbortError");
  const f = fixture([
    failure(401),
    (call) => {
      assert.equal(call.options.signal, controller.signal);
      controller.abort();
      throw aborted;
    },
  ]);
  await assert.rejects(
    f.api("/auth/me", { signal: controller.signal }),
    (error) => error === aborted,
  );
  assert.equal(f.calls.length, 2);
  assert.equal(f.events.length, 0);
  assert.equal(f.currentSession(), f.session);
});

test("an explicit refresh rejection cannot recursively refresh itself", async () => {
  const f = fixture([failure(401)]);
  await assert.rejects(
    f.api("/api/refresh", {
      method: "POST",
      body: { profile_id: "profile" },
    }),
    (error: any) => error.status === 401,
  );
  assert.equal(f.calls.length, 1);
  assert.equal(f.events.length, 1);
  assert.equal(f.events[0].type, "dm:unauthorized");
});

test("a non-DM gateway 401 does not refresh or announce logout", async () => {
  const f = fixture([failure(401)]);
  await assert.rejects(
    f.api("/api/login", { method: "POST", body: {} }),
    (error: any) => error.status === 401,
  );
  assert.equal(f.calls.length, 1);
  assert.equal(f.events.length, 0);
});

test("a DM authorization failure without a profile does not attempt refresh", async () => {
  const f = fixture([failure(401)]);
  f.setSession({ authenticated: false, profiles: [] });
  await assert.rejects(f.api("/auth/me"), (error: any) => error.status === 401);
  assert.equal(f.calls.length, 1);
  assert.equal(f.events.length, 1);
  assert.equal(f.events[0].type, "dm:unauthorized");
});

test("forbidden DM requests do not trigger token refresh or logout", async () => {
  const f = fixture([failure(403)]);
  await assert.rejects(f.api("/auth/me"), (error: any) => error.status === 403);
  assert.equal(f.calls.length, 1);
  assert.equal(f.events.length, 0);
});
