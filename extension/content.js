'use strict';
// Tells the extension when the bypass key is held, so a browser download can skip DownloaDan on demand.
(() => {
  const ext = typeof browser !== 'undefined' ? browser : chrome;
  if (window.top !== window && window.innerWidth < 200) return;

  // ---- bypass key: hold it while clicking a download so the browser handles it instead of DownloaDan ----
  let bypassKey = 'F8', keyHeld = false;
  const MODS = { Alt: 'altKey', Control: 'ctrlKey', Shift: 'shiftKey', Meta: 'metaKey' };
  const keyId = (e) => (MODS[e.key] ? e.key : e.code);                       // modifiers by name, other keys by physical key
  const isHeld = (e) => !!bypassKey && (MODS[bypassKey] ? !!e[MODS[bypassKey]] : keyHeld);
  const tell = (held, click) => { try { ext.runtime.sendMessage({ type: 'bypass', held, click }).catch(() => {}); } catch { /* extension reloaded */ } };
  try {
    ext.storage.local.get('bypassKey').then((r) => { if (typeof r.bypassKey === 'string') bypassKey = r.bypassKey; }).catch(() => {});
    ext.storage.onChanged.addListener((ch, area) => { if (area === 'local' && ch.bypassKey) { bypassKey = typeof ch.bypassKey.newValue === 'string' ? ch.bypassKey.newValue : 'F8'; if (!bypassKey) keyHeld = false; } });
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
  addEventListener('focus', () => { try { ext.runtime.sendMessage({ type: 'refreshConfig' }).catch(() => {}); } catch { /* extension reloaded */ } });
})();
