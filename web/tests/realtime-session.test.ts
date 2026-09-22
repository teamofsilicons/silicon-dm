import assert from "node:assert/strict";
import test from "node:test";
import { build } from "esbuild";
import vm from "node:vm";

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
        builder.onResolve({ filter: /^\.\/(api|storage|ting)$/ }, (args) => ({
          path: args.path,
          namespace: "test",
        }));
        builder.onLoad({ filter: /.*/, namespace: "test" }, (args) => ({
          contents:
            args.path === "./api"
              ? `export class ApiError extends Error { constructor(status,code,message){super(message);this.status=status;this.code=code;} }
        globalThis.ApiError=ApiError; export const api=(...args)=>harness.api(...args);
        export const pathSegment=encodeURIComponent;
        export const queryString=(v)=>"?"+new URLSearchParams(Object.entries(v).filter(([k,v])=>v!==null&&v!==undefined)).toString();`
              : args.path === "./ting"
                ? `export const watchTing=(origin,actor,org,environment,changed,status)=>{harness.hint=changed;harness.watch={origin,actor,org,environment,changed,status,closed:false};harness.watches.push(harness.watch);status({state:'connected',origin,message:'verified'});const watch=harness.watch;return {close(){watch.closed=true;},reconnect(){}};};`
                : `export class StorageError extends Error {constructor(code,message){super(message);this.code=code;}}
        export const authenticated=()=>{},scopeFor=()=>"scope",getDeviceId=async()=>"device";
        export const getGeneration=async()=>harness.generation;
        export const adoptGeneration=async(_s,g)=>{const changed=harness.generation!==undefined&&harness.generation!==g;harness.generation=g;return changed;};
        export const getSyncCheckpoint=async()=>harness.checkpoint;
        export const beginSyncSnapshot=async()=>{harness.log.push('begin');harness.checkpoint={cursor:null,snapshot_id:'snapshot'};harness.messages.clear();return {...harness.checkpoint};};
        export const commitSyncPage=async(_s,g,expected,next,messages,device,inaccessible=[])=>{
          if(JSON.stringify(expected)!==JSON.stringify(harness.checkpoint))throw new StorageError('sync_conflict','changed');
          if(g!==harness.generation)throw new StorageError('environment_changed','changed');
          harness.log.push('commit:'+String(next.cursor));
          harness.commits.push({messages,inaccessible,next});
          for(const item of messages)harness.messages.set(item.message.id,item.message);
          for(const id of inaccessible)harness.messages.delete(id);
          harness.checkpoint={...next};return messages.map(item=>item.message);
        };
        export const pendingReceipts=async()=>harness.receipts,completeReceipt=async(_s,g,id)=>{harness.receipts=harness.receipts.filter(r=>r.message_id!==id);};
        export const queueReceipt=async(_s,g,d,c,m,status)=>harness.receipts.push({generation:g,device_id:d,conversation_id:c,message_id:m,status});
        export const cachedMessage=async(_s,id,g)=>harness.cachedRead ? harness.cachedRead(id,g) : harness.messages.get(id);
        export const broadcastSource='test',broadcastUpdate=(update)=>harness.broadcasts.push(update);`,
        }));
      },
    },
  ],
});
const tick = () => new Promise((resolve) => setImmediate(resolve));
const message = (id = "001") => ({
  id: `room#${id}`,
  conversation_id: "room",
  sender: { type: "carbon", id: "other" },
  status: "sent",
  history: [],
  created_at: "2026-09-22T00:00:00Z",
  text: id,
});
const page = (cursor = "next", events: unknown[] = []) => ({
  cursor,
  events,
  has_more: false,
  upper_sequence: 10,
  testing_environment_id: null,
  testing_generation: null,
});
const reference = (id = "001", sequence = 1) => ({
  event_id: `event-${id}`,
  type: "message",
  sequence,
  conversation_id: "room",
  message_id: id,
});
async function fixture(
  initial?: { cursor: string },
  override?: (path: string, options: any, harness: any) => unknown,
  testingGeneration?: number,
) {
  const calls: any[] = [],
    states: string[] = [],
    errors: any[] = [],
    messages: any[] = [],
    resets: any[] = [],
    removed: string[] = [],
    snapshots: number[] = [];
  const timers = new Map<number, () => void>();
  let timer = 0;
  const harness: any = {
    checkpoint: initial,
    watches: [],
    tingStates: [],
    generation: testingGeneration ?? null,
    serverGeneration: testingGeneration ?? null,
    messages: new Map(),
    receipts: [],
    log: [],
    commits: [],
    broadcasts: [],
    next: {
      ...page(),
      testing_environment_id: testingGeneration ? "sandbox" : null,
      testing_generation: testingGeneration ?? null,
    },
    override,
  };
  const session = {
    authenticated: true,
    profile_id: "profile",
    organization_id: "tos",
    testing_environment_id: testingGeneration ? "sandbox" : null,
    actor: { id: "alice", type: "carbon" },
  };
  const events = {
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent() {},
  };
  const channels: any[] = [];
  class Channel {
    onmessage?: (event: any) => void;
    constructor() {
      channels.push(this);
    }
    close() {}
    postMessage() {}
    receive(update: any) {
      this.onmessage?.({ data: update });
    }
  }
  const context = vm.createContext({
    BroadcastChannel: Channel,
    URL,
    URLSearchParams,
    Date,
    Math,
    console,
    AbortController,
    DOMException,
    navigator: { onLine: true },
    document: { ...events, visibilityState: "visible" },
    window: events,
    setInterval: () => 1,
    clearInterval() {},
    setTimeout: (f: () => void) => {
      timers.set(++timer, f);
      return timer;
    },
    clearTimeout: (id: number) => timers.delete(id),
    harness,
  });
  harness.api = async (path: string, options: any) => {
    calls.push({ path, options });
    harness.log.push(path);
    if (harness.override) {
      const value = await harness.override(path, options, harness);
      if (value !== undefined) return value;
    }
    if (path === "/iam")
      return {
        app_id: "tos>dm",
        testing_environment_id: session.testing_environment_id,
        testing_generation: harness.serverGeneration,
      };
    if (path.startsWith("/sync?reset"))
      return {
        ...page("anchor"),
        testing_environment_id: session.testing_environment_id,
        testing_generation: harness.serverGeneration,
      };
    if (path.startsWith("/sync?")) return harness.next;
    if (path.startsWith("/conversations?"))
      return { items: [{ id: "room" }], next_cursor: null };
    if (path.startsWith("/conversations/room/messages?"))
      return { items: [message("000")], next_cursor: null };
    if (path.includes("/receipts")) return {};
    if (path.startsWith("/conversations/room/messages/"))
      return message(path.split("/").at(-1));
    if (path.startsWith("/presence/devices/")) return {};
    if (path === "/api/config")
      return { ting_browser_origin: "https://ting.example" };
    throw new Error("unexpected " + path);
  };
  vm.runInContext(bundle.outputFiles[0]!.text, context);
  const connection = context.realtime.connectRealtime(session, {
    onMessage: (m: any) => messages.push(m),
    onReceipt() {},
    onReset: (g: any) => resets.push(g),
    onSnapshot: () => snapshots.push(1),
    onInaccessible: (id: string) => removed.push(id),
    onState: (s: string) => states.push(s),
    onError: (e: any) => errors.push(e),
    onTing: (value: any) => harness.tingStates.push(value),
  });
  await tick();
  return {
    harness,
    context,
    connection,
    channels,
    calls,
    states,
    errors,
    messages,
    resets,
    removed,
    snapshots,
    timers,
  };
}

