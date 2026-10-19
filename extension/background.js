'use strict';
const ext = typeof browser !== 'undefined' ? browser : chrome;   // Firefox: browser (promises). Chromium: chrome.
// Service worker: talks to the DownloaDan app and hands it links and browser downloads.
// provides DDBridge: finds the app and talks to it. A Chromium service worker loads it here; Firefox lists it in the manifest.
if (typeof importScripts === 'function') importScripts('bridge-client.js');

// ---- settings that live in the app (the bypass key) ---------------------------------------------
function applyConfig(c) { if (c && typeof c.bypassKey === 'string') ext.storage.local.set({ bypassKey: c.bypassKey }); }
// One request to the app. Tries the last known port first; if the app moved (that port was busy at its start), look for it again.
async function api(op, body) {
  const { token = '', port } = await ext.storage.local.get(['token', 'port']);
  if (!token) return { ok: false, error: 'not-paired' };
  let r = port ? await DDBridge.call(port, token, op, body) : { status: 0 };
  if (r.status === 0) {
    const found = (await DDBridge.discover(port))[0];
    if (!found) return { ok: false, error: 'offline' };
    if (found.port !== port) await ext.storage.local.set({ port: found.port });
    r = await DDBridge.call(found.port, token, op, body);
  }
  if (r.status === 401) return { ok: false, error: 'bad-token' };
  return r.ok ? { ok: true, json: r.json } : { ok: false, error: 'http-' + r.status };
}
async function refreshConfig() {
  const r = await api('ping');
  if (r.ok) applyConfig(r.json && r.json.config);       // if the app is closed we simply keep the last known key
}
refreshConfig();                                      // whenever the service worker starts

// Is the user holding the bypass key right now (or did they click something while holding it a moment ago)?
async function bypassActive() {
  const { bypass } = await ext.storage.session.get('bypass');
  if (!bypass) return false;
  const now = Date.now();
  return (bypass.held && now - (bypass.heldAt || 0) < 15000) || now - (bypass.clickAt || 0) < 4000;   // old "held" marks expire, so a lost key-up can't disable capture
}

async function cookieHeader(url) {
  try {
    const list = await ext.cookies.getAll({ url });
    const s = list.map((c) => `${c.name}=${c.value}`).join('; ');
    return s && s.length <= 16000 && /^[\x20-\x7e]+$/.test(s) ? s : undefined;
  } catch { return undefined; }
}

async function post({ url, referer }) {
  const cookie = await cookieHeader(url);               // lets DownloaDan download things you are logged in for
  const r = await api('add', { url, referer, cookie });
  if (r.ok) applyConfig(r.json && r.json.config);
  return { ok: r.ok, error: r.error };
}

ext.runtime.onMessage.addListener((msg, sender, reply) => {
  if (sender.id !== ext.runtime.id) return false;
  (async () => {
    if (msg.type === 'add') return post(msg);
    if (msg.type === 'bypass') {                       // the page reports the bypass key going down/up, or a click while it is held
      const cur = (await ext.storage.session.get('bypass')).bypass || {};
      const now = Date.now();
      await ext.storage.session.set({ bypass: { held: !!msg.held, heldAt: msg.held ? now : cur.heldAt || 0, clickAt: msg.click ? now : cur.clickAt || 0 } });
      return { ok: true };
    }
    if (msg.type === 'refreshConfig') {                // a tab was focused: pick up a key changed in the app (at most every 5 s)
      const { cfgAt = 0 } = await ext.storage.session.get('cfgAt');
      if (Date.now() - cfgAt > 5000) { await ext.storage.session.set({ cfgAt: Date.now() }); await refreshConfig(); }
      return { ok: true };
    }
    if (msg.type === 'options') { ext.runtime.openOptionsPage(); return { ok: true }; }
    return { ok: false };
  })().then(reply, (e) => reply({ ok: false, error: String(e && e.message || e) }));
  return true;
});

ext.runtime.onInstalled.addListener((details) => {
  ext.contextMenus.create({ id: 'downloadan', title: 'Download with DownloaDan', contexts: ['link'] });
  if (details && details.reason === 'install') ext.runtime.openOptionsPage();     // the options page finds DownloaDan and connects
});
ext.contextMenus.onClicked.addListener(async (info) => {
  const url = info.linkUrl;
  if (!url || !/^https?:/i.test(url)) return;
  const r = await post({ url, referer: info.pageUrl });
  if (!r.ok) {
    ext.action.setBadgeBackgroundColor({ color: '#d93a4a' }); ext.action.setBadgeText({ text: '!' });
    setTimeout(() => ext.action.setBadgeText({ text: '' }), 4000);
  }
});
ext.action.onClicked.addListener(() => ext.runtime.openOptionsPage());

// ---- Capture normal browser downloads (like IDM) -------------------------------------------
// The browser download is paused, handed to DownloaDan, and only cancelled once DownloaDan accepted it.
// If DownloaDan is closed or not paired, the browser simply carries on, so you never lose a download.
ext.downloads.onCreated.addListener(async (item) => {
  try {
    const { capture = true } = await ext.storage.local.get('capture');
    if (!capture || item.byExtensionId) return;
    const url = item.finalUrl || item.url;
    if (!/^https?:/i.test(url || '')) return;            // blob:/data:/file: stay with the browser
    if (await bypassActive()) return;                    // the bypass key is held: let the browser download it
    try { await ext.downloads.pause(item.id); } catch { /* may already be finished */ }
    const r = await post({ url, referer: item.referrer || undefined });
    if (r.ok) {
      try { await ext.downloads.cancel(item.id); await ext.downloads.erase({ id: item.id }); } catch { /* ignore */ }
    } else {
      try { await ext.downloads.resume(item.id); } catch { /* ignore */ }
      ext.action.setBadgeBackgroundColor({ color: '#d93a4a' }); ext.action.setBadgeText({ text: '!' });
      setTimeout(() => ext.action.setBadgeText({ text: '' }), 4000);
    }
  } catch { /* never break the browser's own download */ }
});
