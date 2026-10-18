//! The download engine.
//!
//! One `run()` call downloads one file: it probes the server, splits the file into segments (one per
//! connection), streams them in parallel straight into a pre-sized `.danpart` file, retries flaky
//! connections, and renames the file when every byte has arrived. Pausing stops the connections and keeps the
//! per-segment positions so a later `run()` continues where this one stopped.
//!
//! The engine knows nothing about queues or the UI. It reports through an `on_update` callback and returns an
//! `Outcome`; the manager applies both to the download list.

use crate::model::Segment;
use crate::util::{
    filename_from, sanitize_filename, unique_name, with_ext, PART_EXT,
};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_ENCODING, CONTENT_RANGE, COOKIE, LOCATION, RANGE, REFERER, USER_AGENT};
use reqwest::{Client, Response, Url};
use serde::Serialize;
use std::error::Error as _;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
/// Files smaller than this are never split (a connection has to be worth its setup cost).
const MIN_SEGMENT: i64 = 1024 * 1024;
const MAX_REDIRECTS: usize = 8;
const MAX_RETRIES: u32 = 5;
/// Bytes collected in memory before one write to disk.
const FLUSH_AT: usize = 512 * 1024;

// ---------------------------------------------------------------------------------------------------------
// Errors, control, updates

#[derive(Debug, Clone)]
pub enum Fail {
    /// Stopped on purpose (pause / cancel / a sibling segment failed).
    Aborted,
    /// Retrying cannot help (HTTP 404, disk full, ...).
    NoRetry(String),
    /// The partial data is no longer trustworthy: throw it away and start over.
    Restart(String),
    /// Network trouble: worth retrying.
    Other(String),
}

impl Fail {
    pub fn message(&self) -> String {
        match self {
            Fail::Aborted => "Aborted".into(),
            Fail::NoRetry(m) | Fail::Restart(m) | Fail::Other(m) => m.clone(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StopReason {
    Pause,
    Cancel,
}

/// Lets the manager stop a running download and learn why it stopped.
#[derive(Clone)]
pub struct Control {
    token: CancellationToken,
    reason: Arc<AtomicU8>,
}

impl Default for Control {
    fn default() -> Self {
        Self::new()
    }
}

impl Control {
    pub fn new() -> Self {
        Control { token: CancellationToken::new(), reason: Arc::new(AtomicU8::new(0)) }
    }
    pub fn stop(&self, r: StopReason) {
        // the first reason wins (a pause followed by a cancel while stopping still ends as the first)
        let _ = self.reason.compare_exchange(0, if r == StopReason::Pause { 1 } else { 2 }, Ordering::SeqCst, Ordering::SeqCst);
        self.token.cancel();
    }
    pub fn reason(&self) -> Option<StopReason> {
        match self.reason.load(Ordering::SeqCst) {
            1 => Some(StopReason::Pause),
            2 => Some(StopReason::Cancel),
            _ => None,
        }
    }
    pub fn is_stopped(&self) -> bool {
        self.token.is_cancelled()
    }
}

#[derive(Debug, Clone)]
pub enum Update {
    Note(Option<String>),
    /// The server has been probed and the file prepared.
    Started { final_url: String, size: i64, accept_ranges: bool, filename: String, part_path: String, segments: Vec<Segment> },
    Progress { downloaded: i64, speed: i64, eta: Option<i64>, segments: Vec<Segment>, clear_note: bool },
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Completed { filename: String, size: i64, downloaded: i64 },
    Paused { downloaded: i64, segments: Option<Vec<Segment>> },
    Cancelled,
    Error { message: String, restart: bool, downloaded: i64, segments: Option<Vec<Segment>> },
}

/// What the manager remembers about an unfinished download.
#[derive(Debug, Clone)]
pub struct Resume {
    pub final_url: Option<String>,
    pub size: i64,
    pub accept_ranges: bool,
    pub part_path: PathBuf,
    pub filename: String,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone)]
pub struct JobSpec {
    pub url: String,
    pub referer: Option<String>,
    pub cookie: Option<String>,
    pub dir: PathBuf,
    /// A name chosen by the user; otherwise it comes from the server.
    pub filename: Option<String>,
    pub connections: usize,
    pub resume: Option<Resume>,
}

pub type UpdateFn = Arc<dyn Fn(Update) + Send + Sync>;

// ---------------------------------------------------------------------------------------------------------
// HTTP

pub fn build_client() -> Client {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none()) // redirects are followed by hand so cookies never leak to another host
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(32)
        .http1_only()
        .build()
        .expect("HTTP client")
}

/// The part of a request that comes from the download itself.
#[derive(Clone, Default)]
pub struct ReqHeaders {
    pub referer: Option<String>,
    pub cookie: Option<String>,
}

fn describe(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return "Connection timed out".into();
    }
    let mut msg = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        msg = s.to_string();
        src = s.source();
    }
    if e.is_connect() && !msg.to_lowercase().contains("connect") {
        msg = format!("Could not connect ({msg})");
    }
    msg
}

