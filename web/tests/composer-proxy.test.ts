/**
 * Briefcase files on DM's own origin (GET /api/uploads/file, lib/server/briefcase.ts, lib/upload/display.ts): only the
 * configured Briefcase's public files, by their own path; Range and validators through; uploaded bytes never run as DM
 * (nosniff, a sandbox CSP, inline only for raster images, audio, video and PDF). And the settings that switch uploads
 * on, and GET /api/uploads/config.
 */
import assert from "node:assert/strict";
import { afterEach, beforeEach, test } from "node:test";
import { NextRequest } from "next/server";
import { GET as config } from "../app/api/uploads/config/route";
import { GET, HEAD } from "../app/api/uploads/file/route";
import { fileResponseHeaders, publicFileTarget, uploadsSettings, type EnabledUploads } from "../lib/server/briefcase";
import { sealSession, type StoredSession } from "../lib/server/session";
import { displayUrl, downloadUrl, isBriefcaseFile, loadsByItself, remoteSite, showsFromLocalBytes } from "../lib/upload/display";
import { applyTestEnv, json, mockFetch } from "./env";

const ACCOUNT = { uuid: "nln", kind: "carbon" as const, id: "c:ada", display_name: "Ada", pfp_url: null, custodian: null };
const session: StoredSession = { v: 1, at: "eyJ.live.sig", rt: "sar_live", ae: Date.now() + 20 * 60_000, re: Date.now() + 86_400_000, scope: "profile", acct: ACCOUNT };
/** The sealed cookie (sealing needs the test environment in place, so it is made when a test runs). */
const cookie = () => `webkit_session=${sealSession(session)}`;
const API = "https://api.briefcase.test";
const FILE = `${API}/api/v1/public/9e9693be-62a6-414c-9cb0-78453a2337f0/apps/dm/20261017-181014-78w1mb-Harbour%20at%20dusk.png?view=inline`;

let restore: () => void = () => undefined;
beforeEach(() => applyTestEnv({ BRIEFCASE_API_URL: API, UPLOADS_MAX_BYTES: undefined }));
afterEach(() => {
  restore();
  restore = () => undefined;
  applyTestEnv({ BRIEFCASE_API_URL: undefined });
});

const enabled = (source: Record<string, string>) => {
  const settings = uploadsSettings(source);
  assert.equal(settings.enabled, true, JSON.stringify(settings));
  return settings as EnabledUploads;
};

test("settings: uploads are off without BRIEFCASE_API_URL (and say why), https only except a stand-in on this machine", () => {
  const off = uploadsSettings({});
  assert.equal(off.enabled, false);
  // Carbons read that uploads are not set up; the operator's log says which setting.
  assert.equal(off.enabled ? "" : off.reason, "Uploads are not set up on this DM website yet.");
  assert.match(off.enabled ? "" : off.problem, /BRIEFCASE_API_URL is not set/);
  assert.equal(off.maxBytes, 5 * 1024 ** 4, "no limit of the website's own: Silicon Briefcase's 5 TiB per file");
  assert.equal(off.capped, false);
  const live = enabled({ BRIEFCASE_API_URL: "https://api.briefcase.teamofsilicons.com/api/v1/" });
  assert.equal(live.apiBase, "https://api.briefcase.teamofsilicons.com", "a trailing /api/v1 is the same API");
  assert.equal(live.publicBase, live.apiBase);
  const stand = enabled({ BRIEFCASE_API_URL: "http://127.0.0.1:4541", UPLOADS_MAX_BYTES: "1000" });
  assert.equal(stand.apiBase, "http://127.0.0.1:4541");
  assert.equal(stand.publicBase, "https://127.0.0.1:4541", "DM takes https links only: a plain-http stand-in is written as https");
  assert.equal(stand.maxBytes, 1000);
  assert.equal(stand.capped, true, "UPLOADS_MAX_BYTES is the operator's own limit");
  const huge = enabled({ BRIEFCASE_API_URL: API, UPLOADS_MAX_BYTES: String(10 * 1024 ** 4) });
  assert.equal(huge.maxBytes, 5 * 1024 ** 4, "a limit above Briefcase's own changes nothing");
  assert.equal(huge.capped, false);
  assert.equal(enabled({ BRIEFCASE_API_URL: API, UPLOADS_MAX_BYTES: " " }).capped, false, "an empty setting is no setting");
  for (const bad of ["http://briefcase.example.com", "ftp://x", "not a url", "https://user:pw@x.test", "https://x.test/?a=1", "http://admin:s3cret@briefcase.example.com"]) {
    const settings = uploadsSettings({ BRIEFCASE_API_URL: bad });
    assert.equal(settings.enabled, false, bad);
    // GET /api/uploads/config hands the reason to any browser: a Carbon's words, no setting's name.
    assert.equal(settings.enabled ? "" : settings.reason, "Uploads are not working on this DM website: its setup needs fixing.");
    assert.match(settings.enabled ? "" : settings.problem, /^Uploads are off: BRIEFCASE_API_URL /);
    // The value itself (credentials and all) is in neither, not even in the log line.
    for (const words of [settings.enabled ? "" : settings.reason, settings.enabled ? "" : settings.problem]) {
      assert.equal(words.includes(bad), false, bad);
      assert.equal(/s3cret|admin|pw@/.test(words), false, bad);
    }
  }
  const lots = uploadsSettings({ BRIEFCASE_API_URL: API, UPLOADS_MAX_BYTES: "lots" });
  assert.equal(lots.enabled, false);
  assert.match(lots.enabled ? "" : lots.problem, /UPLOADS_MAX_BYTES is not a whole number/);
  assert.equal((lots.enabled ? "" : lots.problem).includes("lots"), false);
});

