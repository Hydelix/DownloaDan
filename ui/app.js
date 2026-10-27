'use strict';
/* DownloaDan renderer. Rule: untrusted text (file names, URLs, theme names) is only ever
   inserted with textContent/DOM APIs, never innerHTML. */
const api = window.dan;
const $ = (id) => document.getElementById(id);

const S = { downloads: new Map(), queues: [], settings: {}, themes: { active: 'dark', custom: [] }, builtin: [], token: '', bridge: { ok: false }, sel: new Set() };
const V = { queue: 'all', filter: 'all', search: '', lastClicked: null, cookie: undefined };
const COLOR_LABELS = { bg: 'Background', surface: 'Surface', surface2: 'Surface (raised)', border: 'Border', text: 'Text', textMuted: 'Muted text', accent: 'Accent', accentText: 'Accent text', success: 'Success', warning: 'Warning', danger: 'Danger' };

// ---------- helpers ----------
function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (k === 'class') el.className = v; else if (k.startsWith('data-') || k === 'title' || k === 'type') el.setAttribute(k, v);
    else if (k === 'on') for (const [ev, fn] of Object.entries(v)) el.addEventListener(ev, fn); else el[k] = v;
  }
  for (const c of kids) if (c != null) el.append(c);
  return el;
}
const setText = (el, t) => { if (el.textContent !== t) el.textContent = t; };
const fmtBytes = (n) => { if (!n) return '0 B'; const u = ['B', 'KB', 'MB', 'GB', 'TB']; const i = Math.min(4, Math.floor(Math.log(n) / Math.log(1024))); return (n / 1024 ** i).toFixed(i ? 1 : 0) + ' ' + u[i]; };
const fmtEta = (s) => (s == null ? '' : s < 60 ? s + 's' : s < 3600 ? Math.floor(s / 60) + 'm ' + (s % 60) + 's' : Math.floor(s / 3600) + 'h ' + Math.floor((s % 3600) / 60) + 'm');
function nameOf(d) {
  if (d.filename) return d.filename;
  try { return decodeURIComponent(new URL(d.url).pathname.split('/').filter(Boolean).pop() || new URL(d.url).hostname); } catch { return d.url; }
}
const hostOf = (u) => { try { return new URL(u).hostname; } catch { return ''; } };
const queueName = (id) => (S.queues.find((q) => q.id === id) || {}).name || 'Main';
let toastTimer = 0;
function toast(msg) { const t = $('toast'); t.textContent = msg; t.hidden = false; clearTimeout(toastTimer); toastTimer = setTimeout(() => { t.hidden = true; }, 2600); }

// ---------- theme ----------
const allThemes = () => [...S.builtin, ...S.themes.custom];
function applyThemeObj(t) {
  const r = document.documentElement;
  for (const k of Object.keys(COLOR_LABELS)) r.style.setProperty('--' + k, t.colors[k]);
  r.style.setProperty('--radius', t.shape.radius + 'px');
  r.dataset.density = t.shape.density; r.dataset.font = t.font;
  applyCols();                                            // row gap depends on density
}
function applyActiveTheme() { applyThemeObj(allThemes().find((t) => t.id === S.themes.active) || S.builtin[0]); }

// ---------- rendering ----------
let raf = 0;
const render = () => { if (!raf) raf = requestAnimationFrame(() => { raf = 0; renderAll(); }); };
function visible() {
  const q = V.search.toLowerCase();
  return [...S.downloads.values()].filter((d) => {
    if (V.queue !== 'all' && d.queueId !== V.queue) return false;
    if (V.filter === 'active' && !['downloading', 'queued', 'paused'].includes(d.status)) return false;
    if (V.filter === 'completed' && d.status !== 'completed') return false;
    if (V.filter === 'error' && d.status !== 'error') return false;
    return !q || nameOf(d).toLowerCase().includes(q) || d.url.toLowerCase().includes(q);
  }).sort(compare);
}

const rows = new Map(); let lastOrder = ''; let shown = [];
function createRow(d) {
  const r = { ico: h('div', { class: 'ico' }), nm: h('div', { class: 'nm2' }), sub: h('div', { class: 'sub' }), bar: h('i'), pct: h('span', { class: 'pct' }), speed: h('span', { class: 'num' }), eta: h('span', { class: 'num' }), badge: h('span', { class: 'badge' }) };
  r.el = h('div', { class: 'row', 'data-id': d.id }, r.ico, h('div', { class: 'name-cell' }, r.nm, r.sub), h('div', { class: 'prog' }, h('div', { class: 'bar' }, r.bar), r.pct), r.speed, r.eta, r.badge);
  return r;
}
// Tells the user WHY a queued item isn't moving, so "stuck" and "just waiting" look different.
function queuedNote(d) {
  if (d.hold) return 'Waiting: start the queue (▶) to begin';
  const q = S.queues.find((x) => x.id === d.queueId);
  if (q && !q.enabled) return 'Queue is stopped: click ▶ to run it';
  return 'Waiting for a free slot (' + (S.settings.maxSimultaneous || 1) + ' at a time)';
}
const LABEL = { queued: 'Queued', downloading: 'Downloading', paused: 'Paused', completed: 'Done', error: 'Failed' };
function updateRow(r, d) {
  const nm = nameOf(d); const ext = (nm.split('.').pop() || '').slice(0, 4);
  const pct = d.status === 'completed' ? 100 : d.size ? Math.min(100, (d.downloaded / d.size) * 100) : 0;
  r.el.dataset.s = d.status; r.el.classList.toggle('sel', S.sel.has(d.id));
  setText(r.ico, nm.includes('.') ? ext : 'file'); setText(r.nm, nm); r.nm.title = nm;
  setText(r.sub, d.status === 'error' && d.error ? d.error : d.status === 'downloading' && d.note ? d.note : d.status === 'queued' ? queuedNote(d) : [queueName(d.queueId), d.size ? (d.status === 'downloading' || d.status === 'paused' ? fmtBytes(d.downloaded) + ' of ' + fmtBytes(d.size) : fmtBytes(d.size)) : '', hostOf(d.url)].filter(Boolean).join(' · '));
  r.bar.style.width = pct + '%';
  setText(r.pct, d.size || d.status === 'completed' ? Math.floor(pct) + '%' : '');
  setText(r.speed, d.status === 'downloading' && d.speed ? fmtBytes(d.speed) + '/s' : '');
  setText(r.eta, d.status === 'downloading' ? fmtEta(d.eta) : '');
  setText(r.badge, d.status === 'queued' && d.hold ? 'Waiting' : LABEL[d.status] || d.status);
}

