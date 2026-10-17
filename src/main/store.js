'use strict';
const fs = require('fs');
const path = require('path');

// Tiny JSON store with debounced, atomic writes (write temp file, then rename).
class Store {
  constructor(file, defaults) {
    this.file = file;
    this.data = defaults;
    this._t = null;
    try {
      const raw = JSON.parse(fs.readFileSync(file, 'utf8'));
      if (raw && typeof raw === 'object') this.data = { ...defaults, ...raw };
    } catch { /* first run or unreadable: start from defaults */ }
  }
  save() {
    clearTimeout(this._t);
    this._t = setTimeout(() => this.flush(), 400);
  }
  flush() {
    clearTimeout(this._t);
    const tmp = this.file + '.tmp';
    try {
      fs.mkdirSync(path.dirname(this.file), { recursive: true });
      fs.writeFileSync(tmp, JSON.stringify(this.data));
      fs.renameSync(tmp, this.file);
    } catch (e) {
      console.error('Store flush failed:', e.message);
    }
  }
}

module.exports = { Store };