test("initial recovery anchors before the complete history snapshot, then resumes the opaque cursor", async () => {
  const f = await fixture();
  try {
    const log = f.harness.log;
    assert(
      log.indexOf("/sync?reset=true&limit=100") <
        log.indexOf("/conversations?limit=100"),
    );
    assert(
      log.indexOf("commit:anchor") >
        log.findIndex((x: string) =>
          x.startsWith("/conversations/room/messages?"),
        ),
    );
    assert(log.includes("/sync?cursor=anchor&limit=100"));
    assert.equal(f.harness.checkpoint.cursor, "next");
    assert.equal(f.harness.messages.get("room#000").text, "000");
    assert(f.states.includes("connected"));
    assert.equal(f.harness.watch.origin, "https://ting.example");
    assert.deepEqual(JSON.parse(JSON.stringify(f.harness.watch.environment)), {
      testing_environment_id: null,
      testing_generation: null,
    });
    assert(f.calls.every((c) => !c.path.includes("/ws")));
    assert(f.calls.every((c) => c.options.session.profile_id === "profile"));
  } finally {
    f.connection.close();
  }
});

test("a hydration outage leaves the entire page and cursor uncommitted for retry", async () => {
  let fail = true;
  const f = await fixture({ cursor: "saved" }, (path, _options, h) => {
    if (path.startsWith("/sync?"))
      return page("next", [reference("001"), reference("002", 2)]);
    if (path.endsWith("/002") && fail)
      throw new Error("temporary hydration outage");
  });
  try {
    assert.equal(f.harness.checkpoint.cursor, "saved");
    assert.equal(f.harness.messages.size, 0);
    assert.equal(f.timers.size, 1);
    fail = false;
    const retry = [...f.timers.values()][0]!;
    f.timers.clear();
    retry();
    await tick();
    assert.equal(f.harness.checkpoint.cursor, "next");
    assert.equal(f.harness.messages.size, 2);
  } finally {
    f.connection.close();
  }
});