function renderList() {
  shown = visible();
  const order = shown.map((d) => d.id).join(',');
  for (const d of shown) { let r = rows.get(d.id); if (!r) { r = createRow(d); rows.set(d.id, r); } updateRow(r, d); }
  if (order !== lastOrder) {
    lastOrder = order;
    const keep = new Set(shown.map((d) => d.id));
    for (const id of [...rows.keys()]) if (!keep.has(id)) rows.delete(id);
    $('list').replaceChildren(...shown.map((d) => rows.get(d.id).el));
  }
  $('empty').hidden = shown.length > 0;
}

function renderQueues() {
  const counts = new Map(); let total = 0;
  for (const d of S.downloads.values()) if (d.status !== 'completed') { counts.set(d.queueId, (counts.get(d.queueId) || 0) + 1); total++; }
  const items = [{ id: 'all', name: 'All downloads' }, ...S.queues].map((q) => {
    const real = q.id !== 'all';
    const el = h('div', { class: 'q-item' + (V.queue === q.id ? ' active' : '') + (real && !q.enabled ? ' off' : ''), 'data-q': q.id },
      real ? h('span', { class: 'dot', title: q.enabled ? 'Queue is running (click to stop)' : 'Queue is stopped (click to run)', 'data-act': 'toggle' }) : h('span', { class: 'dot' }),
      h('span', { class: 'nm', textContent: q.name }),
      h('span', { class: 'ct', textContent: String(real ? counts.get(q.id) || 0 : total) }),
      real ? h('button', { class: 'icon-btn qb', title: 'Start queue', textContent: '▶', 'data-act': 'start' }) : null,
      real && q.id !== 'main' ? h('button', { class: 'icon-btn qb', title: 'Rename', textContent: '✎', 'data-act': 'rename' }) : null,
      real && q.id !== 'main' ? h('button', { class: 'icon-btn qb', title: 'Delete queue', textContent: '✕', 'data-act': 'delete' }) : null);
    return el;
  });
  $('queueList').replaceChildren(...items);
}

function fmtLimit(kb) { return kb >= 1024 ? +(kb / 1024).toFixed(1) + ' MB/s' : kb + ' KB/s'; }
function renderStatus() {
  const act = [...S.downloads.values()].filter((d) => d.status === 'downloading');
  setText($('stSpeed'), fmtBytes(act.reduce((a, d) => a + (d.speed || 0), 0)) + '/s');
  setText($('stCount'), act.length + ' active');
  const b = $('stBridge'); b.className = S.bridge.ok ? 'ok' : 'bad';
  setText(b, S.bridge.ok ? 'Browser link: port ' + (S.bridge.port || S.settings.bridgePort) : 'Browser link unavailable');
  setText($('parValue'), String(S.settings.maxSimultaneous || 3));
  const lim = $('stLimit'); lim.className = 'stlimit' + (S.settings.speedLimitOn ? ' on' : '');
  setText(lim, S.settings.speedLimitOn ? 'Speed limit: ' + fmtLimit(S.settings.speedLimitKbps) : 'No speed limit');
  const sel = [...S.sel].map((i) => S.downloads.get(i)).filter(Boolean);
  $('btnResume').disabled = !sel.some((d) => d.status === 'paused' || d.status === 'error');
  $('btnPause').disabled = !sel.some((d) => d.status === 'downloading' || d.status === 'queued');
  $('btnRemove').disabled = sel.length === 0;
  $('btnClear').disabled = ![...S.downloads.values()].some((d) => d.status === 'completed');
}
function renderAll() { renderList(); renderQueues(); renderStatus(); }

// ---------- state sync ----------
function loadMeta(m) {
  S.queues = m.queues; S.settings = m.settings; S.themes = m.themes; S.token = m.token;
  if (V.queue !== 'all' && !S.queues.some((q) => q.id === V.queue)) V.queue = 'all';
  if (!$('dlgTheme').open) applyActiveTheme();
  if ($('dlgSettings').open) { fillSettings(); renderThemeGrid(); }
  render();
}
api.on('patch', ({ items, removed }) => { for (const it of items) S.downloads.set(it.id, it); for (const i of removed) { S.downloads.delete(i); S.sel.delete(i); } render(); });
api.on('meta', loadMeta);
api.on('notice', toast);
api.on('bridge:status', (b) => { S.bridge = b; render(); });
api.on('ui:prompt', (p) => openAdd({ urls: [p.url], referer: p.referer, cookie: p.cookie }));


