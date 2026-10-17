'use strict';
const { contextBridge, ipcRenderer } = require('electron');

// The permission box only gets these two calls.
contextBridge.exposeInMainWorld('danPair', Object.freeze({
  init: () => ipcRenderer.invoke('pair:init'),
  answer: (allow) => ipcRenderer.invoke('pair:answer', !!allow),
}));
