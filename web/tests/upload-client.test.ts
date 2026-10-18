/**
 * The browser's side of an upload (lib/upload/client.ts) when the website answers 202: the file's last byte went and
 * Briefcase is still storing it, so the page asks GET /api/uploads/saving until it has the link. XMLHttpRequest, fetch
 * and the window are fakes here; what is checked is what the page asks, in what order, and what it does when a save is
 * cancelled, abandoned or the page goes away, and when the website restarts (a deploy) while it has the file.
 */
import assert from "node:assert/strict";
import { afterEach, beforeEach, test } from "node:test";
import { uploadFile } from "../lib/upload/client";

type Listener = (event: { persisted?: boolean }) => void;

interface Sent {
  url: string;
  headers: Record<string, string>;
  body: unknown;
}

/** What each POST answers, in order: [status, body]. */
let posts: Array<[number, unknown]> = [];
const sent: Sent[] = [];
/** What each GET /api/uploads/saving answers, in order ([status, body, headers?]); a function may wait (an ask that waits on the server). */
let asks: Array<(signal?: AbortSignal | null) => Promise<[number, unknown, Record<string, string>?]>> = [];
const fetched: Array<{ url: string; method: string; keepalive: boolean }> = [];
const listeners = new Map<string, Set<Listener>>();

class FakeXhr {
  status = 0;
  response: unknown = null;
  responseType = "";
  upload: { onprogress: ((event: { loaded: number; total: number; lengthComputable: boolean }) => void) | null; onload: (() => void) | null } = { onprogress: null, onload: null };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  private url = "";
  private headers: Record<string, string> = {};
  open(_method: string, url: string) {
    this.url = url;
  }
  setRequestHeader(name: string, value: string) {
    this.headers[name] = value;
  }
  getResponseHeader() {
    return null;
  }
  abort() {
    queueMicrotask(() => this.onabort?.());
  }
  send(body: Blob) {
    sent.push({ url: this.url, headers: this.headers, body });
    const [status, response] = posts.shift() ?? [500, null];
    setTimeout(() => {
      this.upload.onprogress?.({ loaded: body.size, total: body.size, lengthComputable: true });
      this.upload.onload?.();
      this.status = status;
      this.response = response;
      this.onload?.();
    }, 5);
  }
}

const json = (status: number, body: unknown, headers: Record<string, string> = {}) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", ...headers } });

/** One window for the whole file, as a page has: what the module listens for once (pagehide) stays registered. */
const fakeWindow = {
  addEventListener: (type: string, listener: Listener) => {
    if (!listeners.has(type)) listeners.set(type, new Set());
    listeners.get(type)!.add(listener);
  },
  removeEventListener: (type: string, listener: Listener) => listeners.get(type)?.delete(listener),
};

beforeEach(() => {
  posts = [];
  asks = [];
  sent.length = 0;
  fetched.length = 0;
  const scope = globalThis as Record<string, unknown>;
  scope.XMLHttpRequest = FakeXhr;
  scope.window = fakeWindow;
  scope.fetch = async (input: string, init: RequestInit & { keepalive?: boolean } = {}) => {
    const method = init.method ?? "GET";
    fetched.push({ url: String(input), method, keepalive: init.keepalive === true });
    if (method === "DELETE") return json(200, { type: "upload.cancel", data: { state: "cancelled" } });
    const next = asks.shift();
    if (!next) return json(404, { error: { code: "upload_unknown", message: "unknown", hint: "send again" } });
    const [status, body, headers] = await next(init.signal);
    return json(status, body, headers);
  };
});

afterEach(() => {
  const scope = globalThis as Record<string, unknown>;
  delete scope.window;
  delete scope.XMLHttpRequest;
});