// ---------- sorting ----------
const loadJSON = (k, d) => { try { const v = JSON.parse(localStorage.getItem(k)); return v && typeof v === 'object' ? v : d; } catch { return d; } };
const saveJSON = (k, v) => { try { localStorage.setItem(k, JSON.stringify(v)); } catch { /* storage unavailable */ } };
const SORTS = { added: ['Date added', 'desc'], name: ['Name', 'asc'], size: ['Size', 'desc'], progress: ['Progress', 'desc'], speed: ['Speed', 'desc'], eta: ['Time left', 'asc'], status: ['Status', 'asc'], completed: ['Date completed', 'desc'] };
const COL_SORT = { name: 'name', prog: 'progress', speed: 'speed', eta: 'eta', status: 'status' };
const STATUS_RANK = { downloading: 0, queued: 1, paused: 2, error: 3, completed: 4 };
const savedSort = loadJSON('downloadan.sort', {});
V.sort = { key: SORTS[savedSort.key] ? savedSort.key : 'added', dir: savedSort.dir === 'asc' || savedSort.dir === 'desc' ? savedSort.dir : 'desc' };

function sortValue(d, key) {
  switch (key) {
    case 'name': return nameOf(d).toLowerCase();
    case 'size': return d.size || 0;
    case 'progress': return d.status === 'completed' ? 1 : d.size ? d.downloaded / d.size : 0;
    case 'speed': return d.status === 'downloading' ? d.speed || 0 : 0;
    case 'eta': return d.status === 'downloading' && d.eta != null ? d.eta : null;
    case 'status': return STATUS_RANK[d.status] ?? 9;
    case 'completed': return d.completedAt || null;
    default: return d.addedAt;
  }
}
function compare(a, b) {
  const { key, dir } = V.sort; const x = sortValue(a, key), y = sortValue(b, key);
  let r = 0;
  if (x == null || y == null) { if (x != null) return -1; if (y != null) return 1; }   // empty values always go last
  else r = typeof x === 'string' ? x.localeCompare(y, undefined, { numeric: true, sensitivity: 'base' }) : x - y;
  if (dir === 'desc') r = -r;
  return r || b.addedAt - a.addedAt || (a.id < b.id ? -1 : 1);
}
function setSort(key, dir) {
  V.sort = { key, dir: dir || (V.sort.key === key ? (V.sort.dir === 'asc' ? 'desc' : 'asc') : SORTS[key][1]) };
  saveJSON('downloadan.sort', V.sort); updateSortUI(); render();
}
function updateSortUI() {
  setText($('btnSort'), 'Sort: ' + SORTS[V.sort.key][0] + (V.sort.dir === 'asc' ? ' ↑' : ' ↓'));
  document.querySelectorAll('.hcell').forEach((c) => { setText(c.querySelector('.arrow'), COL_SORT[c.dataset.col] === V.sort.key ? (V.sort.dir === 'asc' ? '▲' : '▼') : ''); });
}
$('cols').addEventListener('click', (e) => {
  if (e.target.closest('.grip')) return;
  const c = e.target.closest('.hcell'); if (c) setSort(COL_SORT[c.dataset.col]);
});
$('btnSort').addEventListener('click', (e) => {
  e.stopPropagation();
  const m = $('sortMenu');
  if (!m.hidden) { m.hidden = true; return; }
  const item = (label, on, fn) => h('button', { type: 'button', textContent: (on ? '✓  ' : '     ') + label, on: { click: () => { hideCtx(); fn(); } } });
  m.replaceChildren(h('div', { class: 'hd', textContent: 'Sort by' }),
    // clicking the option that is already active swaps Ascending/Descending; picking another option keeps the current direction
    ...Object.keys(SORTS).map((k) => { const on = V.sort.key === k; return item(SORTS[k][0] + (on ? (V.sort.dir === 'asc' ? '  ↑' : '  ↓') : ''), on, () => setSort(k, on ? (V.sort.dir === 'asc' ? 'desc' : 'asc') : V.sort.dir)); }),
    h('hr'), item('Ascending (A to Z, oldest first)', V.sort.dir === 'asc', () => setSort(V.sort.key, 'asc')),
    item('Descending (Z to A, newest first)', V.sort.dir === 'desc', () => setSort(V.sort.key, 'desc')),
    h('hr'), item('Reset column widths', false, resetCols));
  hideCtx(); m.hidden = false;
  const r = $('btnSort').getBoundingClientRect();
  m.style.left = Math.max(8, Math.min(r.right - m.offsetWidth, innerWidth - m.offsetWidth - 8)) + 'px'; m.style.top = r.bottom + 4 + 'px';
});

// ---------- resizable columns ----------
const COLS = { name: { min: 120 }, prog: { min: 100, def: 210 }, speed: { min: 60, def: 84 }, eta: { min: 50, def: 64 }, status: { min: 70, def: 108 } };
const colW = loadJSON('downloadan.cols', {});
for (const k of Object.keys(colW)) if (!COLS[k] || typeof colW[k] !== 'number' || !Number.isFinite(colW[k])) delete colW[k];
const colPx = (k) => Math.max(COLS[k].min, Math.min(900, colW[k] ?? COLS[k].def));
function applyCols() {
  const auto = colW.name == null;
  const gap = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--gap')) || 12;
  const nameTrack = auto ? 'minmax(160px, 1fr)' : colPx('name') + 'px';
  const tracks = ['38px', nameTrack, colPx('prog') + 'px', colPx('speed') + 'px', colPx('eta') + 'px', colPx('status') + 'px'];
  if (!auto) tracks.push('1fr');                       // keeps the row full-width when Name has a fixed size
  const min = 38 + (auto ? 160 : colPx('name')) + colPx('prog') + colPx('speed') + colPx('eta') + colPx('status') + gap * 5 + 16;
  const t = $('table'); t.style.setProperty('--cols', tracks.join(' ')); t.style.setProperty('--table-min', min + 'px');
}
function resetCols() { for (const k of Object.keys(colW)) delete colW[k]; saveJSON('downloadan.cols', colW); applyCols(); }
$('cols').addEventListener('pointerdown', (e) => {
  const g = e.target.closest('.grip'); if (!g || e.button !== 0) return;
  e.preventDefault(); e.stopPropagation();
  const col = g.parentElement.dataset.col, startX = e.clientX, startW = g.parentElement.getBoundingClientRect().width;
  g.setPointerCapture(e.pointerId); g.classList.add('drag'); document.body.style.cursor = 'col-resize';
  const move = (ev) => { colW[col] = Math.round(Math.max(COLS[col].min, Math.min(900, startW + ev.clientX - startX))); applyCols(); };
  const up = () => {
    g.removeEventListener('pointermove', move); g.removeEventListener('pointerup', up); g.removeEventListener('pointercancel', up);
    g.classList.remove('drag'); document.body.style.cursor = ''; saveJSON('downloadan.cols', colW);
  };
  g.addEventListener('pointermove', move); g.addEventListener('pointerup', up); g.addEventListener('pointercancel', up);
});
$('cols').addEventListener('dblclick', (e) => {
  const g = e.target.closest('.grip'); if (!g) return;
  delete colW[g.parentElement.dataset.col]; saveJSON('downloadan.cols', colW); applyCols();
});
window.addEventListener('resize', applyCols);
applyCols(); updateSortUI();