test("inaccessible references purge cached content and advance while authorized references hydrate", async () => {
  let ctx: any;
  const f = await fixture({ cursor: "saved" });
  try {
    ctx = f.context;
    f.harness.messages.set("room#001", message("001"));
    f.harness.next = page("after", [reference("001"), reference("002", 2)]);
    f.harness.override = (path: string) => {
      if (path.endsWith("/001"))
        throw new ctx.ApiError(404, "not_found", "revoked");
    };
    f.harness.hint();
    await tick();
    assert.equal(f.harness.checkpoint.cursor, "after");
    assert(!f.harness.messages.has("room#001"));
    assert(f.harness.messages.has("room#002"));
    assert.deepEqual(f.removed, ["room#001"]);
  } finally {
    f.connection.close();
  }
});

test("expired sync cursor restarts boundary-first snapshot and removes stale canonical cache", async () => {
  const f = await fixture({ cursor: "saved" });
  try {
    f.harness.messages.set("stale", message("stale"));
    let expired = false;
    f.harness.override = (path: string) => {
      if (path.startsWith("/sync?") && !path.includes("reset") && !expired) {
        expired = true;
        throw new f.context.ApiError(409, "sync_reset_required", "expired");
      }
    };
    f.harness.hint();
    await tick();
    assert.equal(f.snapshots.length, 1);
    assert(!f.harness.messages.has("stale"));
    assert.equal(f.harness.checkpoint.cursor, "next");
  } finally {
    f.connection.close();
  }
});

test("revoked DM authorization stops sync and does not consume Ting hints", async () => {
  const f = await fixture({ cursor: "saved" });
  try {
    f.harness.override = (path: string) => {
      if (path.startsWith("/sync?"))
        throw new f.context.ApiError(401, "unauthorized", "revoked");
    };
    f.harness.hint();
    await tick();
    assert(f.states.includes("unauthorized"));
    const count = f.calls.length;
    f.harness.hint();
    await tick();
    assert.equal(f.calls.length, count);
    assert.equal(f.timers.size, 0);
  } finally {
    f.connection.close();
  }
});