fn host_key(u: &Url) -> (Option<String>, Option<u16>) {
    (u.host_str().map(|h| h.to_ascii_lowercase()), u.port_or_known_default())
}

fn same_host(a: &str, b: &str) -> bool {
    match (Url::parse(a), Url::parse(b)) {
        (Ok(a), Ok(b)) => host_key(&a) == host_key(&b),
        _ => false,
    }
}

fn header_text(h: &HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    h.get(name).map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
}

/// GET `url`, following redirects by hand. Cookies are dropped as soon as a redirect leaves the original host.
async fn open(client: &Client, url: &str, h: &ReqHeaders, range: Option<&str>, cancel: &CancellationToken) -> Result<(Response, Url), Fail> {
    let mut u = Url::parse(url).map_err(|_| Fail::NoRetry("Invalid URL".into()))?;
    let mut cookie = h.cookie.clone();
    for redirects in 0..=MAX_REDIRECTS {
        if u.scheme() != "http" && u.scheme() != "https" {
            return Err(Fail::NoRetry("Only http(s) URLs are supported".into()));
        }
        if cancel.is_cancelled() {
            return Err(Fail::Aborted);
        }
        let mut req = client
            .get(u.clone())
            .header(USER_AGENT, UA)
            .header(ACCEPT, "*/*")
            .header(ACCEPT_ENCODING, "identity");
        if let Some(r) = &h.referer {
            if let Ok(v) = HeaderValue::from_str(r) {
                req = req.header(REFERER, v);
            }
        }
        if let Some(c) = &cookie {
            if let Ok(v) = HeaderValue::from_str(c) {
                req = req.header(COOKIE, v);
            }
        }
        if let Some(r) = range {
            req = req.header(RANGE, r);
        }
        let resp = tokio::select! {
            _ = cancel.cancelled() => return Err(Fail::Aborted),
            r = req.send() => r.map_err(|e| Fail::Other(describe(&e)))?,
        };
        if resp.status().is_redirection() {
            if let Some(loc) = header_text(resp.headers(), LOCATION) {
                drop(resp);
                if redirects >= MAX_REDIRECTS {
                    return Err(Fail::Other("Too many redirects".into()));
                }
                let next = u.join(&loc).map_err(|_| Fail::Other("Bad redirect".into()))?;
                if host_key(&next) != host_key(&u) {
                    cookie = None; // never leak cookies to another host
                }
                u = next;
                continue;
            }
        }
        return Ok((resp, u));
    }
    Err(Fail::Other("Too many redirects".into()))
}

pub struct Probe {
    pub final_url: String,
    pub size: i64,
    pub accept_ranges: bool,
    pub headers: HeaderMap,
}