// ---------- selection & toolbar ----------
$('list').addEventListener('click', (e) => {
  const row = e.target.closest('.row'); if (!row) { S.sel.clear(); render(); return; }
  const id = row.dataset.id;
  if (e.shiftKey && V.lastClicked) {
    const ids = shown.map((d) => d.id); const a = ids.indexOf(V.lastClicked); const b = ids.indexOf(id);
    if (a >= 0 && b >= 0) { S.sel.clear(); ids.slice(Math.min(a, b), Math.max(a, b) + 1).forEach((i) => S.sel.add(i)); }
  } else if (e.ctrlKey || e.metaKey) { S.sel.has(id) ? S.sel.delete(id) : S.sel.add(id); }
  else { S.sel.clear(); S.sel.add(id); }
  V.lastClicked = id; render();
});
$('list').addEventListener('dblclick', (e) => { const row = e.target.closest('.row'); if (row) api.openFile(row.dataset.id).then((m) => { if (m) toast(m === 'File not found' ? 'File not found' : 'Could not open file'); }); });
const selIds = () => [...S.sel].filter((i) => S.downloads.has(i));
// One "Remove" action everywhere (toolbar, Delete key, right-click). It always asks first.
function removeSelected() {
  const items = selIds().map((i) => S.downloads.get(i)).filter(Boolean);
  if (items.length) openRemove(items);
}
let rmItems = [];
function openRemove(items) {
  rmItems = items;
  const done = items.filter((d) => d.status === 'completed'), open = items.length - done.length;
  setText($('rmTitle'), items.length === 1 ? 'Remove download' : 'Remove ' + items.length + ' downloads');
  setText($('rmText'), items.length === 1 ? 'Remove this download from the list?' : 'Remove these downloads from the list?');
  const shownNames = items.slice(0, 5).map((d) => h('li', { textContent: nameOf(d) }));
  if (items.length > 5) shownNames.push(h('li', { textContent: 'and ' + (items.length - 5) + ' more…' }));
  $('rmList').replaceChildren(...shownNames);
  $('rmDelete').checked = false;                                    // never ticked by default
  $('rmDeleteRow').hidden = $('rmHint').hidden = done.length === 0;
  setText($('rmDeleteLabel'), done.length === 1 ? 'Also delete the file from disk' : 'Also delete the ' + done.length + ' downloaded files from disk');
  setText($('rmNote'), open ? (open === 1 ? '1 unfinished download' : open + ' unfinished downloads') + ' will be cancelled and its partly downloaded data discarded.' : '');
  $('dlgRemove').showModal(); $('rmOk').focus();
}
$('rmCancel').onclick = () => $('dlgRemove').close();
$('rmOk').onclick = async () => {
  const ids = rmItems.map((d) => d.id), del = !$('rmDeleteRow').hidden && $('rmDelete').checked;
  $('dlgRemove').close(); S.sel.clear();
  const r = await api.remove(ids, del); const pl = (n, w) => n + ' ' + w + (n === 1 ? '' : 's');
  let msg = 'Removed ' + pl(r.removed, 'download');
  if (r.deleted) msg += ' and moved ' + pl(r.deleted, 'file') + ' to the Recycle Bin';
  if (r.failed) msg += (r.deleted ? '. ' : ' but ') + 'could not delete ' + pl(r.failed, 'file');
  toast(msg);
};
$('btnResume').onclick = () => api.resume(selIds());
$('btnPause').onclick = () => api.pause(selIds());
$('btnRemove').onclick = () => removeSelected();
$('btnClear').onclick = () => { const n = [...S.downloads.values()].filter((d) => d.status === 'completed').length; if (!n) return; api.clearCompleted(); toast('Cleared ' + n + ' completed download' + (n === 1 ? '' : 's') + ' (files kept)'); };
$('btnAdd').onclick = () => openAdd({ fromClipboard: true });
$('parMinus').onclick = () => api.setSettings({ maxSimultaneous: Math.max(1, S.settings.maxSimultaneous - 1) });
$('parPlus').onclick = () => api.setSettings({ maxSimultaneous: Math.min(10, S.settings.maxSimultaneous + 1) });
$('search').addEventListener('input', (e) => { V.search = e.target.value; render(); });
$('tabs').addEventListener('click', (e) => {
  const b = e.target.closest('button[data-f]'); if (!b) return;
  V.filter = b.dataset.f; [...$('tabs').querySelectorAll('button[data-f]')].forEach((x) => x.classList.toggle('active', x === b)); render();
});
document.addEventListener('keydown', (e) => {
  if (document.querySelector('dialog[open]')) return;
  if (e.ctrlKey && e.key.toLowerCase() === 'n') { e.preventDefault(); openAdd({ fromClipboard: true }); }
  else if (e.ctrlKey && e.key.toLowerCase() === 'a' && e.target.tagName !== 'INPUT') { e.preventDefault(); shown.forEach((d) => S.sel.add(d.id)); render(); }
  else if (e.key === 'Delete' && e.target.tagName !== 'INPUT') removeSelected();
  else if (e.key === 'Escape') hideCtx();
});