test("GET /api/uploads/config says whether uploads work, and why not: in a Carbon's words, the setting only in the log", async () => {
  assert.deepEqual(await config().json(), { enabled: true, max_bytes: 5497558138880, capped: false });
  applyTestEnv({ BRIEFCASE_API_URL: API, UPLOADS_MAX_BYTES: "1073741824" });
  assert.deepEqual(await config().json(), { enabled: true, max_bytes: 1073741824, capped: true });
  applyTestEnv({ BRIEFCASE_API_URL: undefined, UPLOADS_MAX_BYTES: undefined });
  const logged: string[] = [];
  const warn = console.warn;
  console.warn = (...words: unknown[]) => {
    logged.push(words.join(" "));
  };
  try {
    const off = (await config().json()) as Record<string, unknown>;
    assert.deepEqual(off, { enabled: false, reason: "Uploads are not set up on this DM website yet.", max_bytes: 5497558138880, capped: false });
    await config().json();
  } finally {
    console.warn = warn;
  }
  assert.equal(logged.length, 1, "the operator's line is logged once, not on every page");
  assert.match(logged[0]!, /BRIEFCASE_API_URL is not set/);
});

test("what the proxy may fetch: the configured Briefcase's public files only, by their own path", () => {
  const settings = enabled({ BRIEFCASE_API_URL: API });
  const target = publicFileTarget(settings, FILE);
  assert.ok(!("problem" in target));
  assert.equal(target.upstream, FILE);
  assert.equal(target.download, false);
  assert.equal(target.fileName, "Harbour at dusk.png", "downloads are named after the file, not its stored name");
  const attachment = publicFileTarget(settings, FILE.replace("view=inline", "view=attachment"));
  assert.ok(!("problem" in attachment) && attachment.download && attachment.upstream === FILE, "the bytes are always fetched inline; the proxy decides how they are shown");
  const local = enabled({ BRIEFCASE_API_URL: "http://127.0.0.1:4541" });
  const lifted = publicFileTarget(local, "https://127.0.0.1:4541/api/v1/public/u/apps/dm/a.png?view=inline");
  assert.ok(!("problem" in lifted) && lifted.upstream === "http://127.0.0.1:4541/api/v1/public/u/apps/dm/a.png?view=inline");
  for (const bad of [
    null,
    "not a url",
    "https://evil.test/api/v1/public/u/apps/dm/a.png",
    "https://api.briefcase.test.evil.test/api/v1/public/u/a.png",
    `${API}/api/v1/obo/uploads/reserve`,
    `${API}/api/v1/public/onlyowner`,
    `${API}/api/v1/public/u/a%2Fb.png`,
    `${API}/api/v1/public/u/a%5cb.png`,
    `${API}/api/v1/public/u//a.png`,
    `${API}/api/v1/public/u/a.png?view=metadata`,
    "https://user:pw@api.briefcase.test/api/v1/public/u/a.png",
    "http://api.briefcase.test/api/v1/public/u/a.png",
  ]) {
    assert.ok("problem" in publicFileTarget(settings, bad), String(bad));
  }
});

