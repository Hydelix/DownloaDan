'use strict';
const path = require('path');
const fs = require('fs');
const crypto = require('crypto');
const { app, BrowserWindow, Tray, Menu, ipcMain, dialog, shell, clipboard, nativeImage, Notification, session, screen } = require('electron');
const { Store } = require('./store');
const { Manager, defaultState } = require('./manager');
const { inspect } = require('./engine');
const { createUpdater } = require('./updater');
const { createBridge } = require('./bridge');
const { createApi } = require('./api');
const { ALLOWED_EXTENSION_IDS } = require('./extension-ids');
const theme = require('./theme');

// Lightweight: no GPU process needed for a simple list UI.
app.disableHardwareAcceleration();
const gotLock = app.requestSingleInstanceLock();
if (!gotLock) app.quit();
app.setAppUserModelId('com.downloadan.app');
Menu.setApplicationMenu(null);

const ASSETS = path.join(__dirname, '..', '..', 'assets');
const INDEX = path.join(__dirname, '..', 'renderer', 'index.html');
const PROMPT_HTML = path.join(__dirname, '..', 'renderer', 'prompt.html');
const PAIR_HTML = path.join(__dirname, '..', 'renderer', 'pair.html');
const PORT_RANGE = Array.from({ length: 10 }, (_, i) => 18642 + i);      // keep in sync with extension/bridge-client.js
const startHidden = process.argv.includes('--hidden');

let store, mgr, bridge, api, updater, win = null, tray = null, quitting = false;
let rendererReady = false, bridgeStatus = { ok: false, error: 'Not started' };
let promptWin = null, promptCur = null;
const prompts = [];          // "New download" requests from the browser, waiting for the small box

let manualCheck = false, notifiedVersion = null;

// ---------- helpers ----------
const send = (ch, payload) => { if (win && !win.isDestroyed() && rendererReady) win.webContents.send(ch, payload); };
const allThemes = () => [...theme.BUILTIN, ...store.data.themes.custom];
const activeTheme = () => allThemes().find((t) => t.id === store.data.themes.active) || theme.BUILTIN[0];

function applyChrome() {
  if (!win || win.isDestroyed()) return;
  const t = activeTheme();
  win.setBackgroundColor(t.colors.bg);
  try { win.setTitleBarOverlay({ color: t.colors.bg, symbolColor: t.colors.text, height: 40 }); } catch { /* not supported */ }
}

// ---------- remember the window's size, position and maximized state ----------
const MIN_W = 860, MIN_H = 540, DEFAULT_W = 1080, DEFAULT_H = 700;
function initialBounds() {
  const s = store.data.window;
  const out = { width: DEFAULT_W, height: DEFAULT_H };
  if (!s || !Number.isFinite(s.width) || !Number.isFinite(s.height)) return out;
  let { width, height } = s;
  const hasPos = Number.isFinite(s.x) && Number.isFinite(s.y);
  // Which screen would it be on? If its title bar is no longer reachable (monitor unplugged, resolution changed), centre it instead.
  const area = (d) => d.workArea;
  const overlap = (a, b) => Math.max(0, Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x)) * Math.max(0, Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y));
  let target = null;
  if (hasPos) {
    const rect = { x: s.x, y: s.y, width, height };
    const best = screen.getAllDisplays().map((d) => ({ d, o: overlap(area(d), rect) })).sort((a, b) => b.o - a.o)[0];
    const bar = { x: s.x, y: s.y, width, height: 40 };
    if (best && best.o >= 200 * 100 && overlap(area(best.d), bar) >= 120 * 20) target = best.d;
  }
  const disp = target || screen.getPrimaryDisplay();
  width = Math.max(MIN_W, Math.min(width, area(disp).width));        // never bigger than the screen it opens on
  height = Math.max(MIN_H, Math.min(height, area(disp).height));
  out.width = Math.round(width); out.height = Math.round(height);
  if (target) {
    const w = area(target);
    out.x = Math.round(Math.max(w.x, Math.min(s.x, w.x + w.width - 160)));
    out.y = Math.round(Math.max(w.y, Math.min(s.y, w.y + w.height - 60)));
  }
  return out;
}
function saveBounds() {
  if (!win || win.isDestroyed() || win.isFullScreen()) return;
  const prev = store.data.window;
  if (win.isMaximized() && prev && Number.isFinite(prev.width)) store.data.window = { ...prev, maximized: true };   // keep the last real size; only note "maximized"
  else {
    const b = win.getNormalBounds();                                  // the un-maximized size, even while minimized
    store.data.window = { x: b.x, y: b.y, width: b.width, height: b.height, maximized: win.isMaximized() };
  }
  store.save();
}