// ---------- context menu ----------
const ctx = $('ctx');
function hideCtx() { ctx.hidden = true; $('sortMenu').hidden = true; }
document.addEventListener('click', hideCtx); window.addEventListener('blur', hideCtx);
const ctxItem = (label, fn) => h('button', { type: 'button', textContent: label, on: { click: () => { hideCtx(); fn(); } } });
function showCtx(e, kids) {
  e.preventDefault();
  $('sortMenu').hidden = true;
  ctx.replaceChildren(...kids); ctx.hidden = false;
  ctx.style.left = Math.min(e.clientX, innerWidth - ctx.offsetWidth - 8) + 'px';
  ctx.style.top = Math.min(e.clientY, innerHeight - ctx.offsetHeight - 8) + 'px';
}
$('list').addEventListener('contextmenu', (e) => {
  const row = e.target.closest('.row'); if (!row) return; e.preventDefault();
  if (!S.sel.has(row.dataset.id)) { S.sel.clear(); S.sel.add(row.dataset.id); render(); }
  const sel = selIds().map((i) => S.downloads.get(i)); const one = sel.length === 1 ? sel[0] : null;
  const item = ctxItem;
  const kids = [];
  if (one && one.status === 'completed') kids.push(item('Open', () => api.openFile(one.id)));
  if (one) kids.push(item('Show in folder', () => api.showInFolder(one.id)));
  if (sel.some((d) => d.status === 'paused' || d.status === 'error')) kids.push(item('Resume', () => api.resume(selIds())));
  if (sel.some((d) => d.status === 'downloading' || d.status === 'queued')) kids.push(item('Pause', () => api.pause(selIds())));
  if (sel.some((d) => d.status !== 'completed')) {
    kids.push(h('hr'), h('div', { class: 'hd', textContent: 'Move to queue' }));
    S.queues.forEach((q) => kids.push(item(q.name, () => api.move(selIds(), q.id))));
  }
  if (one && ['error', 'paused', 'queued'].includes(one.status)) kids.push(item('Refresh download address…', () => openRefresh(one)));
  kids.push(h('hr'));
  if (one) kids.push(item('Copy link', () => { api.writeClipboard(one.url); toast('Link copied'); }));
  kids.push(item('Remove…', () => removeSelected()));
  showCtx(e, kids);
});

// right-click on a queue: start / stop it, and rename or delete the ones you made (Main can't be renamed or deleted)
$('queueList').addEventListener('contextmenu', (e) => {
  const el = e.target.closest('.q-item');
  const q = el && S.queues.find((x) => x.id === el.dataset.q);
  if (!q) return;                                         // "All downloads" is a view, not a queue: no menu
  const kids = [ctxItem('Start queue', () => { api.startQueue(q.id); toast('Queue started'); })];
  if (q.enabled) kids.push(ctxItem('Stop queue', () => api.enableQueue(q.id, false)));
  if (q.id !== 'main') {
    kids.push(h('hr'), ctxItem('Rename…', () => openQueueDialog(q.id)),
      ctxItem('Delete queue…', () => { if (confirm('Delete this queue? Its downloads move to Main.')) api.deleteQueue(q.id); }));
  }
  showCtx(e, kids);
});

// right-click on the empty part of the list
function emptyMenu(e) {
  if (e.defaultPrevented || e.target.closest('#cols')) return;
  const kids = [ctxItem('Add URL…', () => openAdd({ fromClipboard: true }))];
  if (shown.length) kids.push(ctxItem('Select all', () => { shown.forEach((d) => S.sel.add(d.id)); render(); }));
  showCtx(e, kids);
}
$('table').addEventListener('contextmenu', emptyMenu);
$('empty').addEventListener('contextmenu', emptyMenu);

