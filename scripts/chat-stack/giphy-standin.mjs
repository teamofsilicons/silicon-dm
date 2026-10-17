#!/usr/bin/env node
// The Giphy stand-in of the chat stack (127.0.0.1:4543): Giphy's trending and search APIs as DM's service calls them
// (DM_GIPHY_API_BASE_URL=http://127.0.0.1:4543/v1/gifs), so the website's GIF picker and `dm gifs …` work without a
// Giphy key. It answers a fixed catalog of six popular GIFs whose media are Giphy's own public https URLs (DM only
// accepts https media), so the browser still loads the pictures from media.giphy.com.
//
//   GET /v1/gifs/trending?api_key=…&limit=25            every GIF of the catalog
//   GET /v1/gifs/search?api_key=…&q=…&limit=25          the GIFs whose title or tags contain a word of q; none for
//                                                       an unknown word (to see the empty state), e.g. q=zzz
// Giphy's response shape: {data:[{type,id,url,slug,title,rating,images:{original,fixed_width,fixed_width_small,
// original_still}}], pagination, meta:{status,msg,response_id}}. A missing api_key is 401, like Giphy.
// Test controls: GET /_health, GET|DELETE /_requests, POST /_faults {"status":429,"count":1}, GET|DELETE /_faults.
import { randomUUID } from 'node:crypto';
import { appendFileSync, mkdirSync } from 'node:fs';
import http from 'node:http';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const portIndex = process.argv.indexOf('--port');
const PORT = Number(portIndex >= 0 ? process.argv[portIndex + 1] : process.env.GIPHY_STANDIN_PORT || 4543);
const LOG = process.env.GIPHY_STANDIN_LOG || join(resolve(HERE, '..', '..'), '.dev', 'logs', 'giphy.log');

/** Popular reaction GIFs (checked to exist at media.giphy.com with these renditions on 2026-10-17). */
const CATALOG = [
  { id: '3o7abKhOpu0NwenH3O', title: 'Excited SpongeBob', tags: ['excited', 'happy', 'yes', 'ready', 'spongebob', 'scheming'], width: 480, height: 270 },
  { id: 'l0MYt5jPR6QX5pnqM', title: 'Office celebration', tags: ['celebrate', 'party', 'dance', 'yay', 'success', 'office', 'win'], width: 370, height: 208 },
  { id: '26ufdipQqU2lhNA4g', title: 'Mind blown', tags: ['mind', 'blown', 'wow', 'amazing', 'whoa', 'mindblown'], width: 200, height: 200 },
  { id: 'xT9IgG50Fb7Mi0prBC', title: 'Wave hello', tags: ['hello', 'hi', 'wave', 'bye', 'goodbye', 'hey'], width: 400, height: 200 },
  { id: '3oEjI6SIIHBdRxXI40', title: 'Loading', tags: ['loading', 'wait', 'waiting', 'thinking', 'busy', 'spinner'], width: 256, height: 256 },
  { id: 'l3q2K5jinAlChoCLS', title: 'Blinking in disbelief', tags: ['blink', 'what', 'surprised', 'confused', 'disbelief', 'huh'], width: 195, height: 229 },
];

const media = (id, file) => `https://media.giphy.com/media/${id}/${file}`;
function item(gif) {
  const scaled = (width) => ({ width: String(width), height: String(Math.round((gif.height * width) / gif.width)) });
  return {
    type: 'gif', id: gif.id, url: `https://giphy.com/gifs/${gif.id}`, slug: `${gif.title.toLowerCase().replace(/[^a-z0-9]+/g, '-')}-${gif.id}`,
    title: `${gif.title} GIF`, rating: 'g', alt_text: gif.title,
    images: {
      original: { url: media(gif.id, 'giphy.gif'), width: String(gif.width), height: String(gif.height) },
      downsized: { url: media(gif.id, 'giphy.gif'), width: String(gif.width), height: String(gif.height) },
      fixed_width: { url: media(gif.id, '200w.gif'), ...scaled(200) },
      fixed_width_small: { url: media(gif.id, '100w.gif'), ...scaled(100) },
      original_still: { url: media(gif.id, 'giphy_s.gif'), width: String(gif.width), height: String(gif.height) },
    },
  };
}

