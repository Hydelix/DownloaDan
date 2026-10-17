'use strict';
const http = require('http');
const https = require('https');
const fs = require('fs');
const fsp = fs.promises;
const path = require('path');
const { execFile } = require('child_process');
const { EventEmitter } = require('events');

const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36';
const MIN_SEGMENT = 1024 * 1024;
const MAX_REDIRECTS = 8;
const MAX_RETRIES = 5;
const PART_EXT = '.danpart';

const agents = {
  'http:': new http.Agent({ keepAlive: true, maxSockets: 64 }),
  'https:': new https.Agent({ keepAlive: true, maxSockets: 64 }),
};

const abortErr = () => Object.assign(new Error('Aborted'), { aborted: true });
const sleep = (ms, signal) => new Promise((resolve, reject) => {
  const t = setTimeout(resolve, ms);
  signal.addEventListener('abort', () => { clearTimeout(t); reject(abortErr()); }, { once: true });
});

function open(url, headers, signal, redirects = 0) {
  return new Promise((resolve, reject) => {
    let u;
    try { u = new URL(url); } catch { return reject(new Error('Invalid URL')); }
    if (u.protocol !== 'http:' && u.protocol !== 'https:') return reject(new Error('Only http(s) URLs are supported'));
    if (signal.aborted) return reject(abortErr());
    const lib = u.protocol === 'https:' ? https : http;
    const req = lib.get(u, { headers, agent: agents[u.protocol] }, (res) => {
      const code = res.statusCode;
      if (code >= 300 && code < 400 && res.headers.location) {
        res.resume();
        if (redirects >= MAX_REDIRECTS) return reject(new Error('Too many redirects'));
        let next;
        try { next = new URL(res.headers.location, u).href; } catch { return reject(new Error('Bad redirect')); }
        let nextHeaders = headers;
        if (headers.Cookie && new URL(next).host !== u.host) { nextHeaders = { ...headers }; delete nextHeaders.Cookie; }   // never leak cookies to another host
        return open(next, nextHeaders, signal, redirects + 1).then(resolve, reject);
      }
      resolve({ res, url: u.href });
    });
    req.on('error', reject);
    req.setTimeout(30000, () => req.destroy(new Error('Connection timed out')));
    signal.addEventListener('abort', () => req.destroy(abortErr()), { once: true });
  });
}

function baseHeaders(item) {
  const h = { 'User-Agent': UA, Accept: '*/*', 'Accept-Encoding': 'identity' };
  if (item.referer) h.Referer = item.referer;
  if (item.cookie) h.Cookie = item.cookie;
  return h;
}

async function probe(url, headers, signal) {
  const { res, url: finalUrl } = await open(url, { ...headers, Range: 'bytes=0-0' }, signal);
  const status = res.statusCode;
  const h = res.headers;
  res.destroy();
  if (status === 206) {
    const m = /\/(\d+)\s*$/.exec(h['content-range'] || '');
    const size = m ? parseInt(m[1], 10) : 0;
    return { finalUrl, size, acceptRanges: size > 0, headers: h };
  }
  if (status === 200) return { finalUrl, size: parseInt(h['content-length'] || '0', 10) || 0, acceptRanges: false, headers: h };
  if (status === 416) return { finalUrl, size: 0, acceptRanges: false, headers: h };
  throw Object.assign(new Error(`Server replied HTTP ${status}`), { noRetry: true });
}

const MIME_EXT = { 'video/mp4': '.mp4', 'video/webm': '.webm', 'video/x-matroska': '.mkv', 'video/quicktime': '.mov',
  'audio/mpeg': '.mp3', 'audio/mp4': '.m4a', 'audio/ogg': '.ogg', 'audio/flac': '.flac', 'audio/wav': '.wav',
  'application/pdf': '.pdf', 'application/zip': '.zip', 'image/jpeg': '.jpg', 'image/png': '.png' };

function filenameFrom(headers, url) {
  const cd = headers['content-disposition'];
  if (cd) {
    let m = /filename\*\s*=\s*utf-8''([^;]+)/i.exec(cd);
    if (m) { try { return decodeURIComponent(m[1].trim().replace(/^"|"$/g, '')); } catch { /* fall through */ } }
    m = /filename\s*=\s*"([^"]+)"/i.exec(cd) || /filename\s*=\s*([^;]+)/i.exec(cd);
    if (m) return m[1].trim();
  }
  try {
    const last = new URL(url).pathname.split('/').filter(Boolean).pop() || '';
    const p = decodeURIComponent(last);
    if (p) return p;
  } catch { /* ignore */ }
  return 'download';
}