// ---------- refresh download address ----------
let rf = null;                                             // set while a refresh is waiting
const rfStage = (s) => { $('rfWait').hidden = s !== 'wait'; $('rfActsWait').hidden = s !== 'wait'; $('rfConfirm').hidden = s !== 'confirm'; $('rfActsConfirm').hidden = s !== 'confirm'; };
const rfMsg = (t, bad) => { setText($('rfMsg'), t); $('rfMsg').style.color = bad ? 'var(--danger)' : ''; };
function closeRefresh() { rf = null; if ($('dlgRefresh').open) $('dlgRefresh').close(); }
async function openRefresh(d) {
  const r = await api.refreshStart(d.id);
  if (!r.ok) { toast(r.error); return; }
  rf = { id: d.id };
  setText($('rfName'), r.name); setText($('rfDone'), r.size ? fmtBytes(r.downloaded) + ' of ' + fmtBytes(r.size) : 'Not started yet');
  $('rfUrl').value = ''; rfMsg('Waiting for a download from your browser…'); rfStage('wait'); $('dlgRefresh').showModal();
}
const rfDesc = (x) => x.name + '  ·  ' + (x.size ? fmtBytes(x.size) : 'size unknown');
api.on('refresh:event', (ev) => {
  if (!rf) return;
  if (ev.type === 'checking') rfMsg('Checking the new link…');
  else if (ev.type === 'error') { rfStage('wait'); rfMsg(ev.message, true); }
  else if (ev.type === 'waiting') { rfStage('wait'); rfMsg('Waiting for a download from your browser…'); }
  else if (ev.type === 'timeout') { closeRefresh(); toast('Stopped waiting for a new link'); }
  else if (ev.type === 'done') { closeRefresh(); toast(ev.restarted ? 'New link accepted: starting the download again' : 'Link refreshed: resuming'); }
  else if (ev.type === 'confirm') {
    $('rfReasons').replaceChildren(...ev.reasons.map((t) => h('li', { textContent: t })));
    setText($('rfExp'), rfDesc(ev.expected)); setText($('rfGot'), rfDesc(ev.got) + (ev.got.resumable ? '' : '  ·  cannot resume'));
    setText($('rfWarn'), ev.discards > 0 ? 'Replace and restart throws away the ' + fmtBytes(ev.discards) + ' already downloaded.' : '');
    rfStage('confirm');
  }
});
$('rfUse').onclick = async () => { const r = await api.refreshUseUrl($('rfUrl').value); if (!r.ok) rfMsg(r.error, true); };
$('rfUrl').addEventListener('keydown', (e) => { if (e.key === 'Enter') $('rfUse').click(); });
$('rfCancel').onclick = () => $('dlgRefresh').close();
$('rfKeep').onclick = () => api.refreshDecide('keep');
$('rfNew').onclick = async () => { await api.refreshDecide('new'); closeRefresh(); };
$('rfReplace').onclick = () => api.refreshDecide('replace');
$('dlgRefresh').addEventListener('close', () => { if (rf) { api.refreshCancel(); rf = null; } });   // Esc / Cancel stops waiting

// ---------- updates ----------
let U = { state: 'idle', current: '' };
const fmtTime = (t) => new Date(t).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
function renderUpdate() {
  const s = U.state;
  setText($('updVer'), U.current ? 'Version ' + U.current : '');
  const msg = { disabled: U.message, idle: '', checking: 'Checking for updates…', none: "You're up to date." + (U.lastChecked ? ' (checked ' + fmtTime(U.lastChecked) + ')' : ''),
    available: 'Version ' + U.version + ' is available.', downloading: 'Downloading version ' + U.version + '… ' + U.percent + '%',
    ready: 'Version ' + U.version + ' is ready. Restart DownloaDan to install it; unfinished downloads resume automatically.', error: U.message }[s] || '';
  setText($('updMsg'), msg); $('updMsg').style.color = s === 'error' ? 'var(--danger)' : '';
  $('updCheck').disabled = ['checking', 'downloading', 'ready', 'disabled'].includes(s);
  const act = $('updAction'); act.hidden = !(s === 'available' || s === 'ready'); setText(act, s === 'ready' ? 'Restart and install' : 'Download update');
  const bar = $('updateBar'); bar.hidden = !(s === 'available' || s === 'downloading' || s === 'ready');
  setText($('updBarText'), s === 'available' ? 'Update ' + U.version + ' is available' : s === 'downloading' ? 'Downloading update… ' + U.percent + '%' : 'Update ' + U.version + ' is ready');
  const b = $('updBarBtn'); b.hidden = s === 'downloading'; setText(b, s === 'ready' ? 'Restart and install' : 'Download');
}
function updateAction() {
  if (U.state === 'available') api.updateDownload();
  else if (U.state === 'ready') {
    const n = [...S.downloads.values()].filter((d) => d.status === 'downloading').length;
    if (n && !confirm(n + ' download' + (n === 1 ? ' is' : 's are') + ' running. They will pause and resume automatically after the update. Restart now?')) return;
    api.updateInstall();
  }
}
api.on('update:state', (s) => { U = s; renderUpdate(); });
$('updCheck').onclick = async () => { U = { ...U, state: 'checking' }; renderUpdate(); U = await api.updateCheck(); renderUpdate(); };
$('updAction').onclick = updateAction; $('updBarBtn').onclick = updateAction;
$('setAutoUpdate').onchange = (e) => api.setSettings({ autoUpdate: e.target.checked });

// ---------- queues ----------
let queueEditId = null;
function openQueueDialog(id) {
  queueEditId = id; $('queueTitle').textContent = id ? 'Rename queue' : 'New queue';
  $('queueName').value = id ? queueName(id) : ''; $('queueErr').hidden = true; $('dlgQueue').showModal(); $('queueName').select();
}
async function saveQueue() {
  const name = $('queueName').value;
  const r = queueEditId ? await api.renameQueue(queueEditId, name) : await api.createQueue(name);
  if (!r.ok) { $('queueErr').textContent = r.error; $('queueErr').hidden = false; return; }
  if (!queueEditId) V.queue = r.id;
  $('dlgQueue').close(); render();
}
$('btnNewQueue').onclick = () => openQueueDialog(null);
$('queueOk').onclick = saveQueue; $('queueCancel').onclick = () => $('dlgQueue').close();
$('queueName').addEventListener('keydown', (e) => { if (e.key === 'Enter') saveQueue(); });
$('queueList').addEventListener('click', (e) => {
  const item = e.target.closest('.q-item'); if (!item) return;
  const id = item.dataset.q; const act = e.target.dataset.act;
  if (act === 'start') { api.startQueue(id); toast('Queue started'); return; }
  if (act === 'rename') return id === 'main' ? undefined : openQueueDialog(id);
  if (act === 'delete') { if (confirm('Delete this queue? Its downloads move to Main.')) api.deleteQueue(id); return; }
  if (act === 'toggle') { const q = S.queues.find((x) => x.id === id); api.enableQueue(id, !q.enabled); return; }
  V.queue = id; render();
});

