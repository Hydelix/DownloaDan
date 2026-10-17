'use strict';
const { contextBridge, ipcRenderer } = require('electron');

// The renderer only gets this fixed list of calls. No Node, no fs, no raw IPC.
const CALLS = {
  getState: 'state:get', readClipboard: 'clip:read', writeClipboard: 'clip:write',
  add: 'dl:add', pause: 'dl:pause', pauseAll: 'dl:pauseAll', resume: 'dl:resume', remove: 'dl:remove',
  clearCompleted: 'dl:clearCompleted', move: 'dl:move', openFile: 'dl:open', showInFolder: 'dl:show',
  createQueue: 'q:create', renameQueue: 'q:rename', deleteQueue: 'q:delete', enableQueue: 'q:enable', startQueue: 'q:start',
  updateGet: 'update:get', updateCheck: 'update:check', updateDownload: 'update:download', updateInstall: 'update:install',
  refreshStart: 'refresh:start', refreshCancel: 'refresh:cancel', refreshUseUrl: 'refresh:url', refreshDecide: 'refresh:decide',
  setSettings: 'settings:set', chooseDir: 'dir:choose', regenToken: 'token:regen',
  validateTheme: 'theme:validate', saveTheme: 'theme:save', applyTheme: 'theme:apply',
  deleteTheme: 'theme:delete', exportTheme: 'theme:export', importTheme: 'theme:import',
};
const EVENTS = new Set(['patch', 'meta', 'ui:prompt', 'bridge:status', 'refresh:event', 'notice', 'update:state']);

const api = {};
for (const [name, channel] of Object.entries(CALLS)) api[name] = (...a) => ipcRenderer.invoke(channel, ...a);
api.on = (ch, cb) => { if (EVENTS.has(ch)) ipcRenderer.on(ch, (_e, payload) => cb(payload)); };
contextBridge.exposeInMainWorld('dan', Object.freeze(api));