function createWindow() {
  if (win && !win.isDestroyed()) { if (win.isMinimized()) win.restore(); win.show(); win.focus(); return; }
  const t = activeTheme();
  rendererReady = false;
  win = new BrowserWindow({
    ...initialBounds(), minWidth: MIN_W, minHeight: MIN_H, show: false,
    backgroundColor: t.colors.bg, title: 'DownloaDan', icon: path.join(ASSETS, 'icon.png'),
    titleBarStyle: 'hidden', titleBarOverlay: { color: t.colors.bg, symbolColor: t.colors.text, height: 40 },
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'), contextIsolation: true, nodeIntegration: false,
      sandbox: true, spellcheck: false, devTools: !app.isPackaged, webSecurity: true,
    },
  });
  win.loadFile(INDEX);
  const sweep = setInterval(() => mgr.pruneMissing(), 30000);       // only runs while the window is open
  win.on('focus', () => mgr.pruneMissing());
  win.on('closed', () => clearInterval(sweep));
  let boundsTimer = null;
  const later = () => { clearTimeout(boundsTimer); boundsTimer = setTimeout(saveBounds, 600); };      // not on every pixel of a drag
  for (const e of ['resize', 'move', 'maximize', 'unmaximize']) win.on(e, later);
  if (store.data.window && store.data.window.maximized) win.maximize();
  win.once('ready-to-show', () => win.show());
  win.on('close', () => { clearTimeout(boundsTimer); saveBounds(); if (!store.data.settings.minimizeToTray && !quitting) app.quit(); });
  win.on('closed', () => { win = null; rendererReady = false; });   // renderer memory is released here
}

// ---------- "New download" box (small always-on-top window, created on demand, destroyed after) ----------
function promptPayload() {
  const snap = mgr.snapshot();
  return { pid: promptCur.pid, url: promptCur.url, theme: activeTheme(), queues: snap.queues,
    settings: { downloadDir: snap.settings.downloadDir }, waiting: prompts.length };
}
function openPrompt() {
  const t = activeTheme();
  const w = new BrowserWindow({
    width: 500, height: 560, useContentSize: true, resizable: false, minimizable: false, maximizable: false, fullscreenable: false,
    alwaysOnTop: true, show: false, backgroundColor: t.colors.bg, title: 'DownloaDan: New download', icon: path.join(ASSETS, 'icon.png'),
    titleBarStyle: 'hidden', titleBarOverlay: { color: t.colors.bg, symbolColor: t.colors.text, height: 40 },
    webPreferences: { preload: path.join(__dirname, 'preload-prompt.js'), contextIsolation: true, nodeIntegration: false, sandbox: true, spellcheck: false, devTools: !app.isPackaged },
  });
  promptWin = w;
  w.loadFile(PROMPT_HTML);
  w.once('ready-to-show', () => { w.show(); w.focus(); });
  w.on('closed', () => { promptWin = null; promptCur = null; if (prompts.length) openPrompt(); });   // closing the box = cancel
}
// ---------- "Connect browser" permission box (small always-on-top window, only while a request is waiting) ----------
let pairWin = null;
function openPair() {
  if (pairWin && !pairWin.isDestroyed()) { pairWin.show(); pairWin.focus(); return; }
  const t = activeTheme();
  const w = new BrowserWindow({
    width: 460, height: 400, useContentSize: true, resizable: false, minimizable: false, maximizable: false, fullscreenable: false,
    alwaysOnTop: true, show: false, backgroundColor: t.colors.bg, title: 'DownloaDan: Connect browser', icon: path.join(ASSETS, 'icon.png'),
    titleBarStyle: 'hidden', titleBarOverlay: { color: t.colors.bg, symbolColor: t.colors.text, height: 40 },
    webPreferences: { preload: path.join(__dirname, 'preload-pair.js'), contextIsolation: true, nodeIntegration: false, sandbox: true, spellcheck: false, devTools: !app.isPackaged },
  });
  pairWin = w;
  w.loadFile(PAIR_HTML);
  w.once('ready-to-show', () => { w.show(); w.focus(); });
  w.on('closed', () => { pairWin = null; const open = api.pendingPairing(); if (open) api.resolvePairing(open.id, false); });   // closing the box counts as Deny
}
function closePair() { if (pairWin && !pairWin.isDestroyed()) pairWin.close(); }
function handlePair(channel, fn) {
  ipcMain.handle(channel, (e, ...args) => {
    if (!pairWin || pairWin.isDestroyed() || e.sender !== pairWin.webContents) throw new Error('Rejected');
    return fn(...args);
  });
}