async fn probe(client: &Client, url: &str, h: &ReqHeaders, cancel: &CancellationToken) -> Result<Probe, Fail> {
    let (resp, final_url) = open(client, url, h, Some("bytes=0-0"), cancel).await?;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    drop(resp);
    let final_url = final_url.to_string();
    match status {
        206 => {
            let size = header_text(&headers, CONTENT_RANGE)
                .and_then(|v| v.rsplit('/').next().map(|s| s.trim().to_string()))
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            Ok(Probe { final_url, size, accept_ranges: size > 0, headers })
        }
        200 => {
            let size = header_text(&headers, reqwest::header::CONTENT_LENGTH).and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(0);
            Ok(Probe { final_url, size, accept_ranges: false, headers })
        }
        416 => Ok(Probe { final_url, size: 0, accept_ranges: false, headers }),
        s => Err(Fail::NoRetry(format!("Server replied HTTP {s}"))),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Segments and disk

pub fn plan_segments(size: i64, accept_ranges: bool, connections: usize) -> Vec<Segment> {
    if !accept_ranges || size <= 0 {
        return vec![Segment { start: 0, end: if size > 0 { size - 1 } else { -1 }, pos: 0 }];
    }
    let n = (connections as i64).min(size / MIN_SEGMENT).max(1);
    let chunk = (size + n - 1) / n;
    let mut segs = vec![];
    for i in 0..n {
        let start = i * chunk;
        let end = (size - 1).min(start + chunk - 1);
        if start <= end {
            segs.push(Segment { start, end, pos: start });
        }
    }
    segs
}

fn done_bytes(segs: &[Segment]) -> i64 {
    segs.iter().map(|s| (s.pos - s.start).max(0)).sum()
}

#[cfg(unix)]
fn write_all_at(f: &File, buf: &[u8], off: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.write_all_at(buf, off)
}

#[cfg(windows)]
fn write_all_at(f: &File, mut buf: &[u8], mut off: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, off)?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
        off += n as u64;
    }
    Ok(())
}

