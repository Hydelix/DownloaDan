'use strict';
const ext = typeof browser !== 'undefined' ? browser : chrome;   // Firefox: browser (promises). Chromium: chrome.
// Options page: finds DownloaDan, asks it for permission (the user clicks Allow in the app), and shows the result.
const $ = (id) => document.getElementById(id);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let busy = false, lastState = '';

function show(kind, title, detail, state) {
  $('dot').className = 'dot ' + kind;
  $('sTitle').textContent = title;
  $('sDetail').textContent = detail || '';
  lastState = state || kind;
  const connected = state === 'connected';
  $('btnMain').hidden = connected || kind === 'busy';
  $('btnMain').textContent = state === 'not-running' || state === 'outdated' ? 'Retry' : 'Connect';
  $('btnDisconnect').hidden = !connected;
}

async function pair(port) {
  show('busy', 'Waiting for your permission…', 'A box should have appeared in DownloaDan. Click Allow to connect. It expires after 60 seconds.', 'pairing');
  const s = await DDBridge.pairStart(port);
  if (s.httpStatus === 403) return show('err', 'DownloaDan does not recognise this extension', 'Its ID is ' + ext.runtime.id + '. Update DownloaDan, or use the manual option below.', 'unknown');
  if (s.httpStatus === 429) {
    const why = { 'denied-recently': 'You denied the last request. You can ask again in ' + (s.retryInSec || 30) + ' seconds.', busy: 'Another request is already waiting in DownloaDan.', 'too-many': 'Too many requests. Wait a few minutes and try again.' }[s.error] || 'Try again in a moment.';
    return show('warn', 'Not connected yet', why, 'wait');
  }
  if (!s.pairId) return show('err', 'Could not start the connection', s.error || 'DownloaDan did not answer.', 'error');
  const deadline = Date.now() + ((s.expiresInSec || 60) + 5) * 1000;
  while (Date.now() < deadline) {
    await sleep(1000);
    const p = await DDBridge.pairStatus(port, s.pairId);
    if (p.status === 'allowed' && p.token) { await ext.storage.local.set({ token: p.token, port }); return inspect(false); }
    if (p.status === 'denied') return show('warn', 'Connection denied', 'You clicked Deny in DownloaDan. Click Connect to try again.', 'denied');
    if (p.status === 'expired' || p.httpStatus === 404) return show('warn', 'The request ran out of time', 'Nobody clicked Allow in time. Click Connect to try again.', 'expired');
    if (p.httpStatus === 0) return show('warn', 'DownloaDan stopped answering', 'Is it still running? Start it and click Retry.', 'not-running');
  }
  return show('warn', 'The request ran out of time', 'Nobody clicked Allow in time. Click Connect to try again.', 'expired');
}

async function inspect(autoPair) {
  show('busy', 'Looking for DownloaDan…', '', 'looking');
  const { token = '', port } = await ext.storage.local.get(['token', 'port']);
  const st = await DDBridge.connect({ port, token });
  if (st.state === 'connected') {
    if (st.port !== port) await ext.storage.local.set({ port: st.port });
    return show('ok', 'Connected to DownloaDan ' + (st.hello.version || ''), 'Downloads from your browser can now go to DownloaDan. (Port ' + st.port + ')', 'connected');
  }
  if (st.state === 'not-running') return show('warn', 'DownloaDan is not running', 'Start DownloaDan (look for it in the system tray), then click Retry.', 'not-running');
  if (st.state === 'outdated') return show('warn', 'DownloaDan needs an update', 'This extension needs a newer version of the app. Update DownloaDan, then click Retry.', 'outdated');
  if (autoPair) return pair(st.port);
  return show('warn', st.state === 'bad-token' ? 'The connection expired' : 'Not connected yet', 'Click Connect and allow it in DownloaDan.', st.state);
}

async function run(autoPair) {
  if (busy) return;
  busy = true; $('btnMain').disabled = true;
  try { await inspect(autoPair); } finally { busy = false; $('btnMain').disabled = false; }
}

$('btnMain').onclick = () => run(true);
$('btnDisconnect').onclick = async () => { await ext.storage.local.remove('token'); await run(false); };
$('capture').onchange = (e) => ext.storage.local.set({ capture: e.target.checked });
document.addEventListener('visibilitychange', () => { if (!document.hidden && lastState === 'not-running') run(true); });   // user started the app and came back

$('save').onclick = async () => {                          // manual fallback
  const msg = (t, ok) => { $('msg').textContent = t; $('msg').style.color = ok ? '#3ecf8e' : '#ff5d6c'; };
  const port = parseInt($('port').value, 10) || undefined;
  await ext.storage.local.set({ token: $('token').value.trim(), port });
  const st = await DDBridge.connect({ port, token: $('token').value.trim() });
  if (st.state === 'connected') { msg('Connected to DownloaDan.', true); run(false); }
  else msg({ 'not-running': 'Cannot reach DownloaDan. Is the app running?', 'bad-token': 'DownloaDan is running, but the token is wrong.', 'not-paired': 'Enter the token first.', outdated: 'DownloaDan is too old for this extension.' }[st.state] || 'Could not connect.', false);
};

ext.storage.local.get(['token', 'port', 'capture']).then(({ token = '', port, capture = true }) => {
  $('token').value = token; $('port').value = port || ''; $('capture').checked = capture;
  run(true);                                               // open the page -> find the app, and ask to connect if needed
});
