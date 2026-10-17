'use strict';
const http = require('http');

/*
 * HTTP transport for the browser extension.
 * - Listens on 127.0.0.1 only, and checks the Host header (blocks DNS-rebinding).
 * - Rejects any Origin that is not a browser extension, and sends no CORS headers, so web pages cannot use it.
 * - All decisions are made by the api object; this file only turns HTTP into api.handle() calls and back.
 * - It can bind to the first free port from a list; the extension scans the same range to find the app.
 */
const EXT_ORIGIN = /^(chrome|moz)-extension:\/\/([a-z0-9]+)$/;

function createBridge({ api }) {
  let server = null;
  let port = 0;

  function handle(req, res) {
    const send = (code, obj) => {
      res.writeHead(code, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' });
      res.end(JSON.stringify(obj));
    };
    const host = String(req.headers.host || '').toLowerCase();
    if (host !== `127.0.0.1:${port}` && host !== `localhost:${port}`) return send(403, { error: 'bad-host' });
    const origin = req.headers.origin;
    const m = origin ? EXT_ORIGIN.exec(origin) : null;
    if (origin && !m) return send(403, { error: 'bad-origin' });

    const ctx = {
      transport: 'http', trusted: false,
      token: req.headers['x-downloadan-token'] || req.headers['x-fluxdm-token'],      // the old header name still works until the extension is reloaded
      extensionId: m && m[1] === 'chrome' ? m[2] : null,                                // only Chromium extensions can be paired
      userAgent: req.headers['user-agent'],
    };
    const path = (req.url || '').split('?')[0];
    const reply = (r) => send(r.code, r.body);

    if (req.method === 'GET') {
      if (path === '/hello') return reply(api.handle('hello', null, ctx));
      if (path === '/ping') return reply(api.handle('ping', null, ctx));
      return send(404, { error: 'not-found' });
    }
    const ops = { '/add': 'add', '/pair/start': 'pair.start', '/pair/status': 'pair.status' };
    if (req.method === 'POST' && ops[path]) {
      if (!/^application\/json/i.test(req.headers['content-type'] || '')) return send(415, { error: 'json-only' });
      let size = 0; const chunks = [];
      req.on('data', (c) => { size += c.length; if (size > 65536) req.destroy(); else chunks.push(c); });
      req.on('end', () => {
        let body;
        try { body = JSON.parse(Buffer.concat(chunks).toString('utf8') || '{}'); } catch { return send(400, { error: 'bad-json' }); }
        reply(api.handle(ops[path], body, ctx));
      });
      return undefined;
    }
    return send(404, { error: 'not-found' });
  }

  function listen(p) {
    return new Promise((resolve) => {
      if (server) { server.close(); server = null; }
      port = p;
      const s = http.createServer(handle);
      s.on('error', (e) => { if (server === s) server = null; resolve({ ok: false, error: e.code === 'EADDRINUSE' ? `Port ${p} is already in use` : e.message }); });
      s.listen(p, '127.0.0.1', () => { server = s; resolve({ ok: true, port: p }); });
    });
  }

  return {
    listen,
    // Try each port in turn and keep the first one that is free.
    async listenAny(ports) {
      let last = { ok: false, error: 'No ports to try' };
      for (const p of ports) { last = await listen(p); if (last.ok) return last; }
      return { ok: false, error: `${last.error} (tried ${ports[0]}-${ports[ports.length - 1]})` };
    },
    close() { if (server) { server.close(); server = null; } },
  };
}

module.exports = { createBridge };
