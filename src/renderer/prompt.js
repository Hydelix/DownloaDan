'use strict';
/* "New download" box. Untrusted text (names, URLs, server replies) only goes in via textContent/value. */
const api = window.danPrompt;
const $ = (id) => document.getElementById(id);
const COLORS = ['bg', 'surface', 'surface2', 'border', 'text', 'textMuted', 'accent', 'accentText', 'success', 'warning', 'danger'];
let cur = null, info = null, seq = 0;

const fmtBytes = (n) => { if (!n) return '0 B'; const u = ['B', 'KB', 'MB', 'GB', 'TB']; const i = Math.min(4, Math.floor(Math.log(n) / Math.log(1024))); return (n / 1024 ** i).toFixed(i ? 1 : 0) + ' ' + u[i]; };
function applyTheme(t) {
  const r = document.documentElement;
  for (const k of COLORS) r.style.setProperty('--' + k, t.colors[k]);
  r.style.setProperty('--radius', t.shape.radius + 'px');
  r.dataset.density = t.shape.density; r.dataset.font = t.font;
}
function nameFromUrl(u) { try { return decodeURIComponent(new URL(u).pathname.split('/').filter(Boolean).pop() || '') || 'download'; } catch { return 'download'; } }
function hostOf(u) { try { return new URL(u).hostname; } catch { return ''; } }

async function checkSpace() {
  const free = await api.free($('pDir').value);
  const need = info && info.size > 0 ? info.size : 0;
  const short = free != null && need > free;
  $('pSpace').textContent = free == null ? '' : 'Free space on that drive: ' + fmtBytes(free) + (short ? ' (not enough for this file)' : '');
  $('pSpace').style.color = short ? 'var(--danger)' : '';
}

async function show(init) {
  cur = init; info = null; const mine = ++seq;
  applyTheme(init.theme);
  $('pWait').textContent = init.waiting ? init.waiting + ' more waiting' : '';
  $('pName').value = nameFromUrl(init.url);
  $('pSize').textContent = 'Checking…'; $('pType').textContent = 'Checking…'; $('pResume').textContent = 'Checking…';
  $('pHost').textContent = hostOf(init.url); $('pHost').title = init.url;
  $('pDir').value = init.settings.downloadDir;
  $('pQueue').replaceChildren(...init.queues.map((q) => { const o = document.createElement('option'); o.value = q.id; o.textContent = q.name; return o; }));
  $('pQueue').value = 'main'; $('pStart').checked = true;
  $('pNote').hidden = true; $('pGo').disabled = false;
  $('pName').focus(); $('pName').select();
  checkSpace();
  const r = await api.probe(init.pid);
  if (mine !== seq) return;                       // a newer request replaced this one
  info = r;
  if (r.ok) {
    if (r.filename) $('pName').value = r.filename;
    $('pSize').textContent = r.size ? fmtBytes(r.size) : 'Unknown';
    $('pType').textContent = r.type || 'Unknown';
    $('pResume').textContent = r.resumable ? 'Yes (multi-connection)' : 'No (single connection)';
  } else {
    $('pSize').textContent = '—'; $('pType').textContent = '—'; $('pResume').textContent = '—';
    $('pNote').textContent = 'Could not read file info: ' + r.error + '. You can still try to download it.'; $('pNote').hidden = false;
  }
  checkSpace();
}

$('pBrowse').onclick = async () => { const d = await api.chooseDir($('pDir').value); if (d) { $('pDir').value = d; checkSpace(); } };
$('pGo').onclick = async () => {
  if (!cur) return;
  $('pGo').disabled = true;
  const r = await api.confirm({ pid: cur.pid, filename: $('pName').value.trim(), dir: $('pDir').value, queueId: $('pQueue').value, startQueue: $('pStart').checked });
  if (!r || !r.ok) { $('pNote').textContent = 'Could not add this download.'; $('pNote').hidden = false; $('pGo').disabled = false; }
};
$('pCancel').onclick = () => { if (cur) api.cancel(cur.pid); };
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') $('pCancel').click();
  else if (e.key === 'Enter' && e.target.tagName !== 'SELECT' && e.target.tagName !== 'BUTTON') $('pGo').click();
});
api.onNext(show);
api.init().then((p) => { if (p) show(p); });
