import assert from "node:assert/strict";
import test from "node:test";
import vm from "node:vm";
import { build } from "esbuild";
const bundle = await build({
  entryPoints: [new URL("../src/ting.ts", import.meta.url).pathname],
  bundle: true,
  write: false,
  format: "iife",
  globalName: "ting",
});
const production = { testing_environment_id: null, testing_generation: null };
const environmentId = "00000000-0000-4000-8000-000000000011";
const testing = {
  testing_environment_id: environmentId,
  testing_generation: 7,
};
const identityFor = (environment: unknown) => ({
  authenticated: true,
  id: "c:alice",
  kind: "carbon",
  environment,
});
const tick = () => new Promise((resolve) => setImmediate(resolve));
test("Ting resolves an authorized handle to the canonical organization and fences hints", async () => {
  const canonical = "00000000-0000-4000-8000-000000000099";
  const f = await fixture(undefined, false, production, {
    items: [{ id: canonical, handle: "tos" }],
  });
  try {
    const socket = f.sockets[0];
    socket.frame({ op: "ready", protocol: "v1" });
    assert.equal(socket.sent[0].org_id, canonical);
    assert.equal(f.requests[1].url.pathname, "/v1/orgs");
    assert.equal(f.requests[1].options.credentials, "include");
    socket.frame({
      op: "watching_inbox",
      org_id: "foreign",
      request_id: "request-uuid",
    });
    assert.equal(f.hints(), 0);
    socket.frame({
      op: "watching_inbox",
      org_id: canonical,
      request_id: "request-uuid",
    });
    assert.equal(f.states.at(-1).state, "connected");
    socket.frame({ op: "inbox_changed", org_id: "tos" });
    assert.equal(f.hints(), 1);
    socket.frame({ op: "inbox_changed", org_id: canonical });
    assert.equal(f.hints(), 2);
    socket.frame({
      op: "paused",
      org_id: canonical,
      reason: "permission_changed",
    });
    assert.equal(socket.readyState, 3);
  } finally {
    f.connection.close();
  }
});
for (const organizations of [
  { items: [] },
  {
    items: [
      { id: "a", handle: "tos" },
      { id: "b", handle: "tos" },
    ],
  },
  { items: [{ id: "", handle: "tos" }] },
  { items: [{ id: "other", handle: "another" }] },
]) {
  test(`Ting refuses absent or ambiguous organization mapping ${JSON.stringify(organizations)}`, async () => {
    const f = await fixture(undefined, false, production, organizations);
    try {
      assert.equal(f.sockets.length, 0);
      assert.equal(f.states.at(-1).state, "blocked");
    } finally {
      f.connection.close();
    }
  });
}
async function fixture(
  identity: any = identityFor({ kind: "production" }),
  fail = false,
  environment: any = production,
  organizations: any = { items: [{ id: "tos", handle: "tos" }] },
) {
  const sockets: any[] = [],
    states: any[] = [],
    requests: any[] = [],
    timers = new Map<number, { f: () => void; ms: number }>();
  let timer = 0,
    hints = 0;
  class Socket {
    readyState = 1;
    sent: any[] = [];
    onmessage?: (event: any) => void;
    onclose?: (event: any) => void;
    constructor(readonly url: URL) {
      sockets.push(this);
    }
    send(value: string) {
      this.sent.push(JSON.parse(value));
    }
    close(code: number, reason: string) {
      this.readyState = 3;
      this.onclose?.({ code, reason });
    }
    frame(value: any) {
      this.onmessage?.({ data: JSON.stringify(value) });
    }
  }
  const context = vm.createContext({
    URL,
    JSON,
    Math,
    AbortController,
    crypto: { randomUUID: () => "request-uuid" },
    WebSocket: Socket,
    fetch: async (url: URL, options: any) => {
      requests.push({ url, options });
      if (fail) throw new Error("CORS failed");
      return {
        ok: true,
        status: 200,
        json: async () =>
          url.pathname === "/v1/orgs"
            ? organizations
            : typeof identity === "function"
              ? identity()
              : identity,
      };
    },
    setTimeout: (f: () => void, ms: number) => {
      timers.set(++timer, { f, ms });
      return timer;
    },
    clearTimeout: (id: number) => timers.delete(id),
  });
  vm.runInContext(bundle.outputFiles[0]!.text, context);
  const connection = context.ting.watchTing(
    "https://ting.example",
    { id: "c:alice", type: "carbon" },
    "tos",
    environment,
    () => hints++,
    (s: any) => states.push(s),
  );
  await tick();
  const watch = () => {
    const socket = sockets.at(-1);
    socket.frame({ op: "ready", protocol: "v1" });
    socket.frame({
      op: "watching_inbox",
      org_id: "tos",
      request_id: "request-uuid",
    });
    return socket;
  };
  return {
    sockets,
    states,
    requests,
    timers,
    connection,
    watch,
    hints: () => hints,
  };
}
test("Ting production cookie session is verified before scoped hints without read or ACK", async () => {
  const f = await fixture();
  try {
    const socket = f.watch();
    assert.equal(f.requests[0].url.href, "https://ting.example/v1/me");
    assert.equal(f.requests[0].options.credentials, "include");
    assert.equal(socket.url.href, "wss://ting.example/v1/ws?protocol=v1");
    assert.deepEqual(JSON.parse(JSON.stringify(socket.sent)), [
      { op: "watch_inbox", request_id: "request-uuid", org_id: "tos" },
    ]);
    assert.equal(f.states.at(-1).state, "connected");
    assert.match(f.states.at(-1).message, /environment/);
    socket.frame({ op: "inbox_changed", org_id: "another" });
    assert.equal(f.hints(), 1);
    socket.frame({ op: "inbox_changed", org_id: "tos" });
    assert.equal(f.hints(), 2);
    assert.equal(socket.sent.length, 1);
  } finally {
    f.connection.close();
  }
});
test("same identifier with another member kind cannot open Ting watcher", async () => {
  const f = await fixture({
    authenticated: true,
    id: "c:alice",
    kind: "silicon",
  });
  try {
    assert.equal(f.sockets.length, 0);
    assert.equal(f.states.at(-1).state, "sign-in");
    assert.equal(f.hints(), 0);
  } finally {
    f.connection.close();
  }
});
test("browser CORS/network rejection stays visible and retries without leaking credentials", async () => {
  const f = await fixture(undefined, true);
  try {
    assert.equal(f.sockets.length, 0);
    assert.equal(f.states.at(-1).state, "blocked");
    assert.match(f.states.at(-1).message, /CORS/);
    assert.equal(f.timers.size, 1);
    assert.equal(f.requests[0].options.headers, undefined);
  } finally {
    f.connection.close();
  }
});
for (const [op, reason] of [
  ["paused", "authorization_unavailable"],
  ["error", "unavailable_authorization"],
] as const) {
  test(`${op} transient ${reason} revalidates Ting session before watching again`, async () => {
    const f = await fixture();
    try {
      const socket = f.watch();
      socket.frame({ op, org_id: "tos", reason, error: { code: reason } });
      assert.equal(socket.readyState, 3);
      assert.equal(f.timers.size, 1);
      const retry = [...f.timers.values()][0]!;
      assert.equal(retry.ms, 1000);
      f.timers.clear();
      retry.f();
      await tick();
      assert.equal(f.requests.length, 4);
      assert.equal(f.sockets.length, 2);
    } finally {
      f.connection.close();
    }
  });
}
for (const reason of [
  "session_expired",
  "permission_changed",
  "permission_denied",
  "authentication_required",
]) {
  test(`${reason} requires explicit Ting recovery`, async () => {
    const f = await fixture();
    try {
      f.watch().frame({ op: "paused", org_id: "tos", reason });
      assert.equal(f.timers.size, 0);
      const hints = f.hints();
      f.sockets[0].frame({ op: "inbox_changed", org_id: "tos" });
      assert.equal(f.hints(), hints);
      f.connection.reconnect();
      await tick();
      assert.equal(f.requests.length, 4);
    } finally {
      f.connection.close();
    }
  });
}