// ---------- add dialog ----------
async function openAdd({ urls, referer, cookie, fromClipboard } = {}) {
  const dlg = $('dlgAdd');
  if (dlg.open) { if (urls) $('addUrls').value = ($('addUrls').value.trim() + '\n' + urls.join('\n')).trim(); return; }
  V.cookie = cookie;
  let text = (urls || []).join('\n');
  if (!text && fromClipboard) {
    try { const c = String((await api.readClipboard()) || '').trim(); if (/^https?:\/\/\S+$/i.test(c)) text = c; } catch { /* clipboard unavailable: just open empty */ }
  }
  $('addUrls').value = text; $('addName').value = ''; $('addRef').value = referer || ''; $('addErr').hidden = true;
  $('addDir').value = S.settings.downloadDir;
  $('addQueue').replaceChildren(...S.queues.map((q) => h('option', { value: q.id, textContent: q.name })));
  $('addQueue').value = V.queue !== 'all' ? V.queue : 'main';
  dlg.querySelector('details').open = !!referer;
  dlg.showModal(); $('addUrls').focus();
}
async function submitAdd(start) {
  const urls = $('addUrls').value.split(/\s+/).filter(Boolean);
  if (!urls.length) { $('addErr').textContent = 'Paste at least one link.'; $('addErr').hidden = false; return; }
  const r = await api.add({ urls, queueId: $('addQueue').value, dir: $('addDir').value, filename: $('addName').value.trim() || undefined, referer: $('addRef').value.trim() || undefined, cookie: V.cookie, start });
  if (!r.added.length) { $('addErr').textContent = 'Only http:// and https:// links are supported.'; $('addErr').hidden = false; return; }
  V.cookie = undefined; $('dlgAdd').close();
  if (r.rejected.length) toast(r.rejected.length + ' invalid link(s) skipped');
}
$('addNow').onclick = () => submitAdd(true); $('addQueued').onclick = () => submitAdd(false);
$('addCancel').onclick = () => $('dlgAdd').close();
$('addBrowse').onclick = async () => { const d = await api.chooseDir(); if (d) $('addDir').value = d; };
$('addUrls').addEventListener('input', () => { const multi = $('addUrls').value.split(/\s+/).filter(Boolean).length > 1; $('addName').disabled = multi; if (multi) $('addName').value = ''; });

// ---------- settings ----------
function fillSettings() {
  const s = S.settings;
  const kb = s.speedLimitKbps || 1024, inMb = kb >= 1024 && kb % 1024 === 0;
  if (document.activeElement !== $('setLimit')) { $('setLimit').value = inMb ? kb / 1024 : kb; $('setLimitUnit').value = inMb ? '1024' : '1'; }
  $('setLimitOn').checked = !!s.speedLimitOn;
  $('setConn').value = s.connections; $('setDir').value = s.downloadDir; $('setTray').checked = s.minimizeToTray;
  $('setStartup').checked = s.runAtStartup; $('setNotify').checked = s.notify; $('setPrune').checked = s.removeMissing; $('setAutoUpdate').checked = s.autoUpdate; renderKey(); $('setToken').value = S.token;
  if (document.activeElement !== $('setPort')) $('setPort').value = s.bridgePort;
  $('bridgeState').textContent = S.bridge.ok ? 'Browser integration is running on 127.0.0.1:' + (S.bridge.port || s.bridgePort) + ' (local only).' : 'Browser integration is not running: ' + (S.bridge.error || 'unknown error');
}
$('btnSettings').onclick = () => { fillSettings(); renderThemeGrid(); $('dlgSettings').showModal(); };
$('setClose').onclick = () => $('dlgSettings').close();
$('setTabs').addEventListener('click', (e) => {
  const b = e.target.closest('button[data-p]'); if (!b) return;
  [...$('setTabs').children].forEach((x) => x.classList.toggle('active', x === b));
  document.querySelectorAll('#dlgSettings [data-panel]').forEach((p) => { p.hidden = p.dataset.panel !== b.dataset.p; });
});
const applyLimit = () => api.setSettings({ speedLimitKbps: Math.max(1, Math.round(+$('setLimit').value || 1)) * +$('setLimitUnit').value });
$('setLimit').onchange = applyLimit; $('setLimitUnit').onchange = applyLimit;
$('setLimitOn').onchange = (e) => api.setSettings({ speedLimitOn: e.target.checked });
$('stLimit').onclick = () => api.setSettings({ speedLimitOn: !S.settings.speedLimitOn });
$('setConn').onchange = (e) => api.setSettings({ connections: +e.target.value });
$('setPort').onchange = (e) => api.setSettings({ bridgePort: +e.target.value });
$('setTray').onchange = (e) => api.setSettings({ minimizeToTray: e.target.checked });
$('setStartup').onchange = (e) => api.setSettings({ runAtStartup: e.target.checked });
$('setNotify').onchange = (e) => api.setSettings({ notify: e.target.checked });
$('setPrune').onchange = (e) => api.setSettings({ removeMissing: e.target.checked });
$('setBrowse').onclick = async () => { const d = await api.chooseDir(); if (d) api.setSettings({ downloadDir: d }); };
$('tokCopy').onclick = () => { api.writeClipboard(S.token); toast('Token copied'); };
$('tokRegen').onclick = async () => { if (confirm('Generate a new token? Browsers paired with the old one will stop working until you paste the new one.')) { await api.regenToken(); toast('New token created'); } };