test("answer headers: inline only for raster images, audio, video and PDF; nosniff, sandbox, same-origin, never cached by others", () => {
  const headers = (type: string, extra: Record<string, string> = {}, download = false) =>
    fileResponseHeaders(new Headers({ "content-type": type, "content-length": "42", etag: '"e1"', "accept-ranges": "bytes", "set-cookie": "x=1", "cache-control": "no-store", ...extra }), { download, fileName: "Harbour at dusk.png" });
  for (const type of ["image/png", "image/jpeg", "image/gif", "image/webp", "audio/webm;codecs=opus", "audio/mpeg", "video/mp4", "application/pdf"]) {
    assert.match(headers(type).get("content-disposition")!, /^inline;/, type);
  }
  for (const type of ["image/svg+xml", "text/html", "text/html; charset=utf-8", "application/xhtml+xml", "text/plain", "application/javascript", "application/octet-stream", "text/xml"]) {
    assert.match(headers(type).get("content-disposition")!, /^attachment;/, type);
  }
  assert.match(headers("image/png", {}, true).get("content-disposition")!, /^attachment;/, "a download is always an attachment");
  // Downloads that could be loaded as a script, a stylesheet or a page from DM's origin are plain bytes (with nosniff,
  // a browser then refuses them as any of those); a picture keeps its type, so an <img> still draws an SVG.
  for (const type of ["text/html", "text/html; charset=utf-8", "application/javascript", "text/javascript", "text/css", "application/xhtml+xml", "text/xml", "text/plain"]) {
    assert.equal(headers(type).get("content-type"), "application/octet-stream", type);
  }
  assert.equal(headers("image/svg+xml").get("content-type"), "image/svg+xml");
  assert.equal(headers("application/pdf", {}, true).get("content-type"), "application/pdf", "a PDF downloaded is still a PDF");
  assert.equal(headers("audio/webm;codecs=opus").get("content-type"), "audio/webm;codecs=opus");
  const out = headers("image/png");
  assert.equal(out.get("x-content-type-options"), "nosniff");
  assert.equal(out.get("content-security-policy"), "sandbox; default-src 'none'");
  assert.equal(out.get("cross-origin-resource-policy"), "same-origin");
  assert.equal(out.get("cache-control"), "private, no-store");
  assert.equal(out.get("set-cookie"), null);
  assert.equal(out.get("content-length"), "42");
  assert.equal(out.get("etag"), '"e1"');
  assert.equal(out.get("content-disposition"), "inline; filename=\"Harbour at dusk.png\"; filename*=UTF-8''Harbour%20at%20dusk.png");
  assert.equal(headers("image/png", { "cache-control": "public, max-age=600" }).get("cache-control"), "private, max-age=600", "never a shared cache");
  assert.equal(headers("image/png", { "content-encoding": "gzip" }).get("content-length"), null, "a decoded body has another length");
  assert.equal(headers("<script>/x").get("content-type"), "application/octet-stream", "a type that is not a media type is not passed on");
  const unicode = fileResponseHeaders(new Headers({ "content-type": "application/pdf" }), { download: false, fileName: "Plan “Q4” — ünïcode.pdf" });
  assert.equal(unicode.get("content-disposition"), "inline; filename=\"Plan _Q4_ _ _n_code.pdf\"; filename*=UTF-8''Plan%20%E2%80%9CQ4%E2%80%9D%20%E2%80%94%20%C3%BCn%C3%AFcode.pdf");
});