test("closing during hydration cannot commit a late message or cursor", async () => {
  const f = await fixture({ cursor: "saved" });
  try {
    let release: (v: any) => void = () => {};
    f.harness.next = page("late", [reference()]);
    f.harness.override = (path: string) =>
      path.endsWith("/001")
        ? new Promise((resolve) => {
            release = resolve;
          })
        : undefined;
    f.harness.hint();
    await tick();
    f.connection.close();
    release(message());
    await tick();
    assert.equal(f.harness.checkpoint.cursor, "next");
    assert(!f.harness.messages.has("room#001"));
  } finally {
    f.connection.close();
  }
});

test("explicit read receipts and presence use scoped HTTP only", async () => {
  const f = await fixture({ cursor: "saved" });
  try {
    await f.connection.receipt("room", "room#001", "read");
    f.connection.presence("typing");
    await tick();
    const receipt = f.calls.find((c) => c.path.endsWith("/001/receipts"));
    assert.equal(receipt.options.body.status, "read");
    assert.equal(receipt.options.body.device_id, "device");
    assert(
      f.calls.some(
        (c) =>
          c.path === "/presence/devices/device" &&
          c.options.body.activity === "typing",
      ),
    );
    await assert.rejects(
      () => f.connection.receipt("room", "room#001", "read", "another"),
      /profile/,
    );
    assert.throws(() => f.connection.presence("typing", "another"), /profile/);
  } finally {
    f.connection.close();
  }
});

test("a rejected receipt does not stall another queued recipient receipt", async () => {
  const f = await fixture({ cursor: "saved" });
  try {
    f.harness.receipts = [
      {
        generation: null,
        device_id: "device",
        conversation_id: "room",
        message_id: "room#001",
        status: "read",
      },
      {
        generation: null,
        device_id: "device",
        conversation_id: "room",
        message_id: "room#002",
        status: "delivered",
      },
    ];
    f.harness.override = (path: string) => {
      if (path.endsWith("/001/receipts"))
        throw new f.context.ApiError(404, "not_found", "deleted");
    };
    f.harness.hint();
    await tick();
    assert.equal(f.harness.receipts.length, 0);
    assert(f.calls.some((c) => c.path.endsWith("/002/receipts")));
    assert(!f.states.includes("unauthorized"));
  } finally {
    f.connection.close();
  }
});

test("a late unauthorized response from the previous connection cannot sign out a reconnect", async () => {
  const f = await fixture({ cursor: "saved" });
  try {
    let rejectOld: (error: Error) => void = () => {};
    f.harness.next = page("after", [reference()]);
    let deferred = false;
    f.harness.override = (path: string) => {
      if (path.endsWith("/001") && !deferred) {
        deferred = true;
        return new Promise((_resolve, reject) => {
          rejectOld = reject;
        });
      }
    };
    f.harness.hint();
    await tick();
    f.connection.reconnect();
    rejectOld(new f.context.ApiError(401, "unauthorized", "old connection"));
    await tick();
    assert(!f.states.includes("unauthorized"));
    assert.equal(f.harness.checkpoint.cursor, "after");
    assert(f.harness.messages.has("room#001"));
  } finally {
    f.connection.close();
  }
});

for (const authorized of [true, false]) {
  test(`duplicate message/status refs hydrate exactly once with ${authorized ? "authorized" : "inaccessible"} result`, async () => {
    const f = await fixture({ cursor: "saved" });
    try {
      f.harness.messages.set("room#001", message());
      f.harness.next = page("deduped", [
        { ...reference(), type: "message_status" },
        { ...reference("001", 2), event_id: "second-event" },
      ]);
      let reads = 0;
      f.harness.override = (path: string) => {
        if (!path.endsWith("/001")) return;
        reads++;
        if ((reads === 1) !== authorized)
          throw new f.context.ApiError(403, "forbidden", "permission changed");
        return message();
      };
      f.harness.hint();
      await tick();
      assert.equal(reads, 1);
      const commit = f.harness.commits.at(-1);
      assert.equal(commit.messages.length, authorized ? 1 : 0);
      assert.equal(commit.inaccessible.length, authorized ? 0 : 1);
      if (authorized) assert.equal(commit.messages[0].deliver, true);
      assert.equal(f.harness.messages.has("room#001"), authorized);
      assert.equal(f.harness.checkpoint.cursor, "deduped");
      const broadcast = f.harness.broadcasts.find(
        (u: any) => u.kind === (authorized ? "message" : "inaccessible"),
      );
      assert.equal(broadcast.generation, null);
    } finally {
      f.connection.close();
    }
  });
}

