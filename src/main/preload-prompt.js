'use strict';
const { contextBridge, ipcRenderer } = require('electron');

// The popup only gets these fixed calls (no Node, no filesystem, no raw IPC).
const CALLS = { init: 'prompt:init', probe: 'prompt:probe', free: 'prompt:free', chooseDir: 'prompt:dir', confirm: 'prompt:confirm', cancel: 'prompt:cancel' };
const api = {};
for (const [name, channel] of Object.entries(CALLS)) api[name] = (...a) => ipcRenderer.invoke(channel, ...a);
api.onNext = (cb) => ipcRenderer.on('prompt:next', (_e, payload) => cb(payload));
contextBridge.exposeInMainWorld('danPrompt', Object.freeze(api));