// ---------- bypass key ----------
const prettyKey = (k) => ({ '': 'Off', Control: 'Ctrl', Meta: 'Windows key', ScrollLock: 'Scroll Lock', Pause: 'Pause / Break', PrintScreen: 'Print Screen', CapsLock: 'Caps Lock', NumLock: 'Num Lock' }[k] || k.replace(/^Key([A-Z])$/, '$1').replace(/^Digit(\d)$/, '$1'));
let capturing = false;
function renderKey() {
  const k = S.settings.bypassKey ?? 'F8';
  setText($('keyBtn'), capturing ? 'Press a key…  (Esc to cancel)' : prettyKey(k));
  $('keyBtn').classList.toggle('active', capturing);
  setText($('keyHint'), k ? 'While you hold ' + prettyKey(k) + ' and click a download, DownloaDan stays out of the way and your browser downloads it. Takes effect when you click back into a web page.' : 'Off: DownloaDan catches every download.');
}
function stopCapture() { capturing = false; document.removeEventListener('keydown', onCaptureKey, true); renderKey(); }
function onCaptureKey(e) {
  e.preventDefault(); e.stopPropagation();
  if (e.repeat) return;
  if (e.key === 'Escape') { stopCapture(); return; }
  const id = ['Alt', 'Control', 'Shift', 'Meta'].includes(e.key) ? e.key : e.code;
  stopCapture(); api.setSettings({ bypassKey: id });
}
$('keyBtn').onclick = () => { if (capturing) return stopCapture(); capturing = true; document.addEventListener('keydown', onCaptureKey, true); renderKey(); };
$('keyReset').onclick = () => { if (capturing) stopCapture(); api.setSettings({ bypassKey: 'F8' }); };
$('keyOff').onclick = () => { if (capturing) stopCapture(); api.setSettings({ bypassKey: '' }); };
window.addEventListener('blur', () => { if (capturing) stopCapture(); });
$('dlgSettings').addEventListener('close', () => { if (capturing) stopCapture(); });

// ---------- themes ----------
function renderThemeGrid() {
  const cards = allThemes().map((t) => {
    const custom = S.themes.custom.some((c) => c.id === t.id);
    const btn = (label, fn) => h('button', { type: 'button', textContent: label, on: { click: (e) => { e.stopPropagation(); fn(); } } });
    const sw = h('div', { class: 'sw' }, ...['bg', 'surface2', 'accent', 'text', 'success'].map((k) => { const i = h('i'); i.style.background = t.colors[k]; return i; }));
    return h('div', { class: 'theme-card' + (S.themes.active === t.id ? ' active' : ''), on: { click: () => api.applyTheme(t.id) } }, sw,
      h('div', { class: 'tn', textContent: t.name }), h('div', { class: 'ta', textContent: t.author ? 'by ' + t.author : 'custom' }),
      h('div', { class: 'tb' }, custom ? btn('Edit', () => openThemeEditor(t, true)) : null, btn('Duplicate', () => openThemeEditor(t, false)), btn('Export', async () => { const r = await api.exportTheme(t.id); if (r.ok) toast('Theme exported'); else if (!r.cancelled) toast(r.error); }), custom ? btn('Delete', () => { if (confirm('Delete this theme?')) api.deleteTheme(t.id); }) : null));
  });
  $('themeGrid').replaceChildren(...cards);
}
$('themeImport').onclick = async () => { const r = await api.importTheme(); if (r.ok) toast('Imported "' + r.name + '"'); else if (!r.cancelled) toast('Import failed: ' + r.error); };

let editing = null;
function readEditor() {
  const colors = {}; document.querySelectorAll('#thColors input').forEach((i) => { colors[i.dataset.k] = i.value; });
  return { name: $('thName').value, author: $('thAuthor').value || undefined, colors, shape: { radius: +$('thRadius').value, density: $('thDensity').value }, font: $('thFont').value };
}
let vTimer = 0;
function onEditorChange() {
  const t = readEditor(); $('thRadiusVal').textContent = t.shape.radius + 'px';
  applyThemeObj(t);                                       // live preview (colors are hex-only by construction of <input type=color>)
  clearTimeout(vTimer);
  vTimer = setTimeout(async () => { const r = await api.validateTheme(t); $('thErr').hidden = r.ok; $('thErr').textContent = r.error || ''; $('thSave').disabled = !r.ok; }, 80);
}
function openThemeEditor(t, edit) {
  editing = { id: edit ? t.id : null, saved: false };
  $('themeTitle').textContent = edit ? 'Edit theme' : 'New theme';
  $('thName').value = edit ? t.name : t.name + ' copy'; $('thAuthor').value = t.author && edit ? t.author : '';
  $('thRadius').value = t.shape.radius; $('thDensity').value = t.shape.density; $('thFont').value = t.font;
  $('thColors').replaceChildren(...Object.keys(COLOR_LABELS).map((k) => h('label', {}, h('input', { type: 'color', value: t.colors[k], 'data-k': k, on: { input: onEditorChange } }), COLOR_LABELS[k])));
  $('dlgTheme').showModal(); onEditorChange();
}
['thName', 'thAuthor', 'thRadius', 'thDensity', 'thFont'].forEach((i) => $(i).addEventListener('input', onEditorChange));
$('themeNew').onclick = () => openThemeEditor(allThemes().find((t) => t.id === S.themes.active) || S.builtin[0], false);
$('thCancel').onclick = () => $('dlgTheme').close();
$('thSave').onclick = async () => {
  const r = await api.saveTheme({ id: editing.id, ...readEditor() });
  if (!r.ok) { $('thErr').textContent = r.error; $('thErr').hidden = false; return; }
  editing.saved = true; $('dlgTheme').close();
};
$('dlgTheme').addEventListener('close', () => { applyActiveTheme(); });   // drop any unsaved preview

// ---------- boot ----------
(async function init() {
  const st = await api.getState();
  S.builtin = st.builtinThemes; S.bridge = st.bridge;
  for (const d of st.downloads) S.downloads.set(d.id, d);
  loadMeta(st);
  api.updateGet().then((u) => { U = u; renderUpdate(); });
  if (st.pending.length) openAdd({ urls: st.pending.map((p) => p.url), referer: st.pending[0].referer, cookie: st.pending[0].cookie });
})();
