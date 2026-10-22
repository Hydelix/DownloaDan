'use strict';
/*
 * Connects the pages to the app's Rust backend. The pages only ever talk to window.dan (main window),
 * window.danPrompt ("New download" box) and window.danPair ("Connect browser" box). Each call below maps to one
 * backend command; the backend checks which window is asking and validates every argument, so the page itself is
 * treated as untrusted. No file system, no network, no raw IPC is exposed to page code.
 */
(function () {
  const T = window.__TAURI__;
  if (!T) { document.body.textContent = 'DownloaDan must be started as an app (npm run tauri dev).'; return; }
  const { invoke } = T.core;
  const { listen } = T.event;
  const win = T.window.getCurrentWindow();

  // name -> [backend command, ...argument names]
  const bind = (table) => {
    const out = {};
    for (const [name, [cmd, ...keys]] of Object.entries(table)) {
      out[name] = (...args) => invoke(cmd, Object.fromEntries(keys.map((k, i) => [k, args[i]])));
    }
    return out;
  };
  const events = (allowed, extra) => ({
    on: (channel, cb) => { if (allowed.has(channel)) listen(channel, (e) => cb(e.payload)); },
    ...extra,
  });

  const main = bind({
    getState: ['state_get'], readClipboard: ['clip_read'], writeClipboard: ['clip_write', 'text'],
    add: ['dl_add', 'p'], pause: ['dl_pause', 'ids'], pauseAll: ['dl_pause_all'], resume: ['dl_resume', 'ids'],
    remove: ['dl_remove', 'ids', 'del'], clearCompleted: ['dl_clear_completed'], move: ['dl_move', 'ids', 'q'],
    openFile: ['dl_open', 'id'], showInFolder: ['dl_show', 'id'],
    createQueue: ['q_create', 'name'], renameQueue: ['q_rename', 'id', 'name'], deleteQueue: ['q_delete', 'id'],
    enableQueue: ['q_enable', 'id', 'on'], startQueue: ['q_start', 'id'],
    updateGet: ['update_get'], updateCheck: ['update_check'], updateDownload: ['update_download'], updateInstall: ['update_install'],
    refreshStart: ['refresh_start', 'id'], refreshCancel: ['refresh_cancel'], refreshUseUrl: ['refresh_url', 'u'], refreshDecide: ['refresh_decide', 'choice'],
    setSettings: ['settings_set', 'p'], chooseDir: ['dir_choose'], regenToken: ['token_regen'],
    validateTheme: ['theme_validate', 't'], saveTheme: ['theme_save', 't'], applyTheme: ['theme_apply', 'tid'],
    deleteTheme: ['theme_delete', 'tid'], exportTheme: ['theme_export', 'tid'], importTheme: ['theme_import'],
  });
  const MAIN_EVENTS = new Set(['patch', 'meta', 'ui:prompt', 'bridge:status', 'refresh:event', 'notice', 'update:state']);
  window.dan = Object.freeze(events(MAIN_EVENTS, main));

  window.danPrompt = Object.freeze(events(new Set(), {
    ...bind({
      init: ['prompt_init'], probe: ['prompt_probe', 'pid'], free: ['prompt_free', 'dir'], chooseDir: ['prompt_dir', 'start'],
      confirm: ['prompt_confirm', 'p'], cancel: ['prompt_cancel', 'pid'],
    }),
    onNext: (cb) => listen('prompt:next', (e) => cb(e.payload)),
  }));
  window.danPair = Object.freeze(bind({ init: ['pair_init'], answer: ['pair_answer', 'allow'] }));

  // ---- custom title bar: drag to move, double-click to maximise, and the window buttons ----
  const isMain = /(^|\/)(index\.html)?$/.test(location.pathname);
  document.addEventListener('mousedown', (e) => {
    if (e.button !== 0) return;
    const bar = e.target.closest && e.target.closest('.titlebar');
    if (!bar || e.target.closest('input, button, select, textarea, a, [data-nodrag]')) return;
    if (e.detail === 2 && isMain) win.toggleMaximize();
    else win.startDragging();
  });
  // The browser's own right-click menu (Back, Reload, Save as, Print, developer tools) makes no sense in an app.
  // Only text fields keep theirs, so copy and paste still work; the app's own menus are built where they are needed.
  document.addEventListener('contextmenu', (e) => {
    if (!(e.target.closest && e.target.closest('input, textarea'))) e.preventDefault();
  });
  document.addEventListener('click', (e) => {
    const b = e.target.closest && e.target.closest('[data-wc]');
    if (!b) return;
    const act = b.dataset.wc;
    if (act === 'min') win.minimize();
    else if (act === 'max') win.toggleMaximize();
    else if (act === 'close') win.close();
  });
})();