/// On NTFS, writing far into a normal file makes Windows zero-fill everything before it, which can freeze a
/// multi-GB download at 0%. Marking the file sparse avoids that.
#[cfg(windows)]
async fn set_sparse(path: &Path) -> bool {
    use std::process::Stdio;
    let mut cmd = tokio::process::Command::new("fsutil");
    cmd.args(["sparse", "setflag"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    matches!(tokio::time::timeout(Duration::from_secs(8), cmd.status()).await, Ok(Ok(s)) if s.success())
}

#[cfg(not(windows))]
async fn set_sparse(_path: &Path) -> bool {
    false
}

// ---------------------------------------------------------------------------------------------------------
// One download

struct Shared {
    client: Client,
    url: String,
    hdr: ReqHeaders,
    file: Arc<File>,
    ranged: bool,
    starts: Vec<i64>,
    ends: Vec<i64>,
    pos: Vec<AtomicI64>,
    token: CancellationToken,
    on_update: UpdateFn,
}

impl Shared {
    fn snapshot(&self) -> Vec<Segment> {
        (0..self.pos.len()).map(|i| Segment { start: self.starts[i], end: self.ends[i], pos: self.pos[i].load(Ordering::Relaxed) }).collect()
    }
}

async fn sleep_or_abort(ms: u64, token: &CancellationToken) -> Result<(), Fail> {
    tokio::select! {
        _ = token.cancelled() => Err(Fail::Aborted),
        _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
    }
}

async fn flush(sh: &Arc<Shared>, idx: usize, buf: &mut Vec<u8>, at: &mut i64) -> Result<(), Fail> {
    if buf.is_empty() {
        return Ok(());
    }
    let file = sh.file.clone();
    let data = std::mem::take(buf);
    let off = *at as u64;
    let (data, res) = tokio::task::spawn_blocking(move || {
        let r = write_all_at(&file, &data, off);
        (data, r)
    })
    .await
    .map_err(|_| Fail::NoRetry("Disk write was interrupted".into()))?;
    *buf = data;
    let n = buf.len() as i64;
    buf.clear();
    if let Err(e) = res {
        return Err(Fail::NoRetry(format!("Could not write to disk: {e}")));
    }
    *at += n;
    sh.pos[idx].store(*at, Ordering::Relaxed);
    Ok(())
}

async fn stream_once(sh: &Arc<Shared>, idx: usize) -> Result<(), Fail> {
    let pos0 = sh.pos[idx].load(Ordering::Relaxed);
    let end = sh.ends[idx];
    let range = sh.ranged.then(|| format!("bytes={pos0}-{end}"));
    let (mut resp, _) = open(&sh.client, &sh.url, &sh.hdr, range.as_deref(), &sh.token).await?;
    let status = resp.status().as_u16();
    if sh.ranged {
        let start_ok = header_text(resp.headers(), CONTENT_RANGE)
            .and_then(|v| v.trim().strip_prefix("bytes").map(|s| s.trim_start().to_string()))
            .and_then(|s| s.split('-').next().and_then(|n| n.trim().parse::<i64>().ok()))
            .map(|n| n == pos0)
            .unwrap_or(false);
        if status != 206 || !start_ok {
            return Err(Fail::Restart("Server stopped honouring range requests".into()));
        }
    } else if status != 200 {
        return if status < 500 { Err(Fail::NoRetry(format!("Server replied HTTP {status}"))) } else { Err(Fail::Other(format!("Server replied HTTP {status}"))) };
    }

    let mut buf: Vec<u8> = Vec::with_capacity(FLUSH_AT + 64 * 1024);
    let mut at = pos0;
    let mut finished = false;
    loop {
        let next = tokio::select! {
            _ = sh.token.cancelled() => {
                flush(sh, idx, &mut buf, &mut at).await?; // keep what has arrived so a resume loses nothing
                return Err(Fail::Aborted);
            }
            c = resp.chunk() => c,
        };
        match next {
            Err(e) => {
                flush(sh, idx, &mut buf, &mut at).await?;
                return Err(Fail::Other(describe(&e)));
            }
            Ok(None) => break,
            Ok(Some(mut chunk)) => {
                if end != -1 {
                    let have = at + buf.len() as i64;
                    let room = end - have + 1;
                    if room <= 0 {
                        finished = true;
                        break;
                    }
                    if chunk.len() as i64 > room {
                        chunk.truncate(room as usize);
                    }
                }
                buf.extend_from_slice(&chunk);
                if end != -1 && at + buf.len() as i64 > end {
                    finished = true; // got everything this connection was asked for
                }
                if buf.len() >= FLUSH_AT || finished {
                    flush(sh, idx, &mut buf, &mut at).await?;
                }
                if finished {
                    break;
                }
            }
        }
    }
    flush(sh, idx, &mut buf, &mut at).await?;
    if end == -1 || at > end || finished {
        Ok(())
    } else {
        Err(Fail::Other("Connection closed early".into()))
    }
}

async fn run_segment(sh: Arc<Shared>, idx: usize) -> Result<(), Fail> {
    let mut attempt = 0u32;
    loop {
        if sh.token.is_cancelled() {
            return Err(Fail::Aborted);
        }
        let end = sh.ends[idx];
        let before = sh.pos[idx].load(Ordering::Relaxed);
        if end != -1 && before > end {
            return Ok(());
        }
        match stream_once(&sh, idx).await {
            Ok(()) => return Ok(()),
            Err(Fail::Other(msg)) => {
                if sh.pos[idx].load(Ordering::Relaxed) > before {
                    attempt = 0; // it was making progress: a new stall gets a fresh set of retries
                }
                attempt += 1;
                if attempt > MAX_RETRIES {
                    return Err(Fail::Other(msg));
                }
                (sh.on_update)(Update::Note(Some(format!("Retrying ({attempt}/{MAX_RETRIES}): {msg}"))));
                sleep_or_abort((1000u64 << attempt).min(15_000), &sh.token).await?;
            }
            Err(e) => return Err(e),
        }
    }
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Download one file. Always returns an `Outcome`; never panics on bad input or network trouble.
pub async fn run(client: Client, spec: JobSpec, ctl: Control, on_update: UpdateFn) -> Outcome {
    let hdr = ReqHeaders { referer: spec.referer.clone(), cookie: spec.cookie.clone() };
    let mut segments: Option<Vec<Segment>> = spec.resume.as_ref().map(|r| r.segments.clone());

    let result: Result<(PathBuf, String, i64), Fail> = async {
        tokio::fs::create_dir_all(&spec.dir).await.map_err(|e| Fail::NoRetry(format!("Could not create the folder: {e}")))?;
        on_update(Update::Note(Some("Connecting…".into())));

        let resumable = spec.resume.as_ref().filter(|r| !r.segments.is_empty() && r.part_path.exists());
        let (part_path, filename, size, accept_ranges, final_url, segs, file) = if let Some(r) = resumable {
            let mut segs = r.segments.clone();
            let file;
            if !r.accept_ranges {
                for s in segs.iter_mut() {
                    s.pos = s.start; // cannot resume without range support
                }
                file = std::fs::OpenOptions::new().read(true).write(true).open(&r.part_path).map_err(|e| Fail::NoRetry(format!("Could not open the partial file: {e}")))?;
                file.set_len(0).map_err(|e| Fail::NoRetry(format!("Could not reset the partial file: {e}")))?;
            } else {
                file = std::fs::OpenOptions::new().read(true).write(true).open(&r.part_path).map_err(|e| Fail::NoRetry(format!("Could not open the partial file: {e}")))?;
            }
            (r.part_path.clone(), r.filename.clone(), r.size, r.accept_ranges, r.final_url.clone().unwrap_or_else(|| spec.url.clone()), segs, file)
        } else {
            let info = probe(&client, &spec.url, &hdr, &ctl.token).await?;
            let content_type = header_text(&info.headers, reqwest::header::CONTENT_TYPE);
            let disposition = header_text(&info.headers, reqwest::header::CONTENT_DISPOSITION);
            let wanted = match &spec.filename {
                Some(n) if !n.is_empty() => n.clone(),
                _ => with_ext(&filename_from(disposition.as_deref(), &info.final_url), content_type.as_deref()),
            };
            let filename = unique_name(&spec.dir, &sanitize_filename(&wanted));
            let part_path = spec.dir.join(format!("{filename}{PART_EXT}"));
            let segs = plan_segments(info.size, info.accept_ranges, spec.connections.max(1));
            let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&part_path).map_err(|e| Fail::NoRetry(format!("Could not create the file: {e}")))?;
            if info.size > 0 {
                on_update(Update::Note(Some("Preparing file on disk…".into())));
                set_sparse(&part_path).await;
                file.set_len(info.size as u64).map_err(|e| Fail::NoRetry(format!("Could not reserve disk space: {e}")))?;
            }
            (part_path, filename, info.size, info.accept_ranges, info.final_url, segs, file)
        };

        on_update(Update::Started {
            final_url: final_url.clone(),
            size,
            accept_ranges,
            filename: filename.clone(),
            part_path: path_string(&part_path),
            segments: segs.clone(),
        });
        on_update(Update::Note(Some("Waiting for the server…".into())));

        let work_token = ctl.token.child_token();
        // The connections go straight to the final address. If a redirect led to another host, the cookie that
        // was meant for the original site must not follow.
        let mut seg_hdr = hdr.clone();
        if !same_host(&spec.url, &final_url) {
            seg_hdr.cookie = None;
        }
        let sh = Arc::new(Shared {
            client: client.clone(),
            url: final_url,
            hdr: seg_hdr,
            file: Arc::new(file),
            ranged: accept_ranges && size > 0,
            starts: segs.iter().map(|s| s.start).collect(),
            ends: segs.iter().map(|s| s.end).collect(),
            pos: segs.iter().map(|s| AtomicI64::new(s.pos)).collect(),
            token: work_token.clone(),
            on_update: on_update.clone(),
        });
        segments = Some(sh.snapshot());

        // progress ticker: speed, ETA and the resume positions, twice a second
        let start_bytes = done_bytes(&sh.snapshot());
        let ticker = {
            let sh = sh.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(500));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                interval.tick().await;
                let mut last = start_bytes;
                let mut speed = 0f64;
                loop {
                    interval.tick().await;
                    let snap = sh.snapshot();
                    let done = done_bytes(&snap);
                    let inst = (done - last) as f64 * 2.0;
                    last = done;
                    speed = if speed != 0.0 { speed * 0.6 + inst * 0.4 } else { inst };
                    let eta = if size > 0 && speed > 1.0 { Some(((size - done) as f64 / speed).round() as i64) } else { None };
                    (sh.on_update)(Update::Progress { downloaded: done, speed: speed.max(0.0).round() as i64, eta, segments: snap, clear_note: done > start_bytes });
                }
            })
        };

        let mut set = JoinSet::new();
        for i in 0..sh.pos.len() {
            set.spawn(run_segment(sh.clone(), i));
        }
        let mut first_err: Option<Fail> = None;
        while let Some(res) = set.join_next().await {
            match res {
                Ok(Ok(())) | Ok(Err(Fail::Aborted)) => {}
                Ok(Err(e)) => {
                    first_err.get_or_insert(e);
                    work_token.cancel();
                }
                Err(_) => {
                    first_err.get_or_insert(Fail::Other("Internal error in a download worker".into()));
                    work_token.cancel();
                }
            }
        }
        ticker.abort();
        let snap = sh.snapshot();
        segments = Some(snap.clone());
        if let Some(e) = first_err {
            return Err(e);
        }
        if ctl.is_stopped() {
            return Err(Fail::Aborted);
        }

        let total = done_bytes(&snap);
        if size > 0 && total != size {
            return Err(Fail::Restart("Size mismatch".into()));
        }
        let size = if size > 0 { size } else { total };
        drop(sh); // closes the file so it can be renamed
        let mut target = filename;
        if spec.dir.join(&target).exists() {
            target = unique_name(&spec.dir, &target); // never overwrite a file that appeared meanwhile
        }
        tokio::fs::rename(&part_path, spec.dir.join(&target)).await.map_err(|e| Fail::NoRetry(format!("Could not finish the file: {e}")))?;
        Ok((spec.dir.join(&target), target, size))
    }
    .await;

    let downloaded = segments.as_deref().map(done_bytes).unwrap_or(0);
    match result {
        Ok((_path, filename, size)) => Outcome::Completed { filename, size, downloaded: size },
        Err(e) => {
            if let Some(reason) = ctl.reason() {
                return match reason {
                    StopReason::Cancel => Outcome::Cancelled,
                    StopReason::Pause => Outcome::Paused { downloaded, segments },
                };
            }
            if matches!(e, Fail::Aborted) {
                return Outcome::Paused { downloaded, segments };
            }
            let restart = matches!(e, Fail::Restart(_));
            Outcome::Error { message: e.message(), restart, downloaded, segments }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Quick look at a URL (for the "New download" box and "refresh address")

#[derive(Serialize, Debug, Clone)]
pub struct Inspect {
    pub ok: bool,
    pub size: i64,
    #[serde(rename = "type")]
    pub content_type: String,
    pub resumable: bool,
    pub filename: String,
    pub error: String,
}

impl Inspect {
    fn failed(msg: impl Into<String>) -> Self {
        Inspect { ok: false, size: 0, content_type: String::new(), resumable: false, filename: String::new(), error: msg.into() }
    }
}

pub async fn inspect(client: &Client, url: &str, referer: Option<String>, cookie: Option<String>) -> Inspect {
    let token = CancellationToken::new();
    let h = ReqHeaders { referer, cookie };
    match tokio::time::timeout(Duration::from_secs(15), probe(client, url, &h, &token)).await {
        Err(_) => Inspect::failed("The server did not answer in time"),
        Ok(Err(e)) => Inspect::failed(match e {
            Fail::Aborted => "Could not reach the server".to_string(),
            other => other.message(),
        }),
        Ok(Ok(info)) => {
            let ct = header_text(&info.headers, reqwest::header::CONTENT_TYPE).unwrap_or_default();
            let ct_main = ct.split(';').next().unwrap_or("").trim().to_string();
            let disposition = header_text(&info.headers, reqwest::header::CONTENT_DISPOSITION);
            let name = with_ext(&filename_from(disposition.as_deref(), &info.final_url), Some(&ct_main));
            Inspect { ok: true, size: info.size, content_type: ct_main, resumable: info.accept_ranges, filename: sanitize_filename(&name), error: String::new() }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_plan() {
        // unknown size or no ranges: one segment
        assert_eq!(plan_segments(0, true, 8), vec![Segment { start: 0, end: -1, pos: 0 }]);
        assert_eq!(plan_segments(5_000_000, false, 8), vec![Segment { start: 0, end: 4_999_999, pos: 0 }]);
        // small files are not split
        assert_eq!(plan_segments(1_500_000, true, 8).len(), 1);
        // 10 MB with 8 connections: 8 segments covering everything exactly once
        let segs = plan_segments(10 * 1024 * 1024, true, 8);
        assert_eq!(segs.len(), 8);
        assert_eq!(segs[0].start, 0);
        assert_eq!(segs.last().unwrap().end, 10 * 1024 * 1024 - 1);
        for w in segs.windows(2) {
            assert_eq!(w[0].end + 1, w[1].start);
        }
        // never more segments than whole megabytes
        assert_eq!(plan_segments(3 * 1024 * 1024, true, 16).len(), 3);
    }

    #[test]
    fn extname_is_reexported_semantics() {
        assert_eq!(crate::util::extname("x.mp4"), ".mp4");
    }
}