const recent = [];
let faults = [];
function record(entry) {
  const line = { at: new Date().toISOString(), ...entry };
  recent.unshift(line);
  if (recent.length > 300) recent.length = 300;
  try {
    mkdirSync(dirname(LOG), { recursive: true });
    appendFileSync(LOG, `${JSON.stringify(line)}\n`, { mode: 0o600 });
  } catch {
    // never breaks an answer
  }
}

function send(res, status, value) {
  const body = JSON.stringify(value);
  res.writeHead(status, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body), 'x-dm-stand-in': 'giphy' });
  res.end(body);
}

function answer(items, limit) {
  const data = items.slice(0, Math.max(1, Math.min(50, limit || 25))).map(item);
  return { data, pagination: { total_count: items.length, count: data.length, offset: 0 }, meta: { status: 200, msg: 'OK', response_id: randomUUID().replace(/-/g, '') } };
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  if (url.pathname === '/_health') return send(res, 200, { ok: true, service: 'giphy-stand-in', port: PORT, catalog: CATALOG.length });
  if (url.pathname === '/_requests') {
    if (req.method === 'DELETE') recent.length = 0;
    return send(res, 200, { count: recent.length, items: recent });
  }
  if (url.pathname === '/_faults') {
    if (req.method === 'DELETE') faults = [];
    if (req.method === 'POST') {
      let body = '';
      for await (const chunk of req) body += chunk;
      try {
        const fault = JSON.parse(body || '{}');
        faults.push({ status: fault.status ?? 503, remaining: Math.max(1, Number(fault.count ?? 1)) });
      } catch {
        return send(res, 400, { error: { code: 'invalid_json', message: 'Send {"status":429,"count":1}.' } });
      }
    }
    return send(res, 200, { faults });
  }
  const route = url.pathname.replace(/\/+$/, '');
  const query = (url.searchParams.get('q') ?? '').trim().toLowerCase();
  const limit = Number(url.searchParams.get('limit') ?? 25);
  const log = (status, count) => record({ method: req.method, path: route, status, ...(route.endsWith('/search') ? { query_words: query.split(/\s+/).filter(Boolean).length } : {}), ...(count === undefined ? {} : { count }) });
  if (req.method !== 'GET' || !['/v1/gifs/trending', '/v1/gifs/search'].includes(route)) {
    log(404);
    return send(res, 404, { data: [], meta: { status: 404, msg: 'Not Found', response_id: '' } });
  }
  if (!url.searchParams.get('api_key')) {
    log(401);
    return send(res, 401, { data: [], meta: { status: 401, msg: 'No API key found in request.', response_id: '' } });
  }
  const fault = faults.find((entry) => entry.remaining > 0);
  if (fault) {
    fault.remaining -= 1;
    faults = faults.filter((entry) => entry.remaining > 0);
    log(fault.status);
    return send(res, fault.status, { data: [], meta: { status: fault.status, msg: 'A fault set with POST /_faults.', response_id: '' } });
  }
  if (route.endsWith('/trending')) {
    log(200, CATALOG.length);
    return send(res, 200, answer(CATALOG, limit));
  }
  const words = query.split(/[^a-z0-9]+/).filter(Boolean);
  const found = CATALOG.filter((gif) => words.some((word) => gif.tags.some((tag) => tag.startsWith(word)) || gif.title.toLowerCase().includes(word)));
  log(200, found.length);
  return send(res, 200, answer(found, limit));
});

server.listen(PORT, '127.0.0.1', () => process.stdout.write(`giphy stand-in listening on http://127.0.0.1:${PORT}\n`));
for (const signal of ['SIGTERM', 'SIGINT']) {
  process.on(signal, () => {
    server.close();
    setTimeout(() => process.exit(0), 300).unref();
  });
}
