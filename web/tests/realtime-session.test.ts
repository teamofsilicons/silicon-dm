import assert from "node:assert/strict";
import test from "node:test";
import { build } from "esbuild";
import vm from "node:vm";

// Execute the actual browser connection lifecycle, replacing only browser I/O.
const bundle = await build({
  entryPoints: [new URL("../src/realtime.ts", import.meta.url).pathname],
  bundle: true,
  write: false,
  format: "iife",
  globalName: "realtime",
  plugins: [
    {
      name: "browser-io",
      setup(builder) {
        builder.onResolve(
          { filter: /^\.\/(api|storage|telemetry)$/ },
          (args) => ({ path: args.path, namespace: "test" }),
        );
        builder.onLoad({ filter: /.*/, namespace: "test" }, (args) => ({
          contents:
            args.path === "./api"
              ? `export class ApiError extends Error { constructor(status, code, message) { super(message); this.status=status; this.code=code; } }
        globalThis.realtimeTestApiError=ApiError;
        export const api=(...args)=>globalThis.harness.api(...args);
        export const gatewayOrigin=()=>"https://dm.example";`
              : args.path === "./telemetry"
                ? `export const telemetryEnabled=()=>false;`
                : `export class StorageError extends Error {}
       export const authenticated=()=>{}, scopeFor=()=>"profile", getDeviceId=async()=>"device", getGeneration=async()=>null;
       export const adoptGeneration=async()=>{}, pendingReceipts=async()=>[], getCursor=async()=>0;
       export const cacheMessage=async(_s,m)=>m, commitDelivery=async()=>{}, completeReceipt=async()=>{}, queueReceipt=async()=>{}, cachedMessage=async()=>null;
       export const broadcastSource="test", broadcastUpdate=()=>{};`,
        }));
      },
    },
  ],
});

async function fixture(
  recovery: "success" | "revoked" | "outage" | "signed-out",
) {
  const sockets: any[] = [],
    calls: any[] = [],
    states: string[] = [],
    timers: (() => void)[] = [];
  class Socket {
    static OPEN = 1;
    static CLOSED = 3;
    readyState = 1;
    onclose?: (event: any) => void;
    onmessage?: (event: any) => void;
    constructor(readonly url: URL) {
      sockets.push(this);
    }
    send() {}
    close(code = 1000, reason = "") {
      this.readyState = 3;
      this.onclose?.({ code, reason });
    }
  }
  const events = { addEventListener() {}, removeEventListener() {} };
  const session = {
    authenticated: true,
    profile_id: "profile",
    actor: { id: "alice", type: "carbon" },
  };
  const context = vm.createContext({
    URL,
    Date,
    Math,
    console,
    WebSocket: Socket,
    navigator: { onLine: true },
    window: events,
    document: events,
    setInterval: () => 1,
    clearInterval: () => {},
    setTimeout: (f: () => void) => {
      timers.push(f);
      return timers.length;
    },
    clearTimeout: () => {},
    harness: {
      api: async (path: string, options: any) => {
        calls.push({ path, options });
        if (recovery === "revoked" || recovery === "outage") {
          const error: any = new context.realtimeTestApiError(
            recovery === "revoked" ? 401 : 503,
            "failure",
            "session check failed",
          );
          throw error;
        }
        return {
          ...session,
          authenticated: recovery !== "signed-out",
          profiles: [{ ...session, authenticated: recovery !== "signed-out" }],
        };
      },
    },
  });
  vm.runInContext(bundle.outputFiles[0]!.text, context);
  const connection = context.realtime.connectRealtime(session, {
    onMessage() {},
    onReceipt() {},
    onReset() {},
    onState: (s: string) => states.push(s),
    onError() {},
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(sockets.length, 1);
  return { sockets, calls, states, timers, connection };
}

for (const recovery of [
  "success",
  "revoked",
  "outage",
  "signed-out",
] as const) {
  test(`authority close handles ${recovery} without mistaking token expiry for logout`, async () => {
    const f = await fixture(recovery);
    try {
      f.sockets[0].close(4001, "connection_closed");
      await new Promise((resolve) => setImmediate(resolve));
      assert.equal(f.calls[0].path, "/api/refresh");
      assert.equal(f.calls[0].options.body.profile_id, "profile");
      if (recovery === "revoked" || recovery === "signed-out") {
        assert(f.states.includes("unauthorized"));
        assert.equal(f.timers.length, 0);
      } else {
        assert(!f.states.includes("unauthorized"));
        assert.equal(f.timers.length, 1);
        f.timers[0]();
        await new Promise((resolve) => setImmediate(resolve));
        assert.equal(f.sockets.length, 2);
      }
    } finally {
      f.connection.close();
    }
  });
}
