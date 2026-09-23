/** Actual browser + DM client/gateway, with independent loopback DM and Ting fixtures.
 * Run: npm run test:session-browser; open the printed URL using the browser tool.
 * This verifies browser integration, not deployed Ting authorization or lifecycle behavior.
 * All credentials are fake. Browser I/O is restricted to the two printed origins.
 */
import { createServer, type Server, type ServerResponse } from "node:http";
import { once } from "node:events";
import { mkdtemp, rm, writeFile, readdir, unlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { randomUUID } from "node:crypto";
import { build } from "esbuild";
import { WebSocketServer, WebSocket } from "ws";
import { Gateway } from "../server/gateway.ts";
import { configuration } from "../server/config.ts";
import { httpType } from "../src/wire.ts";

const directory = await mkdtemp(join(tmpdir(), "dm-ting-browser-"));
const profileId = randomUUID();
const actor = { id: "session-test-carbon", type: "carbon" as const };
const organization = "session-test";
const conversation = `${actor.id}::peer`;
let tokenVersion = 0,
  accessValid = true,
  refreshStatus = 200;
let refreshAttempts = 0,
  messageAttempts = 0,
  dmUpgrades = 0;
let tingConnections = 0,
  tingWatches = 0,
  tingMe = 0,
  tingReads = 0;
let tingEnvironment: Record<string, unknown> | undefined = {
  kind: "production",
};
let registrations = 0,
  presenceWrites = 0,
  presenceDeletes = 0,
  receipts = 0;
let origin = "",
  report: unknown;
const accepted = new Map<string, { body: string; data: object }>();
const attempts: { key: string; body: string; authorized: boolean }[] = [];
const dmRequests: { method: string; path: string; authorized: boolean }[] = [];
const tingFrames: Record<string, unknown>[] = [];
const hydration: { id: string; authorized: boolean }[] = [];
const events: object[] = [];
const messages = new Map<string, Record<string, unknown>>();
const cursors = new Map<string, { position: number; upper: number | null }>();
const syncRequests: {
  cursor: string | null;
  reset: boolean;
  authorized: boolean;
}[] = [];
const sockets = new WebSocketServer({ noServer: true });
const watchers = new Set<WebSocket>();

async function listen(server: Server) {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string") throw Error("Missing listener");
  return `http://127.0.0.1:${address.port}`;
}
function json(
  res: ServerResponse,
  status: number,
  data: unknown,
  envelope = true,
) {
  res.writeHead(status, {
    "Content-Type": "application/json",
    "Cache-Control": "no-store",
  });
  res.end(
    JSON.stringify(
      envelope
        ? {
            type:
              status >= 400
                ? "error"
                : httpType(res.req.method || "GET", res.req.url || "/"),
            data,
          }
        : data,
    ),
  );
}
async function bodyOf(req: AsyncIterable<Buffer | string>) {
  const chunks: Buffer[] = [];
  for await (const chunk of req) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks).toString();
}
function canonicalMessage(id: string, text: string) {
  return {
    "message-id": id,
    conversation_id: conversation,
    sender: { type: "carbon", id: "peer" },
    recipient_id: actor.id,
    message: text,
    attachments: [],
    metadata: {},
    created_at: "2026-09-22T00:00:00Z",
    delivered_at: null,
    read_at: null,
  };
}
function addArrival(id: string, notify: boolean) {
  messages.set(id, canonicalMessage(id, `Incoming fixture message ${id}`));
  events.push({
    event_id: randomUUID(),
    sequence: events.length + 1,
    type: "message",
    conversation_id: conversation,
    message_id: id,
  });
  if (notify)
    for (const socket of watchers)
      if (socket.readyState === WebSocket.OPEN)
        socket.send(
          JSON.stringify({ op: "inbox_changed", org_id: organization }),
        );
}
const upstream = createServer(async (req, res) => {
  const url = new URL(req.url || "/", "http://loopback.invalid");
  const raw = await bodyOf(req);
  const body = raw ? JSON.parse(raw).data : {};
  if (url.pathname === "/api/v1/auth/refresh") {
    refreshAttempts++;
    if (refreshStatus !== 200)
      return json(res, refreshStatus, {
        error: {
          code: "fixture_refresh_failure",
          message: "Controlled refresh failure",
        },
      });
    if (body.refresh_token !== `fixture-refresh-${tokenVersion}`)
      return json(res, 401, {
        error: {
          code: "stale_refresh",
          message: "Refresh token already rotated",
        },
      });
    tokenVersion++;
    accessValid = true;
    return json(res, 200, {
      access_token: `fixture-access-${tokenVersion}`,
      refresh_token: `fixture-refresh-${tokenVersion}`,
      expires_in: 3600,
      actor,
      organization_id: organization,
    });
  }
  const authorized =
    accessValid &&
    req.headers.authorization === `Bearer fixture-access-${tokenVersion}`;
  dmRequests.push({
    method: req.method || "GET",
    path: req.url || "/",
    authorized,
  });
  const sending = req.method === "POST" && url.pathname.endsWith("/messages");
  if (sending) {
    messageAttempts++;
    attempts.push({
      key: String(req.headers["idempotency-key"] || ""),
      body: raw,
      authorized,
    });
  }
  if (url.pathname === "/api/v1/sync")
    syncRequests.push({
      cursor: url.searchParams.get("cursor"),
      reset: url.searchParams.get("reset") === "true",
      authorized,
    });
  const messageId = url.pathname.match(/\/messages\/([a-z0-9]{3,13})$/)?.[1];
  if (messageId && req.method === "GET")
    hydration.push({ id: messageId, authorized });
  if (!authorized)
    return json(res, 401, {
      error: {
        code: "unauthorized",
        message: "Controlled access-token expiry",
      },
    });
  if (url.pathname === "/api/v1/auth/me")
    return json(res, 200, { ...actor, org_role: "member", capabilities: [] });
  if (url.pathname === "/api/v1/iam")
    return json(res, 200, {
      app_id: "dm",
      iam_base_url: apiOrigin,
      api_base_url: apiOrigin,
      testing_environment_id: null,
      testing_generation: null,
      delivery: {
        transport: "ting",
        app_id: "ting",
        browser_origin: tingOrigin,
        receiver_authentication: "ting_session",
        dm_websocket_supported: false,
      },
    });
  if (url.pathname === "/api/v1/sync") {
    const reset = url.searchParams.get("reset") === "true";
    const previous = url.searchParams.get("cursor");
    const saved = previous ? cursors.get(previous) : undefined;
    if (previous && !saved)
      return json(res, 409, {
        error: {
          code: "sync_reset_required",
          message: "Controlled expired synchronization cursor",
        },
      });
    const position = reset ? events.length : saved?.position || 0;
    const upper = reset ? events.length : (saved?.upper ?? events.length);
    // Deliberately page one event at a time to verify actual cursor advancement.
    const page = reset
      ? []
      : events.slice(position, Math.min(position + 1, upper));
    const next = reset ? position : Math.min(position + page.length, upper);
    const hasMore = next < upper;
    const cursor = `fixture_cursor_${randomUUID()}`;
    cursors.set(cursor, { position: next, upper: hasMore ? upper : null });
    return json(res, 200, {
      events: page,
      cursor,
      has_more: hasMore,
      upper_sequence: upper,
      testing_environment_id: null,
      testing_generation: null,
    });
  }
  if (
    url.pathname === "/api/v1/delivery/registration" &&
    req.method === "POST"
  ) {
    registrations++;
    return json(res, 200, {
      registered: true,
      app_id: "dm",
      org_id: organization,
      actor,
    });
  }
  if (url.pathname.startsWith("/api/v1/presence/devices/")) {
    if (req.method === "PUT") presenceWrites++;
    if (req.method === "DELETE") presenceDeletes++;
    return json(res, 200, {
      device_id: url.pathname.split("/").at(-1),
      actor_id: actor.id,
      activity: body.activity ?? null,
      expires_at: "2026-09-22T00:01:00Z",
    });
  }
  if (url.pathname.endsWith("/receipts") && req.method === "POST") {
    receipts++;
    return json(res, 200, { recorded: true });
  }
  if (url.pathname === "/api/v1/conversations")
    return json(res, 200, {
      items: [
        {
          id: conversation,
          org_id: organization,
          participants: [actor, { type: "carbon", id: "peer" }],
          last_message: [...messages.values()].at(-1) || null,
          created_at: "2026-09-22T00:00:00Z",
          updated_at: "2026-09-22T00:00:00Z",
        },
      ],
      next_cursor: null,
    });
  if (messageId && req.method === "GET") {
    const message = messages.get(messageId);
    return message
      ? json(res, 200, message)
      : json(res, 404, { error: { code: "not_found" } });
  }
  if (url.pathname.endsWith("/messages") && req.method === "GET")
    return json(res, 200, {
      items: [...messages.values()],
      next_cursor: null,
    });
  if (sending) {
    const key = String(req.headers["idempotency-key"] || "");
    if (!key) return json(res, 422, { error: { code: "missing_key" } });
    const previous = accepted.get(key);
    if (previous && previous.body !== raw)
      return json(res, 409, { error: { code: "idempotency_conflict" } });
    const data = previous?.data || {
      ...canonicalMessage("099", body.message),
      sender: actor,
      accepted: true,
      request_key: key,
      accepted_count: accepted.size + 1,
    };
    accepted.set(key, { body: raw, data });
    return json(res, 200, data);
  }
  return json(res, 404, {
    error: { code: "unimplemented_fixture_route", message: url.pathname },
  });
});
const apiOrigin = await listen(upstream);
upstream.on("upgrade", (_req, socket) => {
  dmUpgrades++;
  socket.end(
    "HTTP/1.1 410 Gone\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
  );
});
const ting = createServer(async (req, res) => {
  if (req.headers.origin !== origin)
    return json(res, 403, { error: { code: "origin_denied" } }, false);
  res.setHeader("Access-Control-Allow-Origin", origin);
  res.setHeader("Access-Control-Allow-Credentials", "true");
  res.setHeader("Vary", "Origin");
  if (req.method === "OPTIONS") {
    res.setHeader("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
    res.setHeader("Access-Control-Allow-Headers", "Content-Type");
    res.writeHead(204);
    return res.end();
  }
  if (req.url === "/__fixture/session" && req.method === "POST") {
    // HTTP is loopback-only. A real deployed Ting cookie additionally uses Secure.
    res.setHeader(
      "Set-Cookie",
      "ting_session=fixture-ting-session; HttpOnly; SameSite=Lax; Path=/",
    );
    return json(res, 200, { authenticated: true }, false);
  }
  if (
    !req.headers.cookie
      ?.split(/;\s*/)
      .includes("ting_session=fixture-ting-session")
  )
    return json(
      res,
      401,
      {
        error: {
          code: "session_expired",
          message: "Missing fixture Ting cookie",
        },
      },
      false,
    );
  if (req.url === "/v1/me") {
    tingMe++;
    return json(
      res,
      200,
      {
        id: actor.id,
        kind: actor.type,
        authenticated: true,
        ...(tingEnvironment === undefined
          ? {}
          : { environment: tingEnvironment }),
      },
      false,
    );
  }
  if (req.url === "/v1/orgs")
    return json(
      res,
      200,
      { items: [{ id: organization, name: "Fixture" }] },
      false,
    );
  if (req.url?.includes("read") || req.method !== "GET") tingReads++;
  return json(
    res,
    404,
    { error: { code: "unsupported_fixture_operation" } },
    false,
  );
});
const tingOrigin = await listen(ting);
ting.on("upgrade", (req, socket, head) => {
  if (
    req.headers.origin !== origin ||
    !req.headers.cookie
      ?.split(/;\s*/)
      .includes("ting_session=fixture-ting-session") ||
    req.url !== "/v1/ws?protocol=v1"
  ) {
    socket.end(
      "HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
    );
    return;
  }
  sockets.handleUpgrade(req, socket, head, (ws) => {
    tingConnections++;
    ws.send(
      JSON.stringify({
        op: "ready",
        receiver_id: randomUUID(),
        protocol: "v1",
      }),
    );
    ws.on("message", (raw) => {
      const frame = JSON.parse(raw.toString());
      tingFrames.push(frame);
      if (
        frame.op !== "watch_inbox" ||
        frame.org_id !== organization ||
        typeof frame.request_id !== "string" ||
        !frame.request_id ||
        "session_token" in frame ||
        "headers" in frame
      ) {
        if (frame.op === "ack") tingReads++;
        ws.close(1008, "unexpected-browser-operation");
        return;
      }
      tingWatches++;
      watchers.add(ws);
      ws.send(
        JSON.stringify({
          op: "watching_inbox",
          request_id: frame.request_id,
          org_id: organization,
        }),
      );
    });
    ws.on("close", () => watchers.delete(ws));
  });
});
let gateway: Gateway;
let browser: Awaited<ReturnType<Gateway["sessions"]["create"]>>;
let clientScript = "";
async function stats() {
  const current = await gateway.sessions.read(browser.id);
  return {
    refreshAttempts,
    messageAttempts,
    dmUpgrades,
    tokenVersion,
    tingConnections,
    tingWatches,
    tingMe,
    tingReads,
    tingFrames,
    registrations,
    presenceWrites,
    presenceDeletes,
    receipts,
    syncRequests,
    hydration,
    dmRequests,
    acceptedCount: accepted.size,
    attempts,
    authenticated: !current?.value.profiles[0]?.auth_required,
    report,
  };
}
const frontend = createServer(async (req, res) => {
  if (req.url === "/test") {
    res.writeHead(200, {
      "Content-Type": "text/html; charset=utf-8",
      "Set-Cookie": gateway.sessions.cookie(browser.id),
      "Cache-Control": "no-store",
    });
    return res.end(
      `<!doctype html><html><head><title>DM Ting browser integration</title><style>body{font:17px system-ui;max-width:950px;margin:40px auto;padding:24px;background:#f5f7fa;color:#182334}li{margin:12px 0}pre{white-space:pre-wrap;background:white;padding:20px;border-radius:12px}.pass{color:#067342}.fail{color:#b21d31}</style></head><body><h1>DM Ting browser integration</h1><p>Actual DM client, gateway, cookies and IndexedDB. Separate loopback Ting origin with controlled CORS. Ting 0.1.3 session context is checked against controlled fixture responses; this does not prove deployed authorization or lifecycle behavior.</p><h2 id="status">Running…</h2><ol id="checks"></ol><pre id="details"></pre><script type="module" src="/session-test.js"></script></body></html>`,
    );
  }
  if (req.url === "/session-test.js") {
    res.writeHead(200, { "Content-Type": "text/javascript" });
    return res.end(clientScript);
  }
  if (req.url?.startsWith("/__test/")) {
    if (req.method !== "GET" && req.headers.origin !== origin)
      return json(res, 403, {});
    if (req.url === "/__test/ting-wrong-environment")
      tingEnvironment = {
        kind: "testing",
        id: "00000000-0000-4000-8000-000000000011",
        generation: 7,
      };
    if (req.url === "/__test/ting-legacy-environment")
      tingEnvironment = undefined;
    if (req.url === "/__test/ting-production-environment")
      tingEnvironment = { kind: "production" };
    if (req.url === "/__test/expire-http") accessValid = false;
    if (req.url === "/__test/arrivals") {
      addArrival("001", false);
      addArrival("002", true);
    }
    if (req.url === "/__test/disconnected-arrival") {
      for (const ws of sockets.clients) ws.close(1012, "fixture-reconnect");
      addArrival("003", false);
    }
    if (req.url === "/__test/transient-ting-pause") {
      for (const ws of watchers) {
        watchers.delete(ws);
        ws.send(
          JSON.stringify({
            op: "paused",
            org_id: organization,
            webhook_ids: [],
            reason: "authorization_unavailable",
          }),
        );
      }
      addArrival("005", false);
    }
    if (req.url === "/__test/expire-cursor") {
      cursors.clear();
      addArrival("004", true);
    }
    if (req.url === "/__test/outage") {
      accessValid = false;
      refreshStatus = 503;
    }
    if (req.url === "/__test/restore") refreshStatus = 200;
    if (req.url === "/__test/revoke") {
      accessValid = false;
      refreshStatus = 401;
    }
    if (req.url === "/__test/complete") {
      report = JSON.parse(await bodyOf(req));
      await writeFile(
        join(directory, "result.json"),
        JSON.stringify({ report, evidence: await stats() }, null, 2),
      );
      console.log(
        JSON.stringify({
          result: report,
          artifact: join(directory, "result.json"),
        }),
      );
    }
    return json(res, 200, await stats());
  }
  if (await gateway.handle(req, res)) return;
  res.writeHead(404);
  res.end();
});
origin = await listen(frontend);
gateway = new Gateway(
  configuration({
    DM_WEB_ORIGIN: origin,
    DM_API_ORIGIN: apiOrigin,
    IAM_LOGIN_ORIGIN: apiOrigin,
    DM_TING_BROWSER_ORIGIN: tingOrigin,
    DM_WEB_STATE_DIR: directory,
  }),
);
await gateway.initialize();
frontend.on("upgrade", (req, socket, head) => {
  dmUpgrades++;
  gateway.upgrade(req, socket, head);
});
browser = await gateway.sessions.create();
browser.value.selected = profileId;
browser.value.profiles = [
  {
    profile_id: profileId,
    actor,
    organization_id: organization,
    access_token: "fixture-access-0",
    refresh_token: "fixture-refresh-0",
    expires_at: Date.now() + 3600000,
  },
];
await gateway.sessions.save(browser);
const built = await build({
  stdin: {
    resolveDir: join(dirname(fileURLToPath(import.meta.url)), ".."),
    loader: "ts",
    contents: `
import {api,setSession} from "./src/api.ts";
import {connectRealtime} from "./src/realtime.ts";
import {getSyncCheckpoint,cachedMessage,adoptGeneration,beginSyncSnapshot,commitSyncPage,pendingReceipts,getCursor,scopeFor,getGeneration} from "./src/storage.ts";
localStorage.setItem("silicon-dm.telemetry","off");
const profileId=${JSON.stringify(profileId)},tingOrigin=${JSON.stringify(tingOrigin)},conversation=${JSON.stringify(conversation)};
const checks=[],states=[],tingStates=[],received=[],errors=[],blocked=[];
let unauthorized=0,ready=0,connection;
const allowed=new Set([location.origin,tingOrigin]),nativeFetch=window.fetch.bind(window),NativeWebSocket=window.WebSocket;
window.fetch=(input,options)=>{const url=new URL(typeof input==='string'||input instanceof URL?input:input.url,location.href);if(!allowed.has(url.origin)){blocked.push(url.origin);throw Error('Fixture blocked external fetch: '+url.origin)}return nativeFetch(input,options)};
window.WebSocket=class extends NativeWebSocket{constructor(input,protocols){const url=new URL(input,location.href);url.protocol=url.protocol==='wss:'?'https:':'http:';if(url.origin!==tingOrigin){blocked.push(url.origin);throw Error('Fixture blocked non-Ting WebSocket')}super(input,protocols)}};
window.addEventListener("dm:unauthorized",()=>unauthorized++);
const check=(name,condition)=>{if(!condition)throw Error(name);checks.push(name);const li=document.createElement("li");li.textContent="PASS: "+name;li.className="pass";document.querySelector("#checks").append(li)};
const wait=async(fn,label)=>{const until=Date.now()+20000;while(!await fn()){if(Date.now()>until)throw Error("Timed out: "+label);await new Promise(r=>setTimeout(r,40))}};
const control=async(path)=>{const response=await fetch("/__test/"+path,{method:"POST"});return(await response.json()).data};
const stats=async()=>(await(await fetch("/__test/stats")).json()).data;
const send=key=>api("/conversations/"+encodeURIComponent(conversation)+"/messages",{method:"POST",body:{text:"Controlled local test message"},idempotencyKey:key});
const finish=async(value)=>{document.querySelector("#details").textContent=JSON.stringify(value,null,2);await fetch("/__test/complete",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(value)})};
try {
 const session=await api("/api/session",{profileId});setSession(session);
 check("Initial DM cookie session authenticated",session.authenticated);
 const probe={...session,profile_id:crypto.randomUUID(),organization_id:'storage-fixture'};
 await adoptGeneration(probe,null);
 await new Promise((resolve,reject)=>{const open=indexedDB.open('silicon-dm-browser',4);open.onerror=()=>reject(open.error);open.onsuccess=()=>{const db=open.result,tx=db.transaction('cursors','readwrite'),scope=scopeFor(probe);tx.objectStore('cursors').put({id:scope+':'+probe.actor.id,scope,sequence:987});tx.oncomplete=()=>{db.close();resolve()};tx.onabort=()=>{db.close();reject(tx.error)}}});
 check("Legacy numeric ACK cursor never becomes an HTTP sync cursor",await getCursor(probe,probe.actor.id)===987&&await getSyncCheckpoint(probe,null)===undefined);
 const base=await beginSyncSnapshot(probe,null),next={cursor:'opaque-atomic-checkpoint'};
 const first={id:'storage::peer#001',conversation_id:'storage::peer',sender:{type:'carbon',id:'peer'},sequence:1,status:'sent',text:'Atomic first message',metadata:{},history:[],created_at:'2026-09-22T00:00:00Z'};
 const second={...first,id:'storage::peer#002',sequence:2,text:'Atomic second message'};
 let atomicFailure;try{await commitSyncPage(probe,null,base,next,[{message:first,deliver:true},{message:{...second,metadata:{uncloneable:()=>{}}},deliver:true}],'storage-device')}catch(e){atomicFailure=e}
 const rolledBack=await getSyncCheckpoint(probe,null);
 check("IndexedDB rolls back page messages, receipts and cursor together",!!atomicFailure&&rolledBack?.snapshot_id===base.snapshot_id&&rolledBack.cursor===null&&!await cachedMessage(probe,first.id)&&(await pendingReceipts(probe)).length===0);
 await commitSyncPage(probe,null,base,next,[{message:first,deliver:true},{message:second,deliver:true}],'storage-device');
 check("Successful page commits messages, delivered receipts and opaque cursor together",(await getSyncCheckpoint(probe,null))?.cursor===next.cursor&&!!await cachedMessage(probe,first.id)&&!!await cachedMessage(probe,second.id)&&(await pendingReceipts(probe)).length===2);
 let conflict;try{await commitSyncPage(probe,null,base,{cursor:'stale-writer'},[],'storage-device')}catch(e){conflict=e}
 check("A stale tab cannot overwrite a committed HTTP cursor",conflict?.code==='sync_conflict'&&(await getSyncCheckpoint(probe,null))?.cursor===next.cursor);
 const sandbox={...probe,testing_environment_id:crypto.randomUUID()};await adoptGeneration(sandbox,1);const beforeClean=await beginSyncSnapshot(sandbox,1);
 await commitSyncPage(sandbox,1,beforeClean,{cursor:'old-generation'},[{message:first,deliver:true}],'storage-device');await adoptGeneration(sandbox,2);
 let fenced;try{await commitSyncPage(sandbox,1,{cursor:'old-generation'},{cursor:'stale-generation'},[{message:second,deliver:true}],'storage-device')}catch(e){fenced=e}
 check("Sandbox clean fences old pages and clears cached messages and receipts",fenced?.code==='environment_changed'&&await getSyncCheckpoint(sandbox,2)===undefined&&!await cachedMessage(sandbox,first.id)&&!await cachedMessage(sandbox,second.id)&&(await pendingReceipts(sandbox)).length===0);
 const currentBase=await beginSyncSnapshot(sandbox,2);await commitSyncPage(sandbox,2,currentBase,{cursor:'current-generation'},[{message:second,deliver:true}],'storage-device');
 let staleAdoption;try{await adoptGeneration(sandbox,1)}catch(e){staleAdoption=e}
 check("Late discovery cannot lower the generation or erase current committed data",!!staleAdoption&&await getGeneration(sandbox)===2&&(await getSyncCheckpoint(sandbox,2))?.cursor==='current-generation'&&!!await cachedMessage(sandbox,second.id)&&(await pendingReceipts(sandbox)).length===1);
 const cookie=await fetch(tingOrigin+"/__fixture/session",{method:"POST",credentials:"include"});
 check("Separate Ting origin establishes an HttpOnly cookie",cookie.ok&&!document.cookie.includes('fixture-ting-session')&&!document.cookie.includes('fixture-access'));
 const handlers={onMessage(message){received.push(message.id)},onReceipt(){},onReset(){},onReady(){ready++},onState(s){states.push(s)},onTing(s){tingStates.push(s)},onError(e){errors.push(e.message)}};
 connection=connectRealtime(session,handlers);
 await wait(async()=>ready>0&&(await stats()).tingWatches>0,"initial DM sync and direct Ting watch");
 let result=await stats();
 check("Ting uses its cookie and watch_inbox on its own origin",result.tingConnections===1&&result.tingMe>0&&result.tingFrames.every(f=>f.op==='watch_inbox'&&!f.session_token));
 check("Ting 0.1.3 production context matches DM before the watch is verified",tingStates.some(s=>s.state==='connected')&&!tingStates.some(s=>s.state==='unverified'));
 const connectionsBeforeMismatch=result.tingConnections,meBeforeMismatch=result.tingMe;
 await control("ting-wrong-environment");connection.reconnect();
 await wait(async()=>(await stats()).tingMe>meBeforeMismatch&&tingStates.at(-1)?.state==='blocked',"Ting environment mismatch rejection");
 result=await stats();check("Matching account in the wrong Ting environment cannot open a new socket",result.tingConnections===connectionsBeforeMismatch);
 const meBeforeLegacy=result.tingMe;await control("ting-legacy-environment");connection.reconnect();
 await wait(async()=>(await stats()).tingMe>meBeforeLegacy&&tingStates.at(-1)?.state==='unverified',"legacy Ting context state");
 check("An older Ting response never implies verified production",tingStates.at(-1)?.state==='unverified');
 const meBeforeVerified=(await stats()).tingMe;await control("ting-production-environment");connection.reconnect();
 await wait(async()=>(await stats()).tingMe>meBeforeVerified&&tingStates.at(-1)?.state==='connected',"matching Ting context recovery");
 check("Explicit reconnect revalidates Ting context before reporting connected",true);
 const retired=await fetch('/api/ws');check("DM WebSocket endpoint is retired",retired.status===410&&result.dmUpgrades===0);
 check("Starting a watch never silently registers delivery",result.registrations===0);
 await control("arrivals");
 await wait(()=>received.includes(conversation+'#001')&&received.includes(conversation+'#002'),"Ting hint to paged DM sync and hydration");
 const checkpoint=await getSyncCheckpoint(session,null);result=await stats();
 check("Ting hint hydrates both messages through authorized DM HTTP",result.hydration.filter(h=>h.authorized).some(h=>h.id==='001')&&result.hydration.filter(h=>h.authorized).some(h=>h.id==='002'));
 check("Opaque sync checkpoint persists after multiple pages",typeof checkpoint?.cursor==='string'&&checkpoint.cursor.startsWith('fixture_cursor_')&&result.syncRequests.filter(r=>r.cursor).length>=3);
 check("Hydrated content is committed to IndexedDB",(await cachedMessage(session,conversation+'#002'))?.text==='Incoming fixture message 002');
 const beforeReconnect=result.tingWatches;
 await control("disconnected-arrival");
 await wait(async()=>received.includes(conversation+'#003')&&(await stats()).tingWatches>beforeReconnect,"reconnect catches a missed hint");
 result=await stats();
 check("Ting reconnect catches up using the persisted opaque cursor",result.syncRequests.some(r=>r.cursor===checkpoint.cursor)&&received.includes(conversation+'#003'));
 const beforeReset=result.dmRequests.length;
 await control("expire-cursor");
 await wait(()=>received.includes(conversation+'#004'),"expired cursor snapshot recovery");
 result=await stats();const resetCalls=result.dmRequests.slice(beforeReset),anchor=resetCalls.findIndex(r=>r.path.includes('/sync?')&&r.path.includes('reset=true')),snapshot=resetCalls.findIndex(r=>r.path.split('?')[0]==='/api/v1/conversations');
 check("Expired cursor anchors sync before canonical snapshot recovery",anchor>=0&&snapshot>anchor);
 const watchesBeforePause=result.tingWatches;await control("transient-ting-pause");
 await wait(async()=>received.includes(conversation+'#005')&&(await stats()).tingWatches>watchesBeforePause,"transient Ting authorization pause recovery");
 check("Transient Ting pause automatically resumes its watch and catches missed arrivals",true);
 connection.presence('typing');await wait(async()=>(await stats()).presenceWrites>0,"HTTP presence");
 check("Presence uses the DM HTTP lease",true);
 result=await stats();check("Hints and hydration never ACK or mark Ting read",result.tingReads===0&&result.tingFrames.every(f=>f.op==='watch_inbox'));
 const deletesBefore=result.presenceDeletes;
 connection.close();connection=undefined;
 await wait(async()=>(await stats()).presenceDeletes>deletesBefore,"presence close finishes before isolated send expiry");
 result=await stats();const refreshBefore=result.refreshAttempts;await control("expire-http");const sent=await send("browser-http-expiry");result=await stats();
 check("Expired DM HTTP token refreshes without logging out",sent.accepted&&result.refreshAttempts===refreshBefore+1&&unauthorized===0);
 const retried=result.attempts.filter(a=>a.key==='browser-http-expiry');
 check("Rejected send retries exactly once with identical body and key",retried.length===2&&!retried[0].authorized&&retried[1].authorized&&retried[0].body===retried[1].body&&result.acceptedCount===1);
 await control("outage");let outage;try{await send("browser-outage-retry")}catch(e){outage=e}result=await stats();
 check("Refresh outage retains the session without accepting the send",outage?.status===503&&unauthorized===0&&result.authenticated&&result.acceptedCount===1);
 await control("restore");const recovered=await send("browser-outage-retry");result=await stats();
 check("HTTP session recovers after outage without sign-in",recovered.accepted&&result.acceptedCount===2&&unauthorized===0);
 await control("revoke");let revoked;try{await send("browser-revoked-send")}catch(e){revoked=e}await wait(()=>unauthorized>0,"revoked DM session");result=await stats();
 check("Revoked refresh requires sign-in and never accepts the action",revoked?.status===401&&!result.authenticated&&result.acceptedCount===2);
 check("No DM WebSocket upgrades or external endpoint calls occurred",result.dmUpgrades===0&&blocked.length===0);
 document.querySelector("#status").textContent="PASS — all "+checks.length+" local browser checks";document.querySelector("#status").className="pass";
 await finish({passed:true,scope:'local browser fixture only',checks,ready,states,tingStates,received,errors,unauthorized,refreshAttempts:result.refreshAttempts,acceptedMessages:result.acceptedCount,liveVerification:'Deployed Ting authorization and testing lifecycle are verified separately.'});
} catch(error) {
 document.querySelector("#status").textContent="FAIL: "+error.message;document.querySelector("#status").className="fail";
 await finish({passed:false,checks,states,tingStates,received,errors,unauthorized,error:error.message});
} finally{connection?.close()}
`,
  },
  bundle: true,
  write: false,
  format: "esm",
  define: { "import.meta.env.VITE_DM_GATEWAY_ORIGIN": JSON.stringify(origin) },
});
clientScript = built.outputFiles[0]!.text;
console.log(
  JSON.stringify({
    url: `${origin}/test`,
    tingOrigin,
    stateDirectory: directory,
    report: join(directory, "result.json"),
    scope: "Local browser integration, not live Ting verification",
  }),
);
let closing = false;
async function close() {
  if (closing) return;
  closing = true;
  await gateway.close();
  for (const ws of sockets.clients) ws.terminate();
  sockets.close();
  for (const server of [frontend, upstream, ting]) server.closeAllConnections();
  await Promise.all(
    [frontend, upstream, ting].map(
      (server) => new Promise<void>((r) => server.close(() => r())),
    ),
  );
  for (const name of await readdir(directory))
    if (name !== "result.json") await unlink(join(directory, name));
  if (!report) await rm(directory, { recursive: true, force: true });
}
process.once("SIGINT", () => void close());
process.once("SIGTERM", () => void close());