function sanitizeFilename(name) {
  let n = String(name)
    .replace(/[\u202a-\u202e\u2066-\u2069]/g, '')       // bidi spoofing
    .replace(/[\u0000-\u001f<>:"/\\|?*]/g, '_')          // illegal on Windows
    .trim().replace(/[. ]+$/, '');
  if (/^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\..*)?$/i.test(n)) n = '_' + n;
  if (!n) n = 'download';
  if (n.length > 180) { const ext = path.extname(n).slice(0, 12); n = n.slice(0, 180 - ext.length) + ext; }
  return n;
}

function withExt(name, contentType) {
  if (path.extname(name)) return name;
  const ext = MIME_EXT[(contentType || '').split(';')[0].trim().toLowerCase()];
  return ext ? name + ext : name;
}

function uniqueName(dir, name) {
  const ext = path.extname(name);
  const base = name.slice(0, name.length - ext.length);
  let n = name, i = 0;
  while (fs.existsSync(path.join(dir, n)) || fs.existsSync(path.join(dir, n + PART_EXT)) || fs.existsSync(path.join(dir, n + '.fluxpart'))) {
    i += 1; n = `${base} (${i})${ext}`;
  }
  return n;
}

function planSegments(size, acceptRanges, connections) {
  if (!acceptRanges || !size) return [{ start: 0, end: size ? size - 1 : -1, pos: 0 }];
  const n = Math.max(1, Math.min(connections, Math.floor(size / MIN_SEGMENT)));
  const chunk = Math.ceil(size / n);
  const segs = [];
  for (let i = 0; i < n; i++) {
    const start = i * chunk;
    const end = Math.min(size - 1, start + chunk - 1);
    if (start <= end) segs.push({ start, end, pos: start });
  }
  return segs;
}

// On NTFS, writing far into a normal file forces Windows to zero-fill everything before it, which can
// freeze a multi-GB download at 0%. Marking the file sparse avoids that.
function setSparse(file) {
  return new Promise((resolve) => {
    if (process.platform !== 'win32') return resolve(false);
    execFile('fsutil', ['sparse', 'setflag', file], { windowsHide: true, timeout: 8000 }, (err) => resolve(!err));
  });
}

const sumDone = (segs) => segs.reduce((a, s) => a + Math.max(0, s.pos - s.start), 0);

class DownloadTask extends EventEmitter {
  /** item is mutated in place (url, dir, filename, segments, size, ...). */
  constructor(item, { connections = 8 } = {}) {
    super();
    this.item = item;
    this.connections = connections;
    this.ac = new AbortController();
    this.fh = null;
    this.reason = null;
    this.lastBytes = 0;
    this.speed = 0;
    this.donePromise = this._run();
  }

  stop(reason) {            // reason: 'pause' | 'cancel'
    this.reason = reason;
    this.ac.abort();
    return this.donePromise;
  }

  async _run() {
    const it = this.item;
    const signal = this.ac.signal;
    let timer = null;
    try {
      await fsp.mkdir(it.dir, { recursive: true });
      const headers = baseHeaders(it);
      this._note('Connecting…');

      if (!it.segments || !it.partPath || !fs.existsSync(it.partPath)) {
        const info = await probe(it.url, headers, signal);
        it.finalUrl = info.finalUrl;
        it.size = info.size;
        it.acceptRanges = info.acceptRanges;
        const wanted = it.filename || withExt(filenameFrom(info.headers, info.finalUrl), info.headers['content-type']);
        it.filename = uniqueName(it.dir, sanitizeFilename(wanted));
        it.partPath = path.join(it.dir, it.filename + PART_EXT);
        it.segments = planSegments(it.size, it.acceptRanges, this.connections);
        this.fh = await fsp.open(it.partPath, 'w+');
        if (it.size > 0) {
          this._note('Preparing file on disk…');
          await setSparse(it.partPath);
          await this.fh.truncate(it.size);
        }
      } else {
        if (!it.acceptRanges) for (const s of it.segments) s.pos = s.start;   // cannot resume
        this.fh = await fsp.open(it.partPath, 'r+');
        if (!it.acceptRanges) await this.fh.truncate(0);
      }

      this.lastBytes = this.startBytes = sumDone(it.segments);
      this._note('Waiting for the server…');
      timer = setInterval(() => this._tick(), 500);

      const work = it.segments.map((s) => this._runSegment(s, signal).catch((e) => { this.ac.abort(); throw e; }));
      await Promise.all(work);

      const total = sumDone(it.segments);
      if (it.size > 0 && total !== it.size) throw Object.assign(new Error('Size mismatch'), { restart: true });
      if (!(it.size > 0)) it.size = total;
      await this.fh.close(); this.fh = null;
      let target = it.filename;                                        // never overwrite a file that appeared meanwhile
      if (fs.existsSync(path.join(it.dir, target))) target = uniqueName(it.dir, target);
      await fsp.rename(it.partPath, path.join(it.dir, target));
      it.filename = target;
      it.downloaded = total;
      return { state: 'completed' };
    } catch (e) {
      it.downloaded = it.segments ? sumDone(it.segments) : 0;
      if (this.reason || e.aborted) return { state: this.reason === 'cancel' ? 'cancelled' : 'paused' };
      return { state: 'error', error: e.message || String(e), restart: !!e.restart };
    } finally {
      clearInterval(timer);
      if (this.fh) { try { await this.fh.close(); } catch { /* ignore */ } this.fh = null; }
    }
  }

  _note(text) { this.item.note = text; this.emit('progress', this.item); }

  _tick() {
    const it = this.item;
    const done = sumDone(it.segments);
    const inst = (done - this.lastBytes) * 2;          // bytes/s (500ms interval)
    this.lastBytes = done;
    this.speed = this.speed ? this.speed * 0.6 + inst * 0.4 : inst;
    it.downloaded = done;
    if (done > this.startBytes) it.note = null;
    it.speed = Math.max(0, Math.round(this.speed));
    it.eta = it.size > 0 && this.speed > 1 ? Math.round((it.size - done) / this.speed) : null;
    this.emit('progress', it);
  }

  async _runSegment(seg, signal) {
    let attempt = 0;
    for (;;) {
      if (signal.aborted) throw abortErr();
      if (seg.end !== -1 && seg.pos > seg.end) return;
      try { await this._stream(seg, signal); return; } catch (e) {
        if (e.aborted || signal.aborted || e.noRetry || e.restart) throw e;
        if (++attempt > MAX_RETRIES) throw e;
        this._note(`Retrying (${attempt}/${MAX_RETRIES}): ${e.message}`);
        await sleep(Math.min(1000 * 2 ** attempt, 15000), signal);
      }
    }
  }

  async _stream(seg, signal) {
    const it = this.item;
    const ranged = it.acceptRanges && it.size > 0;
    const headers = baseHeaders(it);
    if (ranged) headers.Range = `bytes=${seg.pos}-${seg.end}`;
    const { res } = await open(it.finalUrl || it.url, headers, signal);
    if (ranged) {
      const m = /bytes\s+(\d+)-/.exec(res.headers['content-range'] || '');
      if (res.statusCode !== 206 || !m || parseInt(m[1], 10) !== seg.pos) {
        res.destroy();
        throw Object.assign(new Error('Server stopped honouring range requests'), { restart: true });
      }
    } else if (res.statusCode !== 200) {
      res.destroy();
      throw Object.assign(new Error(`Server replied HTTP ${res.statusCode}`), { noRetry: res.statusCode < 500 });
    }
    return new Promise((resolve, reject) => {
      let finished = false;
      const fail = (e) => { if (!finished) { finished = true; res.destroy(); reject(e); } };
      res.on('error', fail);
      res.on('close', () => fail(new Error('Connection closed early')));
      res.on('end', () => {
        if (finished) return;
        if (seg.end === -1 || seg.pos > seg.end) { finished = true; resolve(); }
        else fail(new Error('Connection closed early'));
      });
      res.on('data', (chunk) => {
        res.pause();
        const at = seg.pos;
        if (seg.end !== -1 && at + chunk.length - 1 > seg.end) chunk = chunk.subarray(0, seg.end - at + 1);
        this.fh.write(chunk, 0, chunk.length, at).then(() => {
          seg.pos = at + chunk.length;
          if (!finished) res.resume();
        }, (e) => { e.noRetry = true; fail(e); });
      });
    });
  }
}

// Quick look at a URL (name, size, resume support) for the "New download" box.
async function inspect(url, { referer, cookie } = {}) {
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), 15000);
  try {
    const info = await probe(url, baseHeaders({ referer, cookie }), ac.signal);
    const type = String(info.headers['content-type'] || '').split(';')[0].trim();
    return { ok: true, size: info.size, type, resumable: info.acceptRanges,
      filename: sanitizeFilename(withExt(filenameFrom(info.headers, info.finalUrl), type)) };
  } catch (e) {
    return { ok: false, error: e.aborted ? 'The server did not answer in time' : (e.message || 'Could not reach the server') };
  } finally { clearTimeout(timer); }
}

module.exports = { inspect, DownloadTask, sanitizeFilename, uniqueName, PART_EXT, planSegments };
