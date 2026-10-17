'use strict';
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { EventEmitter } = require('events');
const { DownloadTask, sanitizeFilename, PART_EXT } = require('./engine');

const MAIN_QUEUE = 'main';
const MAIN_NAME = 'Main';
const id = (p) => p + crypto.randomBytes(6).toString('hex');
const clampInt = (v, lo, hi, d) => { const n = Number(v); return Number.isFinite(n) ? Math.min(hi, Math.max(lo, Math.round(n))) : d; };
const fmtB = (n) => { if (!n) return '0 B'; const u = ['B', 'KB', 'MB', 'GB', 'TB']; const i = Math.min(4, Math.floor(Math.log(n) / Math.log(1024))); return (n / 1024 ** i).toFixed(i ? 1 : 0) + ' ' + u[i]; };
const cleanName = (s) => (typeof s === 'string' ? s.replace(/[\u0000-\u001f\u202a-\u202e\u2066-\u2069]/g, '').trim().slice(0, 40) : '');

function defaultState(downloadDir) {
  return {
    settings: { maxSimultaneous: 3, connections: 8, downloadDir, minimizeToTray: true, runAtStartup: false, notify: true, removeMissing: true, autoUpdate: true, bypassKey: 'F8', bypassKeyV: 3, bridgePort: 18642 },
    queues: [{ id: MAIN_QUEUE, name: MAIN_NAME, enabled: true }],
    downloads: [],
    themes: { active: 'dark', custom: [] },
    token: crypto.randomBytes(24).toString('base64url'),
  };
}

class Manager extends EventEmitter {
  constructor(store) {
    super();
    this.store = store;
    this.tasks = new Map();
    this.trash = (f) => fs.promises.unlink(f);          // replaced by the app with "move to Recycle Bin"
    const d = store.data;
    if (!d.queues.some((q) => q.id === MAIN_QUEUE)) d.queues.unshift({ id: MAIN_QUEUE, name: MAIN_NAME, enabled: true });
    if (typeof d.settings.autoUpdate !== 'boolean') d.settings.autoUpdate = true;
    if (typeof d.settings.bypassKey !== 'string') d.settings.bypassKey = 'F8';           // older saves get the default
    if (d.settings.bypassKeyV !== 3) {                                                    // the default changed twice (Alt, then Scroll Lock, now F8): move people still on an old default
      if (d.settings.bypassKey === 'Alt' || d.settings.bypassKey === 'ScrollLock') d.settings.bypassKey = 'F8';
      d.settings.bypassKeyV = 3;
    }
    if (typeof d.settings.removeMissing !== 'boolean') d.settings.removeMissing = true;      // older saves: on by default
    d.queues.find((q) => q.id === MAIN_QUEUE).name = MAIN_NAME;     // the default queue always has this fixed name (also fixes older saves)
    const dup = (q) => q.id !== MAIN_QUEUE && q.name.toLowerCase() === MAIN_NAME.toLowerCase();
    for (const q of d.queues) if (dup(q)) q.name = 'Main (2)';        // a user queue that already used the name "Main" gets renamed
    for (const it of d.downloads) {            // anything that was running when the app closed becomes paused
      if (it.status === 'downloading') { it.status = 'paused'; it.speed = 0; it.eta = null; }
      if (it.autoResume) {                                  // set just before an update restart: carry on automatically
        it.autoResume = false;
        if (it.status === 'paused') { it.status = 'queued'; it.force = true; }
      }
    }
  }
  get s() { return this.store.data.settings; }
  get queues() { return this.store.data.queues; }
  get downloads() { return this.store.data.downloads; }
  find(i) { return this.downloads.find((d) => d.id === i); }
  changed(ids, removed) { this.emit('patch', { ids: ids || [], removed: removed || [] }); this.store.save(); }

  summary(d) {
    return { id: d.id, url: d.url, filename: d.filename || null, dir: d.dir, queueId: d.queueId, status: d.status,
      size: d.size || 0, downloaded: d.downloaded || 0, speed: d.speed || 0, eta: d.eta ?? null,
      error: d.error || null, hold: !!d.hold, note: d.note || null, addedAt: d.addedAt, completedAt: d.completedAt || null };
  }
  snapshot() {
    const t = this.store.data.themes;
    return { downloads: this.downloads.map((d) => this.summary(d)), queues: this.queues, settings: this.s,
      themes: { active: t.active, custom: t.custom }, token: this.store.data.token };
  }

