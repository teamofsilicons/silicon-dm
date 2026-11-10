/** Per-run DM discovery adapter. Real Accounts/DM handle every request; only the
 * Ting discovery flag changes. No shared service configuration or data changes. */
import { createServer } from 'node:http';
const server = createServer(async (req, res) => {
  try {
    const headers = new Headers();
    for (const [key, value] of Object.entries(req.headers)) if (value && !['host', 'connection', 'content-length'].includes(key)) headers.set(key, String(value));
    const controller = new AbortController();
    res.on('close', () => controller.abort());
    const upstream = await fetch(`http://127.0.0.1:4521${req.url}`, {
      method: req.method, headers, signal: controller.signal,
      ...(['GET', 'HEAD'].includes(req.method) ? {} : { body: req, duplex: 'half' }),
    });
    const body = Buffer.from(await upstream.arrayBuffer());
    for (const [key, value] of upstream.headers) if (!['content-length', 'content-encoding', 'transfer-encoding', 'connection'].includes(key)) res.setHeader(key, value);
    res.statusCode = upstream.status;
    if (req.url === '/api/v1/me' && upstream.ok) {
      const value = JSON.parse(body.toString());
      value.data.delivery.ting_enabled = true;
      value.data.delivery.transport = 'ting';
      res.end(JSON.stringify(value));
    } else res.end(body);
  } catch { if (!res.destroyed) { res.statusCode = 502; res.end(); } }
});
server.listen(4747, '127.0.0.1');
process.on('SIGTERM', () => { server.close(); server.closeAllConnections(); });