const request = (url: string, headers: Record<string, string> = {}, method = "GET") => new NextRequest(url, { method, headers: { cookie: cookie(), ...headers } });
const proxyUrl = (file: string, extra = "") => `http://127.0.0.1:4260/api/uploads/file?url=${encodeURIComponent(file)}${extra}`;

test("the proxy streams the file with its range, and sends Briefcase nothing of the browser's", async () => {
  const mock = mockFetch(() => new Response("PNGDATA", { status: 206, headers: { "content-type": "image/png", "content-length": "7", "content-range": "bytes 0-6/100", "accept-ranges": "bytes", etag: '"e1"', "set-cookie": "tracker=1" } }));
  restore = mock.restore;
  const response = await GET(request(proxyUrl(FILE), { range: "bytes=0-6", "if-range": '"e1"', "if-none-match": '"e0"', authorization: "Bearer stolen", "x-forwarded-for": "1.2.3.4" }));
  assert.equal(response.status, 206);
  assert.equal(await response.text(), "PNGDATA");
  assert.equal(response.headers.get("content-range"), "bytes 0-6/100");
  assert.equal(response.headers.get("content-disposition")?.startsWith("inline;"), true);
  assert.equal(response.headers.get("set-cookie"), null);
  assert.equal(mock.calls.length, 1);
  assert.equal(mock.calls[0]!.url, FILE);
  const sent = new Headers(mock.calls[0]!.init.headers);
  assert.equal(sent.get("range"), "bytes=0-6");
  assert.equal(sent.get("if-range"), '"e1"');
  assert.equal(sent.get("if-none-match"), '"e0"');
  assert.equal(sent.get("accept-encoding"), "identity", "the length Briefcase sends is the length the browser gets");
  for (const name of ["cookie", "authorization", "x-forwarded-for"]) assert.equal(sent.get(name), null, name);
  assert.equal(mock.calls[0]!.init.redirect, "manual", "redirects are never followed blindly");
});

test("the proxy refuses what is not a Briefcase public file, and a browser that is not signed in", async () => {
  const mock = mockFetch(() => new Response("nope"));
  restore = mock.restore;
  const other = await GET(request(proxyUrl("https://evil.test/api/v1/public/u/a.png")));
  assert.equal(other.status, 400);
  assert.equal(((await other.json()) as { error: { code: string } }).error.code, "invalid_file_link");
  const signedOut = await GET(new NextRequest(proxyUrl(FILE)));
  assert.equal(signedOut.status, 401);
  assert.equal(mock.calls.length, 0, "nothing was fetched");
});

test("the proxy follows Briefcase's own move to another public address, and nothing else", async () => {
  const moved = `${API}/api/v1/public/c:ada-new/apps/dm/a.png?view=inline`;
  let mock = mockFetch(url => (url === FILE ? new Response(null, { status: 308, headers: { location: "/api/v1/public/c:ada-new/apps/dm/a.png?view=inline" } }) : new Response("A", { status: 200, headers: { "content-type": "image/png" } })));
  restore = mock.restore;
  const followed = await GET(request(proxyUrl(FILE)));
  assert.equal(followed.status, 200);
  assert.deepEqual(mock.calls.map(call => call.url), [FILE, moved]);
  mock.restore();
  mock = mockFetch(() => new Response(null, { status: 302, headers: { location: "https://evil.test/steal" } }));
  restore = mock.restore;
  const refused = await GET(request(proxyUrl(FILE)));
  assert.equal(refused.status, 502);
  assert.equal(((await refused.json()) as { error: { code: string } }).error.code, "file_moved");
  assert.equal(mock.calls.length, 1, "the other host is never asked");
});