  // ---- downloads ---------------------------------------------------------
  add({ urls, queueId, dir, filename, referer, cookie, start, hold }) {
    const q = this.queues.find((x) => x.id === queueId) || this.queues[0];
    const list = (Array.isArray(urls) ? urls : []).slice(0, 200);
    const out = { added: [], rejected: [] };
    let target = this.s.downloadDir;
    if (typeof dir === 'string' && path.isAbsolute(dir)) target = dir;
    const ref = typeof referer === 'string' && /^https?:\/\//i.test(referer) ? referer.slice(0, 4096) : undefined;
    const ck = typeof cookie === 'string' && cookie.length <= 16384 && /^[\x20-\x7e]+$/.test(cookie) ? cookie : undefined;   // printable ASCII only (no header injection)
    for (const raw of list) {
      let u;
      try { u = new URL(String(raw).trim()); } catch { out.rejected.push(String(raw).slice(0, 200)); continue; }
      if ((u.protocol !== 'http:' && u.protocol !== 'https:') || String(raw).length > 8192) { out.rejected.push(String(raw).slice(0, 200)); continue; }
      const item = { id: id('d_'), url: u.href, referer: ref, cookie: ck, dir: target, queueId: q.id, status: 'queued', force: !!start, hold: !!hold && !start,
        filename: list.length === 1 && filename ? sanitizeFilename(filename) : null,
        size: 0, downloaded: 0, speed: 0, eta: null, error: null, addedAt: Date.now() };
      this.downloads.push(item);
      out.added.push(item.id);
    }
    this.changed(out.added);
    this.schedule();
    return out;
  }

  pause(ids) {
    for (const i of ids) {
      const d = this.find(i); if (!d) continue;
      if (d.status === 'downloading') this.tasks.get(i)?.stop('pause');
      else if (d.status === 'queued') { d.status = 'paused'; d.force = false; d.hold = false; }
    }
    this.changed(ids);
  }
  pauseAll() { this.pause(this.downloads.filter((d) => d.status === 'downloading' || d.status === 'queued').map((d) => d.id)); }

  resume(ids) {
    for (const i of ids) {
      const d = this.find(i);
      if (d && (d.status === 'paused' || d.status === 'error')) { d.status = 'queued'; d.force = true; d.hold = false; d.error = null; }
    }
    this.changed(ids);
    this.schedule();
  }

  // Remove downloads from the list. Finished files are only touched when deleteFiles is true, and then they
  // go through this.trash (the Recycle Bin in the app), never a silent permanent delete.
  async remove(ids, deleteFiles) {
    const out = { removed: 0, deleted: 0, failed: 0 };
    for (const i of ids) {
      const d = this.find(i); if (!d) continue;
      if (this.tasks.has(i)) await this.tasks.get(i).stop('cancel');
      if (d.status !== 'completed' && d.partPath) { try { fs.unlinkSync(d.partPath); } catch { /* already gone */ } }   // unfinished data
      if (d.status === 'completed' && deleteFiles && d.filename) {
        const f = path.join(d.dir, d.filename);
        if (fs.existsSync(f)) { try { await this.trash(f); out.deleted += 1; } catch { out.failed += 1; } }
      }
      this.store.data.downloads = this.downloads.filter((x) => x.id !== i);
      out.removed += 1;
    }
    this.changed([], ids);
    this.schedule();
    return out;
  }
  clearCompleted() {
    const ids = this.downloads.filter((d) => d.status === 'completed').map((d) => d.id);
    this.store.data.downloads = this.downloads.filter((d) => d.status !== 'completed');
    this.changed([], ids);
  }
  moveToQueue(ids, queueId) {
    if (!this.queues.some((q) => q.id === queueId)) return;
    for (const i of ids) { const d = this.find(i); if (d && d.status !== 'completed') d.queueId = queueId; }
    this.changed(ids);
    this.schedule();
  }
  filePath(i) { const d = this.find(i); return d && d.status === 'completed' && d.filename ? path.join(d.dir, d.filename) : null; }
  locatePath(i) {
    const d = this.find(i); if (!d || !d.filename) return null;
    if (d.status === 'completed') return path.join(d.dir, d.filename);
    return d.partPath || path.join(d.dir, d.filename + PART_EXT);       // unfinished: the real partial file, whatever its extension
  }

  // Called right before DownloaDan restarts to install an update: remember what was running.
  markResumeOnStart() {
    for (const d of this.downloads) if (d.status === 'downloading') d.autoResume = true;
    this.store.flush();
  }