test("delayed old-generation content hints cannot alter a reused current message ID", async () => {
  const f = await fixture({ cursor: "saved" }, undefined, 7);
  try {
    f.harness.serverGeneration = 8;
    f.harness.next = {
      ...page("generation-eight"),
      testing_environment_id: "sandbox",
      testing_generation: 8,
    };
    f.harness.hint();
    await tick();
    f.harness.messages.set("room#001", {
      ...message(),
      text: "current-generation",
    });
    let reads = 0;
    f.harness.cachedRead = () => {
      reads++;
      return f.harness.messages.get("room#001");
    };
    for (const kind of ["message", "inaccessible"])
      for (const generation of [7, undefined])
        f.channels[0].receive({
          scope: "scope",
          source: "other",
          kind,
          message_id: "room#001",
          generation,
        });
    await tick();
    assert.equal(reads, 0);
    assert.equal(f.removed.length, 0);
    assert.equal(f.messages.length, 0);
    f.channels[0].receive({
      scope: "scope",
      source: "other",
      kind: "message",
      message_id: "room#001",
      generation: 8,
    });
    await tick();
    assert.equal(f.messages.at(-1).text, "current-generation");
    f.channels[0].receive({
      scope: "scope",
      source: "other",
      kind: "inaccessible",
      message_id: "room#001",
      generation: 8,
    });
    await tick();
    assert.deepEqual(f.removed, ["room#001"]);
  } finally {
    f.connection.close();
  }
});

test("an old-generation hint is ignored before this tab has observed the durable generation change", async () => {
  const f = await fixture({ cursor: "saved" }, undefined, 7);
  try {
    f.harness.generation = 8;
    f.channels[0].receive({
      scope: "scope",
      source: "other",
      kind: "inaccessible",
      message_id: "room#001",
      generation: 7,
    });
    await tick();
    assert.equal(f.removed.length, 0);
  } finally {
    f.connection.close();
  }
});

test("a delayed cross-tab cache read cannot publish after reconnect", async () => {
  const f = await fixture({ cursor: "saved" });
  try {
    let release: (message: any) => void = () => {};
    f.harness.cachedRead = () =>
      new Promise((resolve) => {
        release = resolve;
      });
    f.channels[0].receive({
      scope: "scope",
      source: "other",
      kind: "message",
      message_id: "room#001",
      generation: null,
    });
    await tick();
    f.connection.reconnect();
    await tick();
    release(message());
    await tick();
    assert.equal(f.messages.length, 0);
  } finally {
    f.connection.close();
  }
});

test("DM generation changes replace Ting binding and suppress old context callbacks", async () => {
  const f = await fixture({ cursor: "saved" }, undefined, 7);
  try {
    const previous = f.harness.watch;
    assert.equal(previous.environment.testing_generation, 7);
    f.harness.serverGeneration = 8;
    f.harness.next = {
      ...page("generation-eight"),
      testing_environment_id: "sandbox",
      testing_generation: 8,
    };
    previous.changed();
    await tick();
    assert.equal(previous.closed, true);
    assert.equal(f.harness.watches.length, 2);
    assert.equal(f.harness.watch.environment.testing_generation, 8);
    const calls = f.calls.length,
      statuses = f.harness.tingStates.length;
    previous.changed();
    previous.status({
      state: "connected",
      origin: "https://ting.example",
      message: "stale",
    });
    await tick();
    assert.equal(f.calls.length, calls);
    assert.equal(f.harness.tingStates.length, statuses);
  } finally {
    f.connection.close();
  }
});
