'use strict';
// Service worker: talks to the DownloaDan app and remembers media URLs seen per tab.
importScripts('bridge-client.js');                      // provides DDBridge: finds the app and talks to it
const MEDIA_EXT = /\.(mp4|m4v|webm|mkv|mov|mp3|m4a|ogg|opus|flac|wav|aac)(\?|#|$)/i;
let chain = Promise.resolve();                         // serialises storage writes

chrome.webRequest.onResponseStarted.addListener((d) => {
  if (d.tabId < 0 || (d.statusCode !== 200 && d.statusCode !== 206)) return;
  const h = {};
  for (const x of d.responseHeaders || []) h[x.name.toLowerCase()] = x.value || '';
  const ct = (h['content-type'] || '').toLowerCase();
  if (/mpegurl|dash\+xml|mp2t/.test(ct)) return;       // streaming playlists/segments are not supported
  const isMedia = ct.startsWith('video/') || ct.startsWith('audio/') || (MEDIA_EXT.test(d.url) && !ct.startsWith('text/'));
  if (!isMedia) return;
  let size = parseInt(h['content-length'] || '0', 10);
  const cr = /\/(\d+)$/.exec(h['content-range'] || '');
  if (cr) size = parseInt(cr[1], 10);
  if (size && size < 150 * 1024) return;                // ignore tiny fragments
  chain = chain.then(async () => {
    const key = 'tab' + d.tabId;
    const cur = (await chrome.storage.session.get(key))[key] || [];
    if (!cur.some((m) => m.url === d.url)) cur.push({ url: d.url, size, type: ct.split(';')[0] });
    await chrome.storage.session.set({ [key]: cur.slice(-20) });
  }).catch(() => {});
}, { urls: ['<all_urls>'], types: ['media', 'xmlhttprequest', 'other'] }, ['responseHeaders']);

chrome.tabs.onUpdated.addListener((tabId, info) => { if (info.status === 'loading' && info.url) chrome.storage.session.remove('tab' + tabId); });
chrome.tabs.onRemoved.addListener((tabId) => chrome.storage.session.remove('tab' + tabId));

// ---- settings that live in the app (the bypass key) ---------------------------------------------
function applyConfig(c) { if (c && typeof c.bypassKey === 'string') chrome.storage.local.set({ bypassKey: c.bypassKey }); }
// One request to the app. Tries the last known port first; if the app moved (that port was busy at its start), look for it again.
async function api(op, body) {
  const { token = '', port } = await chrome.storage.local.get(['token', 'port']);
  if (!token) return { ok: false, error: 'not-paired' };
  let r = port ? await DDBridge.call(port, token, op, body) : { status: 0 };
  if (r.status === 0) {
    const found = (await DDBridge.discover(port))[0];
    if (!found) return { ok: false, error: 'offline' };
    if (found.port !== port) await chrome.storage.local.set({ port: found.port });
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
  const { bypass } = await chrome.storage.session.get('bypass');
  if (!bypass) return false;
  const now = Date.now();
  return (bypass.held && now - (bypass.heldAt || 0) < 15000) || now - (bypass.clickAt || 0) < 4000;   // old "held" marks expire, so a lost key-up can't disable capture
}

async function cookieHeader(url) {
  try {
    const list = await chrome.cookies.getAll({ url });
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

chrome.runtime.onMessage.addListener((msg, sender, reply) => {
  if (sender.id !== chrome.runtime.id) return false;
  (async () => {
    if (msg.type === 'getMedia') {
      const key = 'tab' + (sender.tab && sender.tab.id);
      return { items: ((await chrome.storage.session.get(key))[key] || []) };
    }
    if (msg.type === 'add') return post(msg);
    if (msg.type === 'bypass') {                       // the page reports the bypass key going down/up, or a click while it is held
      const cur = (await chrome.storage.session.get('bypass')).bypass || {};
      const now = Date.now();
      await chrome.storage.session.set({ bypass: { held: !!msg.held, heldAt: msg.held ? now : cur.heldAt || 0, clickAt: msg.click ? now : cur.clickAt || 0 } });
      return { ok: true };
    }
    if (msg.type === 'refreshConfig') {                // a tab was focused: pick up a key changed in the app (at most every 5 s)
      const { cfgAt = 0 } = await chrome.storage.session.get('cfgAt');
      if (Date.now() - cfgAt > 5000) { await chrome.storage.session.set({ cfgAt: Date.now() }); await refreshConfig(); }
      return { ok: true };
    }
    if (msg.type === 'options') { chrome.runtime.openOptionsPage(); return { ok: true }; }
    return { ok: false };
  })().then(reply, (e) => reply({ ok: false, error: String(e && e.message || e) }));
  return true;
});

chrome.runtime.onInstalled.addListener((details) => {
  chrome.contextMenus.create({ id: 'downloadan', title: 'Download with DownloaDan', contexts: ['link', 'video', 'audio'] });
  if (details && details.reason === 'install') chrome.runtime.openOptionsPage();     // the options page finds DownloaDan and connects
});
chrome.contextMenus.onClicked.addListener(async (info) => {
  const url = info.linkUrl || info.srcUrl;
  if (!url || !/^https?:/i.test(url)) return;
  const r = await post({ url, referer: info.pageUrl });
  if (!r.ok) {
    chrome.action.setBadgeBackgroundColor({ color: '#d93a4a' }); chrome.action.setBadgeText({ text: '!' });
    setTimeout(() => chrome.action.setBadgeText({ text: '' }), 4000);
  }
});
chrome.action.onClicked.addListener(() => chrome.runtime.openOptionsPage());

// ---- Capture normal browser downloads (like IDM) -------------------------------------------
// The browser download is paused, handed to DownloaDan, and only cancelled once DownloaDan accepted it.
// If DownloaDan is closed or not paired, the browser simply carries on, so you never lose a download.
chrome.downloads.onCreated.addListener(async (item) => {
  try {
    const { capture = true } = await chrome.storage.local.get('capture');
    if (!capture || item.byExtensionId) return;
    const url = item.finalUrl || item.url;
    if (!/^https?:/i.test(url || '')) return;            // blob:/data:/file: stay with the browser
    if (await bypassActive()) return;                    // the bypass key is held: let the browser download it
    try { await chrome.downloads.pause(item.id); } catch { /* may already be finished */ }
    const r = await post({ url, referer: item.referrer || undefined });
    if (r.ok) {
      try { await chrome.downloads.cancel(item.id); await chrome.downloads.erase({ id: item.id }); } catch { /* ignore */ }
    } else {
      try { await chrome.downloads.resume(item.id); } catch { /* ignore */ }
      chrome.action.setBadgeBackgroundColor({ color: '#d93a4a' }); chrome.action.setBadgeText({ text: '!' });
      setTimeout(() => chrome.action.setBadgeText({ text: '' }), 4000);
    }
  } catch { /* never break the browser's own download */ }
});