  // ---- remove completed downloads whose file is gone -------------------------
  // Only the LIST entry is removed (never a file). If the drive itself is unreachable (unplugged USB,
  // offline share) the entry is kept, so a missing drive can't wipe your history.
  async pruneMissing() {
    if (!this.s.removeMissing || this._pruning) return [];
    this._pruning = true;
    const gone = [];
    try {
      const done = this.downloads.filter((d) => d.status === 'completed' && d.filename);
      for (let i = 0; i < done.length; i += 40) {
        await Promise.all(done.slice(i, i + 40).map(async (d) => {
          try { await fs.promises.access(path.join(d.dir, d.filename)); return; } catch { /* not there */ }
          try { await fs.promises.access(path.parse(path.resolve(d.dir)).root); } catch { return; }   // drive/share unreachable: keep
          gone.push(d.id);
        }));
      }
      if (gone.length) {
        const set = new Set(gone);
        this.store.data.downloads = this.downloads.filter((d) => !set.has(d.id));
        this.changed([], gone);
        this.emit('pruned', gone.length);
      }
    } finally { this._pruning = false; }
    return gone;
  }

  // ---- refresh download address ------------------------------------------
  sameName(a, b) {
    const norm = (s) => String(s || '').toLowerCase().replace(/ \(\d+\)(?=\.[^.]*$|$)/, '').trim();
    return norm(a) === norm(b);
  }
  // Is the file behind a newly caught link the same file as this (partly downloaded) item?
  compareForRefresh(d, info) {
    const reasons = [];
    const sizeKnown = d.size > 0 && info.size > 0;
    if (sizeKnown && d.size !== info.size) reasons.push(`Different size: expected ${fmtB(d.size)}, the new link is ${fmtB(info.size)}.`);
    else if (!sizeKnown && d.filename && !this.sameName(d.filename, info.filename)) reasons.push('Different file name, and the size cannot be compared.');
    if (!reasons.length && (d.downloaded || 0) > 0 && !info.resumable) reasons.push('The new server does not support resuming, so the download would start again from the beginning.');
    return reasons.length ? { ok: false, reasons } : { ok: true };
  }
  // Point an unfinished download at a new link. restart=true throws away the partial file.
  refreshAddress(id, { url, referer, cookie }, { restart = false, resetName = false } = {}) {
    const d = this.find(id);
    if (!d || d.status === 'completed' || d.status === 'downloading') return false;
    d.url = url; d.finalUrl = null;
    if (referer !== undefined) d.referer = referer;
    if (cookie !== undefined) d.cookie = cookie;
    if (restart) {
      if (d.partPath) { try { fs.unlinkSync(d.partPath); } catch { /* already gone */ } }
      d.segments = null; d.partPath = null; d.size = 0; d.downloaded = 0;
      if (resetName) d.filename = null;
    }
    d.status = 'queued'; d.force = true; d.hold = false; d.error = null; d.note = null;
    this.changed([id]); this.schedule();
    return true;
  }

  // ---- queues ------------------------------------------------------------
  createQueue(name) {
    const n = cleanName(name);
    if (!n) return { ok: false, error: 'Enter a queue name (1-40 characters).' };
    if (this.queues.some((q) => q.name.toLowerCase() === n.toLowerCase())) return { ok: false, error: 'A queue with that name already exists.' };
    if (this.queues.length >= 30) return { ok: false, error: 'Queue limit reached.' };
    const q = { id: id('q_'), name: n, enabled: true };
    this.queues.push(q); this.emit('meta'); this.store.save();
    return { ok: true, id: q.id };
  }
  renameQueue(qid, name) {
    if (qid === MAIN_QUEUE) return { ok: false, error: 'The Main queue is the default queue and cannot be renamed.' };
    const q = this.queues.find((x) => x.id === qid); const n = cleanName(name);
    if (!q) return { ok: false, error: 'Queue not found.' };
    if (!n) return { ok: false, error: 'Enter a queue name (1-40 characters).' };
    if (this.queues.some((x) => x.id !== qid && x.name.toLowerCase() === n.toLowerCase())) return { ok: false, error: 'A queue with that name already exists.' };
    q.name = n; this.emit('meta'); this.store.save();
    return { ok: true };
  }
  deleteQueue(qid) {
    if (qid === MAIN_QUEUE) return { ok: false, error: 'The Main queue is the default queue and cannot be deleted.' };
    if (!this.queues.some((q) => q.id === qid)) return { ok: false, error: 'Queue not found.' };
    const moved = [];
    for (const d of this.downloads) if (d.queueId === qid) { d.queueId = MAIN_QUEUE; moved.push(d.id); }
    this.store.data.queues = this.queues.filter((q) => q.id !== qid);
    this.emit('meta'); this.changed(moved); this.schedule();
    return { ok: true };
  }
  setQueueEnabled(qid, on) {
    if (on) return this.startQueue(qid);
    const q = this.queues.find((x) => x.id === qid); if (!q) return;
    q.enabled = false; this.emit('meta'); this.store.save();
  }
  // Start a queue: let it run, and release anything that was added "without starting".
  startQueue(qid) {
    const q = this.queues.find((x) => x.id === qid); if (!q) return;
    q.enabled = true;
    const released = this.downloads.filter((d) => d.queueId === qid && d.status === 'queued' && d.hold);
    released.forEach((d) => { d.hold = false; });
    this.emit('meta'); this.changed(released.map((d) => d.id)); this.schedule();
  }