test("the proxy: a file no longer shared, Briefcase down, a HEAD and a 304", async () => {
  let mock = mockFetch(() => json(404, { error: { code: "not_found", message: "x" } }));
  restore = mock.restore;
  const gone = await GET(request(proxyUrl(FILE)));
  assert.equal(gone.status, 404);
  assert.equal(((await gone.json()) as { error: { code: string } }).error.code, "file_not_shared");
  mock.restore();
  mock = mockFetch(() => Promise.reject(new TypeError("fetch failed")));
  restore = mock.restore;
  assert.equal((await GET(request(proxyUrl(FILE)))).status, 502);
  mock.restore();
  mock = mockFetch((_url, init) => new Response(init.method === "HEAD" ? null : "abc", { status: 200, headers: { "content-type": "audio/webm", "content-length": "3" } }));
  restore = mock.restore;
  const head = await HEAD(request(proxyUrl(FILE), {}, "HEAD"));
  assert.equal(head.status, 200);
  assert.equal(mock.calls[0]!.init.method, "HEAD");
  assert.equal(await head.text(), "");
  mock.restore();
  mock = mockFetch(() => new Response(null, { status: 304, headers: { etag: '"e1"' } }));
  restore = mock.restore;
  const same = await GET(request(proxyUrl(FILE), { "if-none-match": '"e1"' }));
  assert.equal(same.status, 304);
  assert.equal(same.headers.get("etag"), '"e1"');
  mock.restore();
  mock = mockFetch(() => new Response("x", { status: 200, headers: { "content-type": "image/png" } }));
  restore = mock.restore;
  const download = await GET(request(proxyUrl(FILE, "&download=1")));
  assert.match(download.headers.get("content-disposition")!, /^attachment;/, "?download=1 always downloads");
});

test("displayUrl: Briefcase public files come through this website, everything else as it is", () => {
  assert.equal(isBriefcaseFile(FILE), true);
  assert.equal(displayUrl(FILE), `/api/uploads/file?url=${encodeURIComponent(FILE)}`);
  assert.equal(downloadUrl(FILE), `/api/uploads/file?url=${encodeURIComponent(FILE)}&download=1`);
  assert.equal(isBriefcaseFile("https://briefcase.example/prefix/api/v1/public/9e9693be-62a6-414c-9cb0-78453a2337f0/a.png"), true, "a Briefcase under a path prefix");
  assert.equal(isBriefcaseFile("https://api.briefcase.test/api/v1/public/c:ada/notes/plan.pdf?view=inline"), true, "a link made in Briefcase itself names the owner by id");
  assert.equal(isBriefcaseFile("https://api.briefcase.test/api/v1/public/si%3Ascout/notes/plan.pdf"), true);
  for (const other of ["https://media.giphy.com/media/x/giphy.gif", "https://example.com/a.png", "https://example.com/api/v1/public/onlyowner", "https://example.com/api/v1/public/reports/q4.png", "https://user:pw@api.briefcase.test/api/v1/public/c:ada/a.png", "/api/uploads/file?url=x", "not a url"]) {
    assert.equal(displayUrl(other), other, other);
  }
});

test("what may load by itself as a message is shown: Briefcase files (through this website) and Giphy's GIFs; any other site would learn who reads the message", () => {
  for (const url of [FILE, "https://media.giphy.com/media/x/giphy.gif", "https://media2.giphy.com/media/x/200w.gif", "https://i.giphy.com/x.gif"]) {
    assert.equal(loadsByItself(url), true, url);
    assert.equal(remoteSite(url), null, url);
  }
  for (const [url, site] of [["https://images.example.com/cat.png", "images.example.com"], ["https://giphy.com.example.net/x.gif", "giphy.com.example.net"], ["https://media.giphy.com.evil.test/x.gif", "media.giphy.com.evil.test"], ["http://media.giphy.com/x.gif", "media.giphy.com"], ["https://user:pw@media.giphy.com/x.gif", "media.giphy.com"]] as const) {
    assert.equal(loadsByItself(url), false, url);
    assert.equal(remoteSite(url), site, url);
  }
  assert.equal(loadsByItself("not a url"), false);
  assert.equal(remoteSite("not a url"), null);
});

test("your own upload shows from this tab's bytes only for pictures, recordings and video (a blob: URL is DM's origin)", () => {
  for (const type of ["image/png", "image/jpeg", "image/webp", "audio/webm;codecs=opus", "audio/mp4", "video/mp4", "IMAGE/PNG"]) assert.equal(showsFromLocalBytes(type), true, type);
  for (const type of ["text/html", "image/svg+xml", "application/pdf", "application/javascript", "text/plain", "application/octet-stream", "", "application/xhtml+xml"]) assert.equal(showsFromLocalBytes(type), false, type);
});