const file = () => new File([new Uint8Array(4096).fill(7)], "plan.pdf", { type: "application/pdf" });
const saved = { type: "upload", data: { url: "https://briefcase.test/api/v1/public/x/apps/dm/plan.pdf?view=inline", name: "plan.pdf", content_type: "application/pdf", size: 4096 } };
const stillSaving = (id: string) => ({ type: "upload.saving", data: { id, name: "plan.pdf", content_type: "application/pdf", size: 4096 } });
const waiting = (ms: number, answer: [number, unknown]) => (signal?: AbortSignal | null) =>
  new Promise<[number, unknown]>((resolve, reject) => {
    const timer = setTimeout(() => resolve(answer), ms);
    signal?.addEventListener("abort", () => {
      clearTimeout(timer);
      reject(new DOMException("aborted", "AbortError"));
    });
  });

test("a 202 (Briefcase still storing the file): the page asks about the save until it has the link", async () => {
  posts = [[202, stillSaving("3f2b1c4d-0000-4000-8000-000000000001")]];
  asks = [waiting(30, [202, { type: "upload.saving", data: { id: "x" } }]), waiting(30, [200, saved])];
  const progress: Array<[number, number]> = [];
  let told = 0;
  const uploaded = await uploadFile(file(), { name: "plan.pdf", onProgress: (done, total) => progress.push([done, total]), onSaving: () => (told += 1) });
  assert.deepEqual(uploaded, saved.data);
  assert.equal(told, 1, "the tray is told Briefcase is storing it (Saving… this can take a few minutes)");
  const ask = fetched.filter(call => call.method === "GET");
  assert.equal(ask.length, 2);
  assert.match(ask[0]!.url, /^\/api\/uploads\/saving\?id=3f2b1c4d-0000-4000-8000-000000000001&wait=25$/);
  assert.deepEqual(progress.at(-1), [4096, 4096], "every byte sent: the tray says Saving… meanwhile");
  assert.equal(fetched.filter(call => call.method === "DELETE").length, 0);
});

test("cancelling while the website saves it: the ask stops and the save is cancelled there (a DELETE that outlives the page)", async () => {
  posts = [[202, stillSaving("3f2b1c4d-0000-4000-8000-000000000002")]];
  asks = [waiting(10_000, [200, saved])];
  const cancel = new AbortController();
  const upload = uploadFile(file(), { name: "plan.pdf", signal: cancel.signal });
  while (!fetched.some(call => call.method === "GET")) await new Promise(resolve => setTimeout(resolve, 5));
  cancel.abort();
  await assert.rejects(upload, (error: unknown) => (error as { name?: string }).name === "AbortError");
  const deletes = fetched.filter(call => call.method === "DELETE");
  assert.equal(deletes.length, 1);
  assert.match(deletes[0]!.url, /^\/api\/uploads\/saving\?id=3f2b1c4d-0000-4000-8000-000000000002$/);
  assert.equal(deletes[0]!.keepalive, true);
});

test("a save the website gave up (this page was gone too long) is sent again from its first byte", async () => {
  posts = [[202, stillSaving("3f2b1c4d-0000-4000-8000-000000000003")], [200, saved]];
  asks = [waiting(5, [409, { error: { code: "upload_abandoned", message: "This page stopped asking…", hint: "Send it again (it starts again from its first byte)." } }])];
  let restarts = 0;
  const uploaded = await uploadFile(file(), { name: "plan.pdf", onRestart: () => (restarts += 1) });
  assert.deepEqual(uploaded, saved.data);
  assert.equal(sent.length, 2, "the file went twice");
  assert.equal(restarts, 1);
});

test("a failure while saving is the upload's own, in its words", async () => {
  posts = [[202, stillSaving("3f2b1c4d-0000-4000-8000-000000000004")]];
  asks = [waiting(5, [409, { error: { code: "file_changed", message: "The file's bytes are not the ones prepared for it.", hint: "Choose the file again and send it." } }])];
  await assert.rejects(uploadFile(file(), { name: "plan.pdf" }), (error: unknown) => {
    const failure = error as { code?: string; status?: number; hint?: string };
    return failure.code === "file_changed" && failure.status === 409 && /Choose the file again/.test(failure.hint ?? "");
  });
});

