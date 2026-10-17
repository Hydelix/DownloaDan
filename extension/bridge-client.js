'use strict';
/*
 * How the extension reaches the DownloaDan app. Every other file goes through DDBridge and nothing else,
 * so how the message travels is a detail of this file:
 *   - today:  HTTP to 127.0.0.1, scanning a small port range to find the app (httpTransport below)
 *   - later:  native messaging could be added as a second transport with the same four functions
 *             (hello, call, pairStart, pairStatus) and tried first, without changing background.js or options.js.
 * Loaded with importScripts() in the service worker and with <script> in the options page.
 */
(function (root) {
  const PORTS = Array.from({ length: 10 }, (_, i) => 18642 + i);   // keep in sync with PORT_RANGE in src/main/main.js
  const PROTOCOL_MIN = 1;                                           // oldest app protocol this extension understands
  const TOKEN_HEADER = 'X-DownloaDan-Token';

  async function send(port, path, { method = 'GET', token, body, timeoutMs = 1500 } = {}) {
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), timeoutMs);
    try {
      const headers = {};
      if (body !== undefined) headers['Content-Type'] = 'application/json';
      if (token) headers[TOKEN_HEADER] = token;
      const r = await root.fetch(`http://127.0.0.1:${port}${path}`, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), signal: ctl.signal, cache: 'no-store' });
      let json = null;
      try { json = await r.json(); } catch { /* no body */ }
      return { ok: r.ok, status: r.status, json };
    } catch { return { ok: false, status: 0, json: null }; }     // nothing listening, or it did not answer in time
    finally { clearTimeout(timer); }
  }

  const httpTransport = {
    hello: (port) => send(port, '/hello', { timeoutMs: 800 }),
    call: (port, token, op, body) => (op === 'ping' ? send(port, '/ping', { token }) : send(port, '/' + op, { method: 'POST', token, body })),
    pairStart: (port) => send(port, '/pair/start', { method: 'POST', body: {} }),
    pairStatus: (port, pairId) => send(port, '/pair/status', { method: 'POST', body: { pairId } }),
  };
  let transport = httpTransport;

  const candidates = (preferred) => [...new Set([preferred, ...PORTS].filter(Number.isInteger))];

  // Ask every port in the range whether DownloaDan is there (in parallel; a closed port answers instantly).
  async function discover(preferred) {
    const all = await Promise.all(candidates(preferred).map(async (port) => {
      const r = await transport.hello(port);
      return r.ok && r.json && r.json.app === 'DownloaDan' ? { port, hello: r.json } : null;
    }));
    return all.filter(Boolean);
  }

  // Work out where things stand: not-running | outdated | not-paired | bad-token | connected
  async function connect({ port, token } = {}) {
    const found = await discover(port);
    if (!found.length) return { state: 'not-running' };
    const usable = found.filter((f) => (f.hello.protocol || 0) >= PROTOCOL_MIN);
    if (!usable.length) return { state: 'outdated', port: found[0].port, hello: found[0].hello };
    let rejected = false;
    if (token) {
      for (const f of usable) {
        const r = await transport.call(f.port, token, 'ping');
        if (r.ok) return { state: 'connected', port: f.port, hello: f.hello, config: r.json && r.json.config };
        if (r.status === 401) rejected = true;
      }
    }
    return { state: rejected ? 'bad-token' : 'not-paired', port: usable[0].port, hello: usable[0].hello };
  }

  const flat = (r) => ({ httpStatus: r.status, ...(r.json || {}) });
  root.DDBridge = {
    PORTS, PROTOCOL_MIN, discover, connect,
    call: (port, token, op, body) => transport.call(port, token, op, body),
    pairStart: async (port) => flat(await transport.pairStart(port)),
    pairStatus: async (port, pairId) => flat(await transport.pairStatus(port, pairId)),
    _setTransport(t) { transport = t || httpTransport; },       // extension point (and for tests)
  };
  if (typeof module !== 'undefined' && module.exports) module.exports = root.DDBridge;
})(typeof self !== 'undefined' ? self : globalThis);
