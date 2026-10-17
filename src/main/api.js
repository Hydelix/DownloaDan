'use strict';
const crypto = require('crypto');

/*
 * The app's request handler for the browser extension. It knows NOTHING about how a request arrived:
 * the HTTP bridge calls it today, and a native-messaging host could call it tomorrow with the same
 * operations. Each transport describes the caller in `ctx`:
 *   ctx.trusted      true when the transport itself proves who the caller is (native messaging does, via the browser)
 *   ctx.token        the pairing token the caller presented (HTTP)
 *   ctx.extensionId  the extension ID the transport verified (HTTP: taken from the Origin header)
 *   ctx.userAgent    only used to show "Chrome" or "Edge" in the permission box
 *
 * Operations: hello (open), pair.start / pair.status (open, but only for allowed extensions, and the user must approve),
 *             ping, add (need the token, or a trusted transport).
 * Every operation returns { code, body } so any transport can map it onto its own format.
 */
const PROTOCOL = 1;                       // bump when the message format changes incompatibly
const PAIR_TTL_MS = 60 * 1000;            // how long the user has to click Allow
const DENY_COOLDOWN_MS = 30 * 1000;       // after a Deny, the same extension must wait before asking again
const MAX_STARTS = 5;                     // at most this many permission boxes ...
const START_WINDOW_MS = 10 * 60 * 1000;   // ... per this window

function browserName(ua) {
  const s = String(ua || '');
  if (/Edg\//.test(s)) return 'Microsoft Edge';
  if (/OPR\//.test(s)) return 'Opera';
  if (/Chrome\//.test(s)) return 'Google Chrome';
  return 'your browser';
}

function createApi({ getToken, getConfig = () => ({}), onAdd, version, allowedIds = [], onPairRequest = () => {}, onPairEnd = () => {}, now = Date.now }) {
  let pending = null;                      // { id, extensionId, browser, createdAt, decision, ... }
  const deniedAt = new Map();              // extensionId -> time of last Deny
  let starts = [];

  const tokenOk = (given) => {
    const a = Buffer.from(String(given || '')), b = Buffer.from(String(getToken()));
    return a.length === b.length && crypto.timingSafeEqual(a, b);
  };
  const authed = (ctx) => !!(ctx && (ctx.trusted || tokenOk(ctx.token)));

  function tick() {                        // expire an unanswered request, and forget finished ones
    if (!pending) return;
    const age = now() - pending.createdAt;
    if (!pending.decision && age > PAIR_TTL_MS) { pending.decision = 'expired'; onPairEnd(pending); }
    if (pending.decision && age > PAIR_TTL_MS + 30000) pending = null;
  }

  const handlers = {
    hello: () => ({ code: 200, body: { app: 'DownloaDan', version, protocol: PROTOCOL } }),

    ping: (_b, ctx) => (authed(ctx)
      ? { code: 200, body: { ok: true, app: 'DownloaDan', version, protocol: PROTOCOL, config: getConfig() } }
      : { code: 401, body: { error: 'bad-token' } }),

    add: (b, ctx) => {
      if (!authed(ctx)) return { code: 401, body: { error: 'bad-token' } };
      const url = b && typeof b.url === 'string' ? b.url.trim() : '';
      let ok = false;
      try { const u = new URL(url); ok = (u.protocol === 'http:' || u.protocol === 'https:') && url.length <= 8192; } catch { /* invalid */ }
      if (!ok) return { code: 400, body: { error: 'bad-url' } };
      const referer = typeof b.referer === 'string' && /^https?:\/\//i.test(b.referer) ? b.referer.slice(0, 4096) : undefined;
      const cookie = typeof b.cookie === 'string' && b.cookie.length <= 16384 && /^[\x20-\x7e]+$/.test(b.cookie) ? b.cookie : undefined;
      onAdd({ url, referer, cookie });
      return { code: 200, body: { ok: true, config: getConfig() } };
    },

    'pair.start': (_b, ctx) => {
      const id = ctx && ctx.extensionId;
      if (!id || !allowedIds.includes(id)) return { code: 403, body: { error: 'unknown-extension' } };
      tick();
      const denied = deniedAt.get(id);
      if (denied && now() - denied < DENY_COOLDOWN_MS) return { code: 429, body: { error: 'denied-recently', retryInSec: Math.ceil((DENY_COOLDOWN_MS - (now() - denied)) / 1000) } };
      if (pending && !pending.decision) {
        return pending.extensionId === id
          ? { code: 200, body: { pairId: pending.id, expiresInSec: Math.max(1, Math.ceil((PAIR_TTL_MS - (now() - pending.createdAt)) / 1000)) } }   // same extension asking again: reuse the open box
          : { code: 429, body: { error: 'busy' } };
      }
      starts = starts.filter((t) => now() - t < START_WINDOW_MS);
      if (starts.length >= MAX_STARTS) return { code: 429, body: { error: 'too-many' } };
      starts.push(now());
      pending = { id: crypto.randomBytes(16).toString('hex'), extensionId: id, browser: browserName(ctx.userAgent), createdAt: now(), decision: null };
      onPairRequest(pending);
      return { code: 200, body: { pairId: pending.id, expiresInSec: PAIR_TTL_MS / 1000 } };
    },

    'pair.status': (b, ctx) => {
      tick();
      const id = ctx && ctx.extensionId;
      if (!pending || !id || pending.extensionId !== id || !b || pending.id !== b.pairId) return { code: 404, body: { status: 'unknown' } };
      if (!pending.decision) return { code: 200, body: { status: 'pending', secondsLeft: Math.max(0, Math.ceil((PAIR_TTL_MS - (now() - pending.createdAt)) / 1000)) } };
      if (pending.decision === 'allowed') { pending = null; return { code: 200, body: { status: 'allowed', token: getToken() } }; }   // the token is handed over exactly once
      return { code: 200, body: { status: pending.decision } };
    },
  };

  return {
    PROTOCOL,
    handle(op, body, ctx) { const h = handlers[op]; return h ? h(body, ctx || {}) : { code: 404, body: { error: 'not-found' } }; },
    pendingPairing: () => { tick(); return pending && !pending.decision ? pending : null; },
    // called by the permission box
    resolvePairing(pairId, allow) {
      tick();
      if (!pending || pending.id !== pairId || pending.decision) return false;
      pending.decision = allow ? 'allowed' : 'denied';
      if (!allow) deniedAt.set(pending.extensionId, now());
      return true;
    },
  };
}

module.exports = { createApi, browserName, PROTOCOL, PAIR_TTL_MS, DENY_COOLDOWN_MS };
