'use strict';
/* Permission box for "a browser extension wants to connect". All text is inserted with textContent. */
const api = window.danPair;
const $ = (id) => document.getElementById(id);
const COLORS = ['bg', 'surface', 'surface2', 'border', 'text', 'textMuted', 'accent', 'accentText', 'success', 'warning', 'danger'];
function applyTheme(t) {
  const r = document.documentElement;
  for (const k of COLORS) r.style.setProperty('--' + k, t.colors[k]);
  r.style.setProperty('--radius', t.shape.radius + 'px');
  r.dataset.density = t.shape.density; r.dataset.font = t.font;
}
let left = 0, timer = 0;
function tick() {
  $('pqLeft').textContent = left > 0 ? left + ' seconds' : 'Expired';
  if (left <= 0) { clearInterval(timer); $('pqAllow').disabled = true; }
  left -= 1;
}
$('pqAllow').onclick = () => { $('pqAllow').disabled = true; api.answer(true); };
$('pqDeny').onclick = () => api.answer(false);
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') $('pqDeny').click(); });
api.init().then((p) => {
  if (!p) { api.answer(false); return; }
  applyTheme(p.theme);
  $('pqBrowser').textContent = p.browser;
  $('pqExt').textContent = 'DownloaDan Integration (' + p.extensionId.slice(0, 8) + '…)';
  left = p.secondsLeft; tick(); timer = setInterval(tick, 1000);
  $('pqDeny').focus();                                   // Enter or a stray click never means "Allow"
});