test("leaving the page while the website saves a file cancels it there; a page kept for the back button does not", async () => {
  posts = [[202, stillSaving("3f2b1c4d-0000-4000-8000-000000000005")]];
  asks = [waiting(300, [200, saved])];
  const upload = uploadFile(file(), { name: "plan.pdf" });
  while (!fetched.some(call => call.method === "GET")) await new Promise(resolve => setTimeout(resolve, 5));
  for (const listener of listeners.get("pagehide") ?? []) listener({ persisted: true });
  assert.equal(fetched.filter(call => call.method === "DELETE").length, 0);
  for (const listener of listeners.get("pagehide") ?? []) listener({ persisted: false });
  const deletes = fetched.filter(call => call.method === "DELETE");
  assert.equal(deletes.length, 1);
  assert.ok(deletes[0]!.url.endsWith("id=3f2b1c4d-0000-4000-8000-000000000005"));
  assert.equal(deletes[0]!.keepalive, true);
  await upload;
});

test("an ask answered at once is never repeated faster than once a second", async () => {
  posts = [[202, stillSaving("3f2b1c4d-0000-4000-8000-000000000006")]];
  asks = [async () => [202, { type: "upload.saving", data: { id: "x" } }], async () => [200, saved]];
  const started = Date.now();
  await uploadFile(file(), { name: "plan.pdf" });
  assert.ok(Date.now() - started >= 950, `${Date.now() - started} ms`);
});

test("a gateway's 502 while the website saves (a restart, a blink) is asked again, not taken as the upload's failure", async () => {
  posts = [[202, stillSaving("3f2b1c4d-0000-4000-8000-000000000007")]];
  asks = [async () => [502, "Bad Gateway"], waiting(5, [200, saved])];
  const uploaded = await uploadFile(file(), { name: "plan.pdf" });
  assert.deepEqual(uploaded, saved.data);
  assert.equal(fetched.filter(call => call.method === "GET").length, 2);
});

test("a website that restarts while it saves the file (a deploy) lets it go: the ask hears website_restarting, and the file goes again after the wait asked for", async () => {
  posts = [[202, stillSaving("3f2b1c4d-0000-4000-8000-000000000008")], [200, saved]];
  asks = [async () => [503, { error: { code: "website_restarting", message: "This DM website is restarting, so the file was not saved.", hint: "Send it again in a moment, from its first byte (this website's pages do it by themselves)." } }, { "retry-after": "1" }]];
  let restarts = 0;
  const started = Date.now();
  const uploaded = await uploadFile(file(), { name: "plan.pdf", onRestart: () => (restarts += 1) });
  assert.deepEqual(uploaded, saved.data);
  assert.equal(sent.length, 2, "the file went twice");
  assert.equal(restarts, 1, "the tray is told it starts again");
  assert.ok(Date.now() - started >= 950, `waited ${Date.now() - started} ms (Retry-After: 1)`);
});

test("a gateway answering for the website (it is down for a moment, or a connection was cut on the way in): the file goes again by itself, not as a failure", async () => {
  posts = [[502, null], [200, saved]];
  const started = Date.now();
  const uploaded = await uploadFile(file(), { name: "plan.pdf" });
  assert.deepEqual(uploaded, saved.data);
  assert.equal(sent.length, 2);
  assert.ok(Date.now() - started >= 1_900, `waited ${Date.now() - started} ms before sending again`);
});

test("the website's own refusal in its words is never sent again by itself", async () => {
  posts = [[503, { error: { code: "briefcase_not_allowed", message: "Silicon Briefcase does not accept files from DM yet.", hint: "Send a link instead." } }]];
  await assert.rejects(uploadFile(file(), { name: "plan.pdf" }), (error: unknown) => (error as { code?: string }).code === "briefcase_not_allowed");
  assert.equal(sent.length, 1);
});