  // ---- settings ----------------------------------------------------------
  setSettings(p) {
    const s = this.s; if (!p || typeof p !== 'object') return;
    if ('maxSimultaneous' in p) s.maxSimultaneous = clampInt(p.maxSimultaneous, 1, 10, s.maxSimultaneous);
    if ('connections' in p) s.connections = clampInt(p.connections, 1, 16, s.connections);
    if ('bridgePort' in p) s.bridgePort = clampInt(p.bridgePort, 1024, 65535, s.bridgePort);
    // key that lets the browser handle a download: '' = off, otherwise a key id such as Alt, Shift, KeyB, F8 (letters/digits only; Escape is reserved)
    if (typeof p.bypassKey === 'string' && (p.bypassKey === '' || (/^[A-Za-z0-9]{1,20}$/.test(p.bypassKey) && p.bypassKey !== 'Escape'))) s.bypassKey = p.bypassKey;
    if (typeof p.downloadDir === 'string' && path.isAbsolute(p.downloadDir)) s.downloadDir = p.downloadDir;
    for (const k of ['minimizeToTray', 'runAtStartup', 'notify', 'removeMissing', 'autoUpdate']) if (typeof p[k] === 'boolean') s[k] = p[k];
    this.emit('meta'); this.store.save(); this.schedule();
  }
  regenerateToken() { this.store.data.token = crypto.randomBytes(24).toString('base64url'); this.emit('meta'); this.store.save(); return this.store.data.token; }

  // ---- scheduler ---------------------------------------------------------
  schedule() {
    let active = this.tasks.size;
    const max = this.s.maxSimultaneous;
    if (active >= max) return;
    const order = new Map(this.queues.map((q, n) => [q.id, n]));
    const runnable = (d) => d.status === 'queued' && !d.hold && (d.force || this.queues.find((q) => q.id === d.queueId)?.enabled);
    const next = this.downloads.filter(runnable).sort((a, b) =>
      (b.force ? 1 : 0) - (a.force ? 1 : 0) || (order.get(a.queueId) ?? 99) - (order.get(b.queueId) ?? 99) || a.addedAt - b.addedAt);
    for (const d of next) { if (active >= max) break; this.start(d); active++; }
  }

  start(d) {
    d.status = 'downloading'; d.error = null;
    const task = new DownloadTask(d, { connections: this.s.connections });
    this.tasks.set(d.id, task);
    let lastSave = 0;
    task.on('progress', () => {
      this.emit('patch', { ids: [d.id], removed: [] });
      if (Date.now() - lastSave > 3000) { lastSave = Date.now(); this.store.save(); }   // keeps resume data fresh
    });
    task.donePromise.then((r) => {
      this.tasks.delete(d.id);
      d.speed = 0; d.eta = null; d.note = null;
      if (r.state === 'completed') { d.status = 'completed'; d.cookie = null; d.force = false; d.segments = null; d.partPath = null; d.completedAt = Date.now(); this.emit('completed', this.summary(d)); }
      else if (r.state === 'paused') { d.status = 'paused'; d.force = false; }
      else if (r.state === 'error') {
        d.status = 'error'; d.error = r.error; d.force = false;
        if (r.restart) { d.segments = null; if (d.partPath) { try { fs.unlinkSync(d.partPath); } catch { /* ignore */ } } }
      }
      if (this.find(d.id)) this.changed([d.id]);
      this.schedule();
    });
    this.changed([d.id]);
  }

  async shutdown() {
    await Promise.race([Promise.all([...this.tasks.values()].map((t) => t.stop('pause'))), new Promise((r) => setTimeout(r, 2500))]);
    for (const d of this.downloads) if (d.status === 'downloading') d.status = 'paused';
    this.store.flush();
  }
}

module.exports = { Manager, defaultState, MAIN_QUEUE };