// ---------- "Refresh download address": wait for the browser to catch the file again ----------
let refreshWait = null;                                    // { id, seq, pending, timer }
const refreshSend = (payload) => send('refresh:event', payload);
const windowReady = () => !!(win && !win.isDestroyed() && rendererReady);
const baseName = (u) => { try { return decodeURIComponent(new URL(u).pathname.split('/').filter(Boolean).pop() || ''); } catch { return ''; } };
function refreshClear() { if (refreshWait) clearTimeout(refreshWait.timer); refreshWait = null; }

async function refreshCapture(p) {
  const w = refreshWait; if (!w) return;
  const d = mgr.find(w.id);
  if (!d || d.status === 'completed' || d.status === 'downloading') { refreshClear(); refreshSend({ type: 'error', message: 'That download is no longer waiting for a new link.' }); return; }
  const my = ++w.seq; w.pending = null;
  refreshSend({ type: 'checking' });
  const info = await inspect(p.url, { referer: p.referer || d.referer, cookie: p.cookie || d.cookie });
  if (refreshWait !== w || w.seq !== my) return;           // cancelled, or a newer link arrived meanwhile
  if (!info.ok) { refreshSend({ type: 'error', message: `That link did not work (${info.error}). Still waiting for another one…` }); return; }
  const verdict = mgr.compareForRefresh(d, info);
  if (verdict.ok) { refreshFinish(w, p, info, false); return; }
  if (!windowReady()) { refreshClear(); promptAdd(p); return; }   // nobody to ask: fall back to the normal New download box
  w.pending = { p, info };
  refreshSend({ type: 'confirm', reasons: verdict.reasons, discards: d.downloaded || 0,
    expected: { name: d.filename || baseName(d.url), size: d.size || 0 }, got: { name: info.filename, size: info.size || 0, resumable: !!info.resumable } });
}
function refreshFinish(w, p, info, restart) {
  const d = mgr.find(w.id);
  refreshClear();
  if (!d) return;
  const keepName = !d.filename || mgr.sameName(d.filename, info.filename);
  mgr.refreshAddress(d.id, { url: p.url, referer: p.referer, cookie: p.cookie }, { restart, resetName: restart && !keepName });
  refreshSend({ type: 'done', restarted: restart });
  if (!windowReady() && Notification.isSupported()) new Notification({ title: 'Download link refreshed', body: d.filename || baseName(p.url) }).show();
}

