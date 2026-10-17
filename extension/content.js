'use strict';
// Adds a small download button to the upper-right corner of <video> elements (shown on hover).
(() => {
  if (window.top !== window && window.innerWidth < 200) return;
  const MIN_W = 160, MIN_H = 90;
  const videos = new Set();
  let host = null, root = null, btn = null, note = null, menu = null, current = null, hideT = 0, raf = 0;

  const scan = () => document.querySelectorAll('video').forEach((v) => videos.add(v));
  let scanT = 0;
  new MutationObserver(() => { clearTimeout(scanT); scanT = setTimeout(scan, 600); }).observe(document.documentElement, { childList: true, subtree: true });
  scan();

  const CSS = `
    :host { all: initial }
    .wrap { position: fixed; z-index: 2147483647; display: none; font: 12px/1.3 system-ui, Segoe UI, sans-serif }
    .btn { all: unset; box-sizing: border-box; width: 34px; height: 34px; border-radius: 10px; cursor: pointer; display: grid; place-items: center;
      background: rgba(20,22,28,.82); color: #fff; box-shadow: 0 2px 10px rgba(0,0,0,.45); backdrop-filter: blur(6px); border: 1px solid rgba(255,255,255,.18) }
    .btn:hover { background: #4f8cff } .btn.ok { background: #1fa971 } .btn.err { background: #d93a4a }
    svg { width: 18px; height: 18px; fill: none; stroke: currentColor; stroke-width: 2.2; stroke-linecap: round; stroke-linejoin: round }
    .note { position: absolute; right: 40px; top: 4px; white-space: nowrap; background: rgba(20,22,28,.92); color: #fff; padding: 5px 9px; border-radius: 8px; display: none }
    .menu { position: absolute; right: 0; top: 40px; min-width: 240px; max-width: 360px; background: rgba(20,22,28,.97); color: #fff; border-radius: 10px; padding: 4px; display: none; box-shadow: 0 8px 24px rgba(0,0,0,.5) }
    .menu button { all: unset; box-sizing: border-box; display: block; width: 100%; padding: 7px 10px; border-radius: 7px; cursor: pointer; overflow: hidden; text-overflow: ellipsis; white-space: nowrap }
    .menu button:hover { background: #4f8cff }
    .menu .hd { padding: 6px 10px 3px; opacity: .65; font-size: 11px }`;

  function ensureUI() {
    if (host) return;
    host = document.createElement('downloadan-overlay');
    root = host.attachShadow({ mode: 'closed' });
    const sheet = new CSSStyleSheet(); sheet.replaceSync(CSS); root.adoptedStyleSheets = [sheet];   // not subject to page CSP
    const wrap = document.createElement('div'); wrap.className = 'wrap';
    btn = document.createElement('button'); btn.className = 'btn'; btn.title = 'Download with DownloaDan';
    const NS = 'http://www.w3.org/2000/svg';
    const svg = document.createElementNS(NS, 'svg'); svg.setAttribute('viewBox', '0 0 24 24');
    const p = document.createElementNS(NS, 'path'); p.setAttribute('d', 'M12 4v11m0 0l-4.5-4.5M12 15l4.5-4.5M5 20h14'); svg.append(p); btn.append(svg);
    note = document.createElement('div'); note.className = 'note';
    menu = document.createElement('div'); menu.className = 'menu';
    wrap.append(btn, note, menu); root.append(wrap);
    host._wrap = wrap;
    btn.addEventListener('click', (e) => { e.stopPropagation(); e.preventDefault(); if (e.isTrusted) onClick(); });
    for (const t of ['mousedown', 'mouseup', 'pointerdown', 'dblclick']) wrap.addEventListener(t, (e) => e.stopPropagation());
    host.addEventListener('mouseenter', () => { clearTimeout(hideT); hideT = 0; });
    document.addEventListener('fullscreenchange', attach);
    attach();
  }
  function attach() { (document.fullscreenElement && document.fullscreenElement.tagName !== 'VIDEO' ? document.fullscreenElement : document.documentElement).append(host); }

  function place(v) {
    const r = v.getBoundingClientRect();
    const right = Math.min(r.right, innerWidth), top = Math.max(r.top, 0);
    const w = host._wrap; w.style.display = 'block';
    w.style.left = Math.max(0, right - 34 - 10) + 'px'; w.style.top = Math.min(innerHeight - 40, top + 10) + 'px';
  }
  function show(v) { ensureUI(); clearTimeout(hideT); hideT = 0; if (current !== v) { closeMenu(); note.style.display = 'none'; } current = v; place(v); }
  function hide() { if (host) host._wrap.style.display = 'none'; current = null; closeMenu(); }
  function closeMenu() { if (menu) menu.style.display = 'none'; }

  function hit(x, y) {
    let found = null;
    for (const v of videos) {
      if (!v.isConnected) { videos.delete(v); continue; }
      const r = v.getBoundingClientRect();
      if (r.width < MIN_W || r.height < MIN_H || x < r.left || x > r.right || y < r.top || y > r.bottom) continue;
      if (!found || r.width * r.height <= found.getBoundingClientRect().width * found.getBoundingClientRect().height) found = v;
    }
    if (found) show(found); else if (current && !hideT) hideT = setTimeout(() => { hideT = 0; hide(); }, 700);
  }
  addEventListener('mousemove', (e) => {
    if (e.target === host) { clearTimeout(hideT); hideT = 0; return; }
    if (raf) return; raf = requestAnimationFrame(() => { raf = 0; hit(e.clientX, e.clientY); });
  }, { passive: true, capture: true });
  const reposition = () => { if (current) place(current); };
  addEventListener('scroll', reposition, { passive: true, capture: true }); addEventListener('resize', reposition, { passive: true });

  // ---- bypass key: hold it while clicking a download so the browser handles it instead of DownloaDan ----
  let bypassKey = 'F8', keyHeld = false;
  const MODS = { Alt: 'altKey', Control: 'ctrlKey', Shift: 'shiftKey', Meta: 'metaKey' };
  const keyId = (e) => (MODS[e.key] ? e.key : e.code);                       // modifiers by name, other keys by physical key
  const isHeld = (e) => !!bypassKey && (MODS[bypassKey] ? !!e[MODS[bypassKey]] : keyHeld);
  const tell = (held, click) => { try { chrome.runtime.sendMessage({ type: 'bypass', held, click }).catch(() => {}); } catch { /* extension reloaded */ } };
  try {
    chrome.storage.local.get('bypassKey').then((r) => { if (typeof r.bypassKey === 'string') bypassKey = r.bypassKey; }).catch(() => {});
    chrome.storage.onChanged.addListener((ch, area) => { if (area === 'local' && ch.bypassKey) { bypassKey = typeof ch.bypassKey.newValue === 'string' ? ch.bypassKey.newValue : 'F8'; if (!bypassKey) keyHeld = false; } });
  } catch { /* extension reloaded */ }
  const opt = { capture: true, passive: true };
  addEventListener('keydown', (e) => {
    if (!bypassKey) return;
    if (keyId(e) === bypassKey) { if (!keyHeld) { keyHeld = true; tell(true, false); } }
    else if (isHeld(e)) tell(true, true);                                    // e.g. Enter on a link while the key is held
  }, opt);
  addEventListener('keyup', (e) => { if (keyId(e) === bypassKey && keyHeld) { keyHeld = false; tell(false, false); } }, opt);
  addEventListener('pointerdown', (e) => { if (isHeld(e)) tell(true, true); }, opt);
  const release = () => { if (keyHeld) { keyHeld = false; tell(false, false); } };
  addEventListener('blur', release); addEventListener('pagehide', release);
  addEventListener('focus', () => { try { chrome.runtime.sendMessage({ type: 'refreshConfig' }).catch(() => {}); } catch { /* extension reloaded */ } });

  // The file list also closes when you click anywhere else, or press Escape.
  addEventListener('pointerdown', (e) => { if (menuOpen() && e.target !== host) closeMenu(); }, { capture: true, passive: true });   // clicks inside our own overlay are retargeted to the host
  addEventListener('keydown', (e) => { if (e.key === 'Escape' && menuOpen()) closeMenu(); }, { capture: true, passive: true });

  const send = (m) => new Promise((res) => { try { chrome.runtime.sendMessage(m, (r) => res(r || { ok: false, error: 'offline' })); } catch { res({ ok: false, error: 'reload' }); } });
  const fmt = (n) => (n ? (n / 1048576 >= 1 ? (n / 1048576).toFixed(1) + ' MB' : Math.round(n / 1024) + ' KB') : '');
  function flash(kind, text) {
    btn.className = 'btn ' + kind; note.textContent = text; note.style.display = 'block';
    setTimeout(() => { btn.className = 'btn'; note.style.display = 'none'; }, 2600);
  }
  async function submit(url) {
    closeMenu();
    const r = await send({ type: 'add', url, referer: location.href });
    if (r.ok) return flash('ok', 'Sent to DownloaDan');
    const why = { 'not-paired': 'Not connected yet: click the DownloaDan extension icon', 'bad-token': 'Connection expired: click the DownloaDan extension icon to reconnect', offline: 'DownloaDan is not running', reload: 'Reload this page' }[r.error] || 'Could not reach DownloaDan';
    flash('err', why);
    if (r.error === 'not-paired' || r.error === 'bad-token') send({ type: 'options' });
  }
  const direct = (v) => {
    const out = [];
    const add = (u) => { try { const x = new URL(u, location.href); if (/^https?:$/.test(x.protocol) && !out.includes(x.href)) out.push(x.href); } catch { /* ignore */ } };
    if (v.currentSrc) add(v.currentSrc); if (v.src) add(v.src);
    v.querySelectorAll('source[src]').forEach((s) => add(s.src));
    return out;
  };
  let opening = false;
  const menuOpen = () => !!menu && menu.style.display === 'block';
  async function onClick() {
    if (menuOpen()) { closeMenu(); return; }          // clicking the icon again closes the list
    if (opening) return;                              // ignore double-clicks while the list is being prepared
    opening = true;
    try { await openList(); } finally { opening = false; }
  }
  async function openList() {
    const v = current; if (!v) return;
    const d = direct(v);
    if (d.length === 1) return submit(d[0]);
    const res = await send({ type: 'getMedia' });
    const sniffed = (res.items || []).slice().sort((a, b) => (b.size || 0) - (a.size || 0)).slice(0, 6);
    const list = d.length ? d.map((u) => ({ url: u })) : sniffed;
    if (!list.length) return flash('err', 'No downloadable file found (streaming/DRM video)');
    if (list.length === 1) return submit(list[0].url);
    menu.replaceChildren();
    const hd = document.createElement('div'); hd.className = 'hd'; hd.textContent = 'Choose a file'; menu.append(hd);
    for (const m of list) {
      const b = document.createElement('button');
      let nm = m.url; try { nm = decodeURIComponent(new URL(m.url).pathname.split('/').pop() || new URL(m.url).hostname); } catch { /* keep */ }
      b.textContent = [nm.slice(0, 48), fmt(m.size)].filter(Boolean).join('  ·  ');
      b.addEventListener('click', (e) => { e.stopPropagation(); if (e.isTrusted) submit(m.url); });
      menu.append(b);
    }
    menu.style.display = 'block';
  }
})();
