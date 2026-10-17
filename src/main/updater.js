'use strict';
const { EventEmitter } = require('events');

/*
 * Update checker. Wraps electron-updater (injected, so this file can be tested without Electron).
 * Flow: check -> "available" -> user clicks Download -> "downloading" -> "ready" -> user clicks Restart.
 * Nothing is downloaded or installed without the user's click, except that a downloaded update is
 * also installed the next time DownloaDan quits (autoInstallOnAppQuit).
 * electron-updater verifies the SHA-512 of the installer against the release's latest.yml.
 */
function friendly(e) {
  const msg = String((e && e.message) || e || '');
  if (/404|Cannot find latest|No published versions|latest\.yml/i.test(msg)) return 'No release found yet. Upload a release to the GitHub releases page first (and check owner/repo in package.json).';
  if (/ENOTFOUND|ETIMEDOUT|ECONNREFUSED|ECONNRESET|EAI_AGAIN|net::|ERR_INTERNET|socket hang up/i.test(msg)) return 'Could not reach GitHub. Check your internet connection.';
  if (/sha512|checksum/i.test(msg)) return 'The downloaded update failed its integrity check and was discarded.';
  return msg.split('\n')[0].slice(0, 200) || 'Update check failed.';
}

function createUpdater({ autoUpdater, currentVersion, packaged, beforeInstall }) {
  const ev = new EventEmitter();
  const usable = !!(packaged && autoUpdater);
  let st = {
    state: usable ? 'idle' : 'disabled', current: currentVersion, version: null, percent: 0, lastChecked: null,
    message: usable ? '' : (autoUpdater ? 'Updates work in the installed version of DownloaDan (not when started with npm start).' : 'Update support is not installed. Run "npm install" in the project folder.'),
  };
  const set = (patch) => { st = { ...st, ...patch }; ev.emit('state', st); };

  if (usable) {
    autoUpdater.autoDownload = false;            // the user decides when to download
    autoUpdater.autoInstallOnAppQuit = true;     // a downloaded update installs when DownloaDan next quits
    autoUpdater.allowPrerelease = false;
    autoUpdater.allowDowngrade = false;
    autoUpdater.on('checking-for-update', () => set({ state: 'checking', message: '' }));
    autoUpdater.on('update-available', (i) => set({ state: 'available', version: i.version, message: '', lastChecked: Date.now() }));
    autoUpdater.on('update-not-available', () => set({ state: 'none', version: null, message: '', lastChecked: Date.now() }));
    autoUpdater.on('download-progress', (p) => set({ state: 'downloading', percent: Math.max(0, Math.min(100, Math.round(p.percent || 0))) }));
    autoUpdater.on('update-downloaded', (i) => set({ state: 'ready', version: i.version, percent: 100, message: '' }));
    autoUpdater.on('error', (e) => set({ state: st.version && (st.state === 'downloading') ? 'available' : 'error', message: friendly(e), lastChecked: Date.now() }));
  }

  return {
    on: (name, fn) => ev.on(name, fn),
    state: () => st,
    async check() {
      if (!usable || ['checking', 'downloading', 'ready'].includes(st.state)) return;
      try { await autoUpdater.checkForUpdates(); } catch { /* the 'error' event already reported it */ }
    },
    async download() {
      if (!usable || st.state !== 'available') return;
      set({ state: 'downloading', percent: 0, message: '' });
      try { await autoUpdater.downloadUpdate(); } catch { /* reported through the 'error' event */ }
    },
    install() {
      if (!usable || st.state !== 'ready') return false;
      try { if (beforeInstall) beforeInstall(); } catch { /* never block the install */ }
      autoUpdater.quitAndInstall(true, true);    // silent install, then relaunch DownloaDan
      return true;
    },
  };
}

module.exports = { createUpdater, friendly };