function promptAdd(p) {
  if (refreshWait) { refreshCapture(p); return; }          // a refresh is waiting: this catch belongs to it
  const dup = (x) => x.url === p.url;
  if ((promptCur && dup(promptCur)) || prompts.some(dup)) { if (promptWin && !promptWin.isDestroyed()) promptWin.focus(); return; }
  prompts.push({ ...p, pid: crypto.randomBytes(4).toString('hex') });
  if (promptWin && !promptWin.isDestroyed()) { promptWin.show(); promptWin.focus(); }
  else openPrompt();
}
function finishPrompt() {
  promptCur = prompts.shift() || null;
  if (promptCur && promptWin && !promptWin.isDestroyed()) promptWin.webContents.send('prompt:next', promptPayload());
  else if (promptWin && !promptWin.isDestroyed()) promptWin.close();
}
async function freeSpace(dir) {
  let p = path.resolve(String(dir || ''));
  for (let i = 0; i < 12 && !fs.existsSync(p); i++) p = path.dirname(p);
  try { const s = await fs.promises.statfs(p); return Number(s.bavail) * Number(s.bsize); } catch { return null; }
}
function handlePrompt(channel, fn) {
  ipcMain.handle(channel, (e, ...args) => {
    if (!promptWin || promptWin.isDestroyed() || e.sender !== promptWin.webContents) throw new Error('Rejected');
    return fn(...args);
  });
}

function createTray() {
  tray = new Tray(nativeImage.createFromPath(path.join(ASSETS, 'tray.png')));
  tray.setToolTip('DownloaDan');
  tray.setContextMenu(Menu.buildFromTemplate([
    { label: 'Open DownloaDan', click: createWindow },
    { label: 'Pause all downloads', click: () => mgr.pauseAll() },
    { type: 'separator' },
    { label: 'Quit', click: () => { quitting = true; app.quit(); } },
  ]));
  tray.on('click', createWindow);
}

async function startBridge() {
  const preferred = store.data.settings.bridgePort;
  bridgeStatus = await bridge.listenAny([...new Set([preferred, ...PORT_RANGE])]);      // preferred port first, then the next free one in the range
  send('bridge:status', bridgeStatus);
}

// ---------- IPC (every handler validates its input; the renderer is untrusted) ----------
function handle(channel, fn) {
  ipcMain.handle(channel, (e, ...args) => {
    if (!win || e.sender !== win.webContents) throw new Error('Rejected');
    return fn(...args);
  });
}
const strArr = (a) => (Array.isArray(a) ? a.filter((x) => typeof x === 'string').slice(0, 500) : []);