test("a socket that never acknowledges watch expires and retries", async () => {
  const f = await fixture();
  try {
    assert.equal(f.timers.size, 1);
    const deadline = [...f.timers.values()][0]!;
    assert.equal(deadline.ms, 10000);
    f.timers.clear();
    deadline.f();
    assert.equal(f.sockets[0].readyState, 3);
    assert.equal(f.states.at(-1).state, "blocked");
    assert.equal(f.timers.size, 1);
    const retry = [...f.timers.values()][0]!;
    f.timers.clear();
    retry.f();
    await tick();
    assert.equal(f.requests.length, 4);
    assert.equal(f.sockets.length, 2);
  } finally {
    f.connection.close();
  }
});

for (const [label, identity, expected, state] of [
  [
    "matching testing UUID and generation",
    identityFor({ kind: "testing", id: environmentId, generation: 7 }),
    testing,
    "connected",
  ],
  [
    "legacy missing environment",
    { authenticated: true, id: "c:alice", kind: "carbon" },
    production,
    "unverified",
  ],
  [
    "legacy missing test environment",
    { authenticated: true, id: "c:alice", kind: "carbon" },
    testing,
    "unverified",
  ],
  [
    "production session for testing DM",
    identityFor({ kind: "production" }),
    testing,
    "blocked",
  ],
  [
    "testing session for production DM",
    identityFor({ kind: "testing", id: environmentId, generation: 7 }),
    production,
    "blocked",
  ],
  [
    "foreign testing UUID",
    identityFor({
      kind: "testing",
      id: "00000000-0000-4000-8000-000000000022",
      generation: 7,
    }),
    testing,
    "blocked",
  ],
  [
    "stale generation",
    identityFor({ kind: "testing", id: environmentId, generation: 6 }),
    testing,
    "blocked",
  ],
  [
    "future generation",
    identityFor({ kind: "testing", id: environmentId, generation: 8 }),
    testing,
    "blocked",
  ],
  [
    "missing testing generation",
    identityFor({ kind: "testing", id: environmentId }),
    testing,
    "blocked",
  ],
  [
    "string generation",
    identityFor({ kind: "testing", id: environmentId, generation: "7" }),
    testing,
    "blocked",
  ],
  [
    "invalid testing UUID",
    identityFor({ kind: "testing", id: "sandbox", generation: 7 }),
    testing,
    "blocked",
  ],
  ["null environment", identityFor(null), production, "blocked"],
  ["empty environment", identityFor({}), production, "blocked"],
  [
    "unknown environment",
    identityFor({ kind: "staging" }),
    production,
    "blocked",
  ],
  [
    "contradictory production fields",
    identityFor({ kind: "production", id: environmentId, generation: 7 }),
    production,
    "blocked",
  ],
] as const) {
  test(`Ting session environment: ${label}`, async () => {
    const f = await fixture(identity, false, expected);
    try {
      if (state === "blocked") {
        assert.equal(f.sockets.length, 0);
        assert.equal(f.hints(), 0);
        assert.equal(f.timers.size, 0);
      } else f.watch();
      assert.equal(f.states.at(-1).state, state);
    } finally {
      f.connection.close();
    }
  });
}

test("reconnect rechecks environment and discards late frames from the previous socket", async () => {
  let identity = identityFor({
    kind: "testing",
    id: environmentId,
    generation: 7,
  });
  const f = await fixture(() => identity, false, testing);
  try {
    const old = f.watch();
    const before = f.hints();
    identity = identityFor({
      kind: "testing",
      id: environmentId,
      generation: 8,
    });
    f.connection.reconnect();
    await tick();
    assert.equal(f.requests.length, 3);
    assert.equal(f.sockets.length, 1);
    assert.equal(f.states.at(-1).state, "blocked");
    old.frame({ op: "inbox_changed", org_id: "tos" });
    old.frame({
      op: "watching_inbox",
      org_id: "tos",
      request_id: "request-uuid",
    });
    assert.equal(f.hints(), before);
    assert.equal(f.states.at(-1).state, "blocked");
  } finally {
    f.connection.close();
  }
});

test("a superseded Ting identity response cannot open a socket or emit a hint", async () => {
  let release!: (identity: unknown) => void;
  let me: any = new Promise((resolve) => {
    release = resolve;
  });
  const f = await fixture(() => me);
  try {
    assert.equal(f.sockets.length, 0);
    me = identityFor({ kind: "production" });
    f.connection.reconnect();
    await tick();
    f.watch();
    release(identityFor({ kind: "testing", id: environmentId, generation: 7 }));
    await tick();
    assert.equal(f.sockets.length, 1);
    assert.equal(f.hints(), 1);
    assert.equal(f.states.at(-1).state, "connected");
  } finally {
    f.connection.close();
  }
});