function registerIpc() {
  handle('state:get', async () => {
    await mgr.pruneMissing();
    rendererReady = true;
    return { ...mgr.snapshot(), builtinThemes: theme.BUILTIN, bridge: bridgeStatus, pending: [] };
  });
  handle('clip:read', async () => { try { return String(await clipboard.readText()).slice(0, 4096); } catch { return ''; } });
  handle('clip:write', async (t) => { if (typeof t === 'string') await clipboard.writeText(t.slice(0, 8192)); });

  handle('dl:add', (p) => mgr.add({ ...(p || {}), start: !!(p && p.start) }));
  handle('dl:pause', (ids) => mgr.pause(strArr(ids)));
  handle('dl:pauseAll', () => mgr.pauseAll());
  handle('dl:resume', (ids) => mgr.resume(strArr(ids)));
  handle('dl:remove', (ids, del) => mgr.remove(strArr(ids), !!del));
  handle('dl:clearCompleted', () => mgr.clearCompleted());
  handle('dl:move', (ids, q) => mgr.moveToQueue(strArr(ids), String(q)));
  handle('dl:open', async (i) => { const f = mgr.filePath(String(i)); if (f && fs.existsSync(f)) return shell.openPath(f); return 'File not found'; });
  handle('dl:show', (i) => { const f = mgr.locatePath(String(i)); if (f) shell.showItemInFolder(f); });

  handle('q:create', (n) => mgr.createQueue(n));
  handle('q:rename', (i, n) => mgr.renameQueue(String(i), n));
  handle('q:delete', (i) => mgr.deleteQueue(String(i)));
  handle('q:enable', (i, on) => mgr.setQueueEnabled(String(i), !!on));
  handle('q:start', (i) => mgr.startQueue(String(i)));

  // connect-browser box
  handlePair('pair:init', () => {
    const p = api.pendingPairing(); if (!p) return null;
    return { theme: activeTheme(), browser: p.browser, extensionId: p.extensionId, secondsLeft: Math.max(0, 60 - Math.floor((Date.now() - p.createdAt) / 1000)) };
  });
  handlePair('pair:answer', (allow) => { const p = api.pendingPairing(); if (p) api.resolvePairing(p.id, !!allow); closePair(); return { ok: true }; });

  // updates
  handle('update:get', () => updater.state());
  handle('update:check', async () => { manualCheck = true; await updater.check(); manualCheck = false; return updater.state(); });
  handle('update:download', () => updater.download());
  handle('update:install', () => updater.install());

  // refresh download address
  handle('refresh:start', (i) => {
    const d = mgr.find(String(i));
    if (!d || !['error', 'paused', 'queued'].includes(d.status)) return { ok: false, error: 'This download cannot be refreshed right now.' };
    refreshClear();
    const w = { id: d.id, seq: 0, pending: null, timer: null };
    w.timer = setTimeout(() => { if (refreshWait === w) { refreshClear(); refreshSend({ type: 'timeout' }); } }, 10 * 60 * 1000);
    refreshWait = w;
    return { ok: true, name: d.filename || baseName(d.url), size: d.size || 0, downloaded: d.downloaded || 0 };
  });
  handle('refresh:cancel', () => { refreshClear(); });
  handle('refresh:url', (u) => {
    if (!refreshWait) return { ok: false, error: 'Not waiting for a link any more.' };
    let url; try { url = new URL(String(u).trim()); } catch { return { ok: false, error: 'That is not a valid link.' }; }
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return { ok: false, error: 'Only http:// and https:// links are supported.' };
    refreshCapture({ url: url.href });
    return { ok: true };
  });
  handle('refresh:decide', (choice) => {
    const w = refreshWait; if (!w || !w.pending) return { ok: false };
    const { p, info } = w.pending;
    if (choice === 'replace') { refreshFinish(w, p, info, true); return { ok: true }; }
    if (choice === 'new') { refreshClear(); promptAdd(p); return { ok: true }; }
    w.pending = null; refreshSend({ type: 'waiting' }); return { ok: true };       // keep waiting
  });

  // "New download" box
  handlePrompt('prompt:init', () => { if (!promptCur) promptCur = prompts.shift() || null; return promptCur ? promptPayload() : null; });
  handlePrompt('prompt:probe', async (pid) => {
    const c = promptCur; if (!c || c.pid !== pid) return { ok: false, error: 'Request changed' };
    return inspect(c.url, { referer: c.referer, cookie: c.cookie });
  });
  handlePrompt('prompt:free', (dir) => freeSpace(dir));
  handlePrompt('prompt:dir', async (start) => {
    const r = await dialog.showOpenDialog(promptWin, { properties: ['openDirectory', 'createDirectory'], defaultPath: typeof start === 'string' && path.isAbsolute(start) ? start : undefined });
    return r.canceled ? null : r.filePaths[0];
  });
  handlePrompt('prompt:confirm', (p) => {
    const c = promptCur;
    if (!c || !p || p.pid !== c.pid) return { ok: false };
    const start = !!p.startQueue;
    const r = mgr.add({ urls: [c.url], queueId: String(p.queueId), dir: p.dir, filename: typeof p.filename === 'string' ? p.filename : undefined,
      referer: c.referer, cookie: c.cookie, hold: !start });
    if (!r.added.length) return { ok: false };
    if (start) mgr.startQueue(mgr.find(r.added[0]).queueId);
    finishPrompt();
    return { ok: true };
  });
  handlePrompt('prompt:cancel', (pid) => { if (promptCur && promptCur.pid === pid) finishPrompt(); });

  handle('settings:set', async (p) => {
    const before = store.data.settings.bridgePort;
    mgr.setSettings(p);
    if (p && p.removeMissing === true) mgr.pruneMissing();
    const s = store.data.settings;
    if (app.isPackaged) app.setLoginItemSettings({ openAtLogin: s.runAtStartup, args: ['--hidden'] });
    if (s.bridgePort !== before) await startBridge();
  });
  handle('dir:choose', async () => {
    const r = await dialog.showOpenDialog(win, { properties: ['openDirectory', 'createDirectory'], defaultPath: store.data.settings.downloadDir });
    return r.canceled ? null : r.filePaths[0];
  });
  handle('token:regen', () => mgr.regenerateToken());

  // themes
  handle('theme:validate', (t) => { const r = theme.validateTheme(t); return r.ok ? { ok: true } : r; });
  handle('theme:save', (t) => {
    const { id: tid, ...body } = t || {};
    const r = theme.validateTheme(body);
    if (!r.ok) return r;
    const list = store.data.themes.custom;
    const existing = list.find((x) => x.id === tid);
    if (existing) Object.assign(existing, r.theme);
    else {
      if (list.length >= 50) return { ok: false, error: 'Theme limit reached (50).' };
      list.push({ id: 't_' + require('crypto').randomBytes(5).toString('hex'), ...r.theme });
    }
    const saved = existing || list[list.length - 1];
    store.data.themes.active = saved.id;
    store.save(); applyChrome(); send('meta', mgr.snapshot());
    return { ok: true, id: saved.id };
  });
  handle('theme:apply', (tid) => {
    if (!allThemes().some((t) => t.id === tid)) return;
    store.data.themes.active = tid; store.save(); applyChrome(); send('meta', mgr.snapshot());
  });
  handle('theme:delete', (tid) => {
    const list = store.data.themes.custom; const i = list.findIndex((t) => t.id === tid);
    if (i < 0) return;
    list.splice(i, 1);
    if (store.data.themes.active === tid) store.data.themes.active = 'dark';
    store.save(); applyChrome(); send('meta', mgr.snapshot());
  });
  handle('theme:export', async (tid) => {
    const t = allThemes().find((x) => x.id === tid); if (!t) return { ok: false, error: 'Theme not found.' };
    const r = await dialog.showSaveDialog(win, { defaultPath: t.name.replace(/[^\w\- ]+/g, '').trim() + '.dantheme', filters: [{ name: 'DownloaDan theme', extensions: ['dantheme'] }] });
    if (r.canceled || !r.filePath) return { ok: false, cancelled: true };
    fs.writeFileSync(r.filePath, theme.serializeTheme(t), 'utf8');
    return { ok: true };
  });
  handle('theme:import', async () => {
    const r = await dialog.showOpenDialog(win, { properties: ['openFile'], filters: [{ name: 'DownloaDan theme', extensions: ['dantheme', 'fluxtheme'] }] });
    if (r.canceled || !r.filePaths[0]) return { ok: false, cancelled: true };
    const file = r.filePaths[0];
    let text;
    try {
      if (fs.statSync(file).size > 8 * 1024) return { ok: false, error: 'Theme file is too large (max 8 KB).' };   // size check BEFORE reading
      text = fs.readFileSync(file, 'utf8');
    } catch { return { ok: false, error: 'Could not read that file.' }; }
    const parsed = theme.parseThemeText(text);
    if (!parsed.ok) return parsed;
    if (store.data.themes.custom.length >= 50) return { ok: false, error: 'Theme limit reached (50).' };
    const entry = { id: 't_' + require('crypto').randomBytes(5).toString('hex'), ...parsed.theme };
    store.data.themes.custom.push(entry);
    store.save(); send('meta', mgr.snapshot());
    return { ok: true, id: entry.id, name: entry.name };
  });
}

// ---------- lifecycle ----------
app.on('second-instance', () => createWindow());
app.on('window-all-closed', () => { /* stay alive in the tray */ });
app.on('before-quit', async (e) => {
  if (quitting === 'done') return;
  e.preventDefault(); quitting = true;
  bridge.close();
  await mgr.shutdown();
  quitting = 'done';
  app.quit();
});

app.on('web-contents-created', (_e, wc) => {
  wc.on('will-navigate', (ev) => ev.preventDefault());
  wc.setWindowOpenHandler(() => ({ action: 'deny' }));
  wc.on('will-attach-webview', (ev) => ev.preventDefault());
});

// One-time carry-over from the app's former name: keeps downloads, queues, settings, themes and the browser pairing token.
function migrateOldData() {
  try {
    const target = path.join(app.getPath('userData'), 'state.json');
    const old = path.join(app.getPath('appData'), 'FluxDM', 'state.json');
    if (!fs.existsSync(target) && fs.existsSync(old)) { fs.mkdirSync(path.dirname(target), { recursive: true }); fs.copyFileSync(old, target); }
  } catch { /* if it cannot be copied, start fresh */ }
}

app.whenReady().then(async () => {
  if (!gotLock) return;
  migrateOldData();
  session.defaultSession.setPermissionRequestHandler((_wc, _p, cb) => cb(false));
  store = new Store(path.join(app.getPath('userData'), 'state.json'), defaultState(app.getPath('downloads')));
  mgr = new Manager(store);
  mgr.trash = (f) => shell.trashItem(f);                    // deleted files go to the Recycle Bin
  mgr.on('patch', (() => {
    let timer = null; const ids = new Set(), removed = new Set();
    return ({ ids: a, removed: r }) => {
      a.forEach((i) => ids.add(i)); r.forEach((i) => removed.add(i));
      if (timer) return;
      timer = setTimeout(() => {
        timer = null;
        const items = [...ids].map((i) => mgr.find(i)).filter(Boolean).map((d) => mgr.summary(d));
        ids.clear();
        const rem = [...removed]; removed.clear();
        send('patch', { items, removed: rem });
      }, 250);
    };
  })());
  mgr.on('meta', () => send('meta', mgr.snapshot()));
  mgr.on('pruned', (n) => send('notice', `Removed ${n} download${n === 1 ? '' : 's'} whose file was moved or deleted`));
  mgr.on('completed', (d) => {
    if (!store.data.settings.notify || !Notification.isSupported()) return;
    const n = new Notification({ title: 'Download complete', body: d.filename || d.url, silent: false });
    n.on('click', () => { const f = mgr.locatePath(d.id); if (f) shell.showItemInFolder(f); });
    n.show();
  });
  api = createApi({
    getToken: () => store.data.token, getConfig: () => ({ bypassKey: store.data.settings.bypassKey }), onAdd: promptAdd, version: app.getVersion(),
    allowedIds: ALLOWED_EXTENSION_IDS, onPairRequest: openPair, onPairEnd: closePair,
  });
  bridge = createBridge({ api });
  registerIpc();
  let autoUpdater = null;
  try { autoUpdater = require('electron-updater').autoUpdater; } catch { /* not installed: updater reports it */ }
  updater = createUpdater({ autoUpdater, currentVersion: app.getVersion(), packaged: app.isPackaged, beforeInstall: () => { quitting = true; mgr.markResumeOnStart(); } });
  updater.on('state', (s) => {
    send('update:state', s);
    if (s.state === 'available' && !manualCheck && notifiedVersion !== s.version && Notification.isSupported()) {   // found by the automatic check
      notifiedVersion = s.version;
      const n = new Notification({ title: 'DownloaDan update available', body: `Version ${s.version} is ready to download.` });
      n.on('click', createWindow); n.show();
    }
  });
  const autoCheck = () => { if (store.data.settings.autoUpdate) updater.check(); };
  setTimeout(autoCheck, 8000);                         // shortly after start, then every 6 hours while running
  setInterval(autoCheck, 6 * 60 * 60 * 1000).unref();
  mgr.schedule();
  mgr.pruneMissing();
  createTray();
  await startBridge();
  if (!startHidden) createWindow();
});
