//! The download list: queues, the scheduler, settings, themes and saving to disk.
//!
//! The manager owns all state behind one lock. Nothing holds the lock across an `.await`, and events are only
//! emitted after the lock is released, so a listener can safely call back into the manager.

use crate::engine::{self, Control, JobSpec, Outcome, Resume, StopReason, Update};
use crate::model::*;
use crate::theme;
use crate::util::{clean_name, fmt_bytes, new_id, new_token, now_ms, sanitize_filename, same_name, PART_EXT};
use parking_lot::Mutex;
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Notify};

#[derive(Debug, Clone)]
pub enum Event {
    /// Downloads that changed or were removed (the app batches these before telling the window).
    Patch { ids: Vec<String>, removed: Vec<String> },
    /// Queues, settings, themes or the token changed.
    Meta,
    /// This many finished downloads were dropped from the list because their file is gone.
    Pruned(usize),
    Completed(Summary),
}

pub type EventFn = Arc<dyn Fn(Event) + Send + Sync>;
pub type TrashFn = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

#[derive(Default, Debug, Clone)]
pub struct AddReq {
    pub urls: Vec<String>,
    pub queue_id: Option<String>,
    pub dir: Option<String>,
    pub filename: Option<String>,
    pub referer: Option<String>,
    pub cookie: Option<String>,
    pub start: bool,
    pub hold: bool,
}

#[derive(Serialize, Default, Debug, Clone)]
pub struct AddResult {
    pub added: Vec<String>,
    pub rejected: Vec<String>,
}

#[derive(Serialize, Default, Debug, Clone)]
pub struct RemoveResult {
    pub removed: usize,
    pub deleted: usize,
    pub failed: usize,
}

/// `{ ok, error?, id?, name? }` as the window expects it.
#[derive(Serialize, Default, Debug, Clone)]
pub struct OpResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl OpResult {
    pub fn ok() -> Self {
        OpResult { ok: true, ..Default::default() }
    }
    pub fn ok_id(id: String) -> Self {
        OpResult { ok: true, id: Some(id), ..Default::default() }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        OpResult { ok: false, error: Some(msg.into()), ..Default::default() }
    }
}

#[derive(Serialize, Debug, Clone)]
pub struct RefreshVerdict {
    pub ok: bool,
    pub reasons: Vec<String>,
}

struct Running {
    ctl: Control,
    done: watch::Receiver<bool>,
    last_save: i64,
}

struct Inner {
    state: State,
    tasks: HashMap<String, Running>,
    pruning: bool,
}

pub struct Manager {
    inner: Mutex<Inner>,
    path: PathBuf,
    client: Client,
    rt: tokio::runtime::Handle,
    events: EventFn,
    trash: Mutex<TrashFn>,
    save_wake: Notify,
}

fn clamp_int(v: &Value, lo: i64, hi: i64, default: i64) -> i64 {
    let n = match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    };
    match n {
        Some(f) if f.is_finite() => (f.round() as i64).clamp(lo, hi),
        _ => default,
    }
}

fn http_url(raw: &str) -> Option<url::Url> {
    let u = url::Url::parse(raw.trim()).ok()?;
    (u.scheme() == "http" || u.scheme() == "https").then_some(u)
}

pub fn valid_referer(r: &str) -> Option<String> {
    let lower = r.to_ascii_lowercase();
    ((lower.starts_with("http://") || lower.starts_with("https://")) && !r.contains(['\r', '\n'])).then(|| r.chars().take(4096).collect())
}

/// Cookie header values: printable ASCII only (this also blocks header injection).
pub fn valid_cookie(c: &str) -> Option<String> {
    (c.len() <= 16384 && !c.is_empty() && c.bytes().all(|b| (0x20..=0x7e).contains(&b))).then(|| c.to_string())
}

fn key_ok(k: &str) -> bool {
    k.is_empty() || ((1..=20).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_alphanumeric()) && k != "Escape")
}

impl Manager {
    /// Load (or create) the state file and start the background saver.
    pub fn new(path: PathBuf, default_download_dir: String, rt: tokio::runtime::Handle, events: EventFn) -> Arc<Manager> {
        let mut state = load_state(&path);
        normalise(&mut state, &default_download_dir);
        let m = Arc::new(Manager {
            inner: Mutex::new(Inner { state, tasks: HashMap::new(), pruning: false }),
            path,
            client: engine::build_client(),
            rt: rt.clone(),
            events,
            trash: Mutex::new(Arc::new(|p: &Path| std::fs::remove_file(p).map_err(|e| e.to_string()))),
            save_wake: Notify::new(),
        });
        m.save(); // write the tidied state (and a new pairing token on first run) right away, not only after the first change
        let saver = m.clone();
        rt.spawn(async move {
            loop {
                saver.save_wake.notified().await;
                tokio::time::sleep(Duration::from_millis(400)).await; // gather bursts of changes into one write
                let me = saver.clone();
                let _ = tokio::task::spawn_blocking(move || me.flush()).await;
            }
        });
        m
    }

    /// Replace how finished files are deleted (the app sends them to the Recycle Bin).
    pub fn set_trash(&self, f: TrashFn) {
        *self.trash.lock() = f;
    }

    pub fn client(&self) -> Client {
        self.client.clone()
    }

    fn emit(&self, e: Event) {
        (self.events)(e);
    }

    pub fn save(&self) {
        self.save_wake.notify_one();
    }

    /// Write the state file now (atomic: temp file, then rename).
    pub fn flush(&self) {
        let json = {
            let g = self.inner.lock();
            serde_json::to_vec(&g.state)
        };
        let Ok(json) = json else { return };
        let tmp = self.path.with_extension("json.tmp");
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    fn changed(&self, ids: Vec<String>, removed: Vec<String>) {
        self.emit(Event::Patch { ids, removed });
        self.save();
    }

    fn meta_changed(&self) {
        self.emit(Event::Meta);
        self.save();
    }

    // ---- read access --------------------------------------------------------------------------------------

    pub fn settings(&self) -> Settings {
        self.inner.lock().state.settings.clone()
    }
    pub fn token(&self) -> String {
        self.inner.lock().state.token.clone()
    }
    pub fn bypass_key(&self) -> String {
        self.inner.lock().state.settings.bypass_key.clone()
    }
    pub fn summary_of(&self, id: &str) -> Option<Summary> {
        self.inner.lock().state.downloads.iter().find(|d| d.id == id).map(Summary::from)
    }
    pub fn summaries(&self, ids: &[String]) -> Vec<Summary> {
        let g = self.inner.lock();
        ids.iter().filter_map(|i| g.state.downloads.iter().find(|d| &d.id == i)).map(Summary::from).collect()
    }
    /// Copy of an item, for the refresh-address flow.
    pub fn item(&self, id: &str) -> Option<Item> {
        self.inner.lock().state.downloads.iter().find(|d| d.id == id).cloned()
    }
    pub fn snapshot(&self) -> Snapshot {
        let g = self.inner.lock();
        Snapshot {
            downloads: g.state.downloads.iter().map(Summary::from).collect(),
            queues: g.state.queues.clone(),
            settings: g.state.settings.clone(),
            themes: ThemesSnapshot { active: g.state.themes.active.clone(), custom: g.state.themes.custom.clone() },
            token: g.state.token.clone(),
        }
    }
    pub fn file_path(&self, id: &str) -> Option<PathBuf> {
        let g = self.inner.lock();
        let d = g.state.downloads.iter().find(|d| d.id == id)?;
        (d.status == Status::Completed).then(|| d.filename.as_ref().map(|f| Path::new(&d.dir).join(f))).flatten()
    }
    /// The file to show in the folder: the finished file, or the real partial file of an unfinished download.
    pub fn locate_path(&self, id: &str) -> Option<PathBuf> {
        let g = self.inner.lock();
        let d = g.state.downloads.iter().find(|d| d.id == id)?;
        let name = d.filename.as_ref()?;
        if d.status == Status::Completed {
            return Some(Path::new(&d.dir).join(name));
        }
        Some(d.part_path.as_ref().map(PathBuf::from).unwrap_or_else(|| Path::new(&d.dir).join(format!("{name}{PART_EXT}"))))
    }

    // ---- downloads ----------------------------------------------------------------------------------------

    pub fn add(self: &Arc<Self>, req: AddReq) -> AddResult {
        let mut out = AddResult::default();
        {
            let mut g = self.inner.lock();
            let queue_id = g.state.queues.iter().find(|q| Some(&q.id) == req.queue_id.as_ref()).unwrap_or(&g.state.queues[0]).id.clone();
            let list: Vec<&String> = req.urls.iter().take(200).collect();
            let mut target = g.state.settings.download_dir.clone();
            if let Some(d) = &req.dir {
                if Path::new(d).is_absolute() {
                    target = d.clone();
                }
            }
            let referer = req.referer.as_deref().and_then(valid_referer);
            let cookie = req.cookie.as_deref().and_then(valid_cookie);
            let single = list.len() == 1;
            for raw in &list {
                let clipped: String = raw.chars().take(200).collect();
                let Some(u) = http_url(raw) else {
                    out.rejected.push(clipped);
                    continue;
                };
                if raw.len() > 8192 {
                    out.rejected.push(clipped);
                    continue;
                }
                let item = Item {
                    id: new_id("d_"),
                    url: u.to_string(),
                    referer: referer.clone(),
                    cookie: cookie.clone(),
                    dir: target.clone(),
                    queue_id: queue_id.clone(),
                    status: Status::Queued,
                    force: req.start,
                    hold: req.hold && !req.start,
                    filename: if single { req.filename.as_deref().filter(|n| !n.is_empty()).map(sanitize_filename) } else { None },
                    added_at: now_ms(),
                    ..Default::default()
                };
                out.added.push(item.id.clone());
                g.state.downloads.push(item);
            }
        }
        self.changed(out.added.clone(), vec![]);
        self.schedule();
        out
    }

    pub fn pause(&self, ids: &[String]) {
        {
            let mut g = self.inner.lock();
            for id in ids {
                let Some(i) = g.state.downloads.iter().position(|d| &d.id == id) else { continue };
                match g.state.downloads[i].status {
                    Status::Downloading => {
                        if let Some(t) = g.tasks.get(id) {
                            t.ctl.stop(StopReason::Pause);
                        }
                    }
                    Status::Queued => {
                        let d = &mut g.state.downloads[i];
                        d.status = Status::Paused;
                        d.force = false;
                        d.hold = false;
                    }
                    _ => {}
                }
            }
        }
        self.changed(ids.to_vec(), vec![]);
    }

    pub fn pause_all(&self) {
        let ids: Vec<String> = self.inner.lock().state.downloads.iter().filter(|d| matches!(d.status, Status::Downloading | Status::Queued)).map(|d| d.id.clone()).collect();
        self.pause(&ids);
    }

    pub fn resume(self: &Arc<Self>, ids: &[String]) {
        {
            let mut g = self.inner.lock();
            for id in ids {
                if let Some(d) = g.state.downloads.iter_mut().find(|d| &d.id == id) {
                    if matches!(d.status, Status::Paused | Status::Error) {
                        d.status = Status::Queued;
                        d.force = true;
                        d.hold = false;
                        d.error = None;
                    }
                }
            }
        }
        self.changed(ids.to_vec(), vec![]);
        self.schedule();
    }

    /// Take downloads off the list. Finished files are only touched when `delete_files` is true, and then they
    /// go through the trash function (the Recycle Bin in the app), never a silent permanent delete.
    pub async fn remove(self: &Arc<Self>, ids: &[String], delete_files: bool) -> RemoveResult {
        let mut out = RemoveResult::default();
        let mut gone: Vec<String> = vec![];
        for id in ids {
            let running = {
                let g = self.inner.lock();
                if !g.state.downloads.iter().any(|d| &d.id == id) {
                    continue;
                }
                g.tasks.get(id).map(|t| (t.ctl.clone(), t.done.clone()))
            };
            if let Some((ctl, mut done)) = running {
                ctl.stop(StopReason::Cancel);
                while !*done.borrow() {
                    if done.changed().await.is_err() {
                        break;
                    }
                }
            }
            let (unfinished_part, trash_target) = {
                let g = self.inner.lock();
                match g.state.downloads.iter().find(|d| &d.id == id) {
                    None => continue,
                    Some(d) => {
                        let part = (d.status != Status::Completed).then(|| d.part_path.clone()).flatten();
                        let file = (d.status == Status::Completed && delete_files).then(|| d.filename.as_ref().map(|f| Path::new(&d.dir).join(f))).flatten();
                        (part, file)
                    }
                }
            };
            if let Some(p) = unfinished_part {
                let _ = tokio::fs::remove_file(p).await; // the unfinished data
            }
            if let Some(f) = trash_target {
                if tokio::fs::metadata(&f).await.is_ok() {
                    let trash = self.trash.lock().clone();
                    let res = tokio::task::spawn_blocking(move || trash(&f)).await;
                    match res {
                        Ok(Ok(())) => out.deleted += 1,
                        _ => out.failed += 1,
                    }
                }
            }
            self.inner.lock().state.downloads.retain(|d| &d.id != id);
            out.removed += 1;
            gone.push(id.clone());
        }
        self.changed(vec![], gone);
        self.schedule();
        out
    }

    pub fn clear_completed(&self) {
        let ids: Vec<String> = {
            let mut g = self.inner.lock();
            let ids = g.state.downloads.iter().filter(|d| d.status == Status::Completed).map(|d| d.id.clone()).collect();
            g.state.downloads.retain(|d| d.status != Status::Completed);
            ids
        };
        self.changed(vec![], ids);
    }

    pub fn move_to_queue(self: &Arc<Self>, ids: &[String], queue_id: &str) {
        {
            let mut g = self.inner.lock();
            if !g.state.queues.iter().any(|q| q.id == queue_id) {
                return;
            }
            for id in ids {
                if let Some(d) = g.state.downloads.iter_mut().find(|d| &d.id == id) {
                    if d.status != Status::Completed {
                        d.queue_id = queue_id.to_string();
                    }
                }
            }
        }
        self.changed(ids.to_vec(), vec![]);
        self.schedule();
    }

    /// Called right before the app restarts to install an update: remember what was running.
    pub fn mark_resume_on_start(&self) {
        {
            let mut g = self.inner.lock();
            for d in g.state.downloads.iter_mut().filter(|d| d.status == Status::Downloading) {
                d.auto_resume = true;
            }
        }
        self.flush();
    }

    /// Drop finished downloads whose file is gone. Only the LIST entry is removed, never a file. If the drive
    /// itself is unreachable (unplugged USB, offline share) the entry stays, so a missing drive cannot wipe the history.
    pub async fn prune_missing(self: &Arc<Self>) -> Vec<String> {
        let todo: Vec<(String, PathBuf, PathBuf)> = {
            let mut g = self.inner.lock();
            if !g.state.settings.remove_missing || g.pruning {
                return vec![];
            }
            g.pruning = true;
            g.state
                .downloads
                .iter()
                .filter(|d| d.status == Status::Completed)
                .filter_map(|d| d.filename.as_ref().map(|f| (d.id.clone(), Path::new(&d.dir).join(f), PathBuf::from(&d.dir))))
                .collect()
        };
        let mut gone: Vec<String> = vec![];
        for chunk in todo.chunks(40) {
            let checks = chunk.iter().cloned().map(|(id, file, dir)| async move {
                if tokio::fs::metadata(&file).await.is_ok() {
                    return None;
                }
                let root = dir.ancestors().last().map(Path::to_path_buf).unwrap_or(dir);
                if tokio::fs::metadata(&root).await.is_err() {
                    return None; // drive or share unreachable: keep the entry
                }
                Some(id)
            });
            let results = join_all(checks).await;
            gone.extend(results.into_iter().flatten());
        }
        {
            let mut g = self.inner.lock();
            g.pruning = false;
            if !gone.is_empty() {
                let set: std::collections::HashSet<&String> = gone.iter().collect();
                g.state.downloads.retain(|d| !set.contains(&d.id));
            }
        }
        if !gone.is_empty() {
            self.changed(vec![], gone.clone());
            self.emit(Event::Pruned(gone.len()));
        }
        gone
    }

    // ---- refresh download address -------------------------------------------------------------------------

    /// Is the file behind a newly caught link the same file as this (partly downloaded) item?
    pub fn compare_for_refresh(&self, id: &str, info: &engine::Inspect) -> RefreshVerdict {
        let g = self.inner.lock();
        let Some(d) = g.state.downloads.iter().find(|d| d.id == id) else {
            return RefreshVerdict { ok: false, reasons: vec!["That download no longer exists.".into()] };
        };
        let mut reasons = vec![];
        let size_known = d.size > 0 && info.size > 0;
        if size_known && d.size != info.size {
            reasons.push(format!("Different size: expected {}, the new link is {}.", fmt_bytes(d.size as u64), fmt_bytes(info.size as u64)));
        } else if !size_known && d.filename.as_ref().is_some_and(|f| !same_name(f, &info.filename)) {
            reasons.push("Different file name, and the size cannot be compared.".into());
        }
        if reasons.is_empty() && d.downloaded > 0 && !info.resumable {
            reasons.push("The new server does not support resuming, so the download would start again from the beginning.".into());
        }
        RefreshVerdict { ok: reasons.is_empty(), reasons }
    }

    /// Point an unfinished download at a new link. `restart` throws away the partial file.
    pub fn refresh_address(self: &Arc<Self>, id: &str, url: String, referer: Option<String>, cookie: Option<String>, restart: bool, reset_name: bool) -> bool {
        {
            let mut g = self.inner.lock();
            let Some(d) = g.state.downloads.iter_mut().find(|d| d.id == id) else { return false };
            if matches!(d.status, Status::Completed | Status::Downloading) {
                return false;
            }
            d.url = url;
            d.final_url = None;
            if referer.is_some() {
                d.referer = referer;
            }
            if cookie.is_some() {
                d.cookie = cookie;
            }
            if restart {
                if let Some(p) = d.part_path.take() {
                    let _ = std::fs::remove_file(p);
                }
                d.segments = None;
                d.size = 0;
                d.downloaded = 0;
                if reset_name {
                    d.filename = None;
                }
            }
            d.status = Status::Queued;
            d.force = true;
            d.hold = false;
            d.error = None;
            d.note = None;
        }
        self.changed(vec![id.to_string()], vec![]);
        self.schedule();
        true
    }

    // ---- queues -------------------------------------------------------------------------------------------

    pub fn create_queue(&self, name: &str) -> OpResult {
        let n = clean_name(name);
        if n.is_empty() {
            return OpResult::err("Enter a queue name (1-40 characters).");
        }
        let id = {
            let mut g = self.inner.lock();
            if g.state.queues.iter().any(|q| q.name.to_lowercase() == n.to_lowercase()) {
                return OpResult::err("A queue with that name already exists.");
            }
            if g.state.queues.len() >= 30 {
                return OpResult::err("Queue limit reached.");
            }
            let id = new_id("q_");
            g.state.queues.push(Queue { id: id.clone(), name: n, enabled: true });
            id
        };
        self.meta_changed();
        OpResult::ok_id(id)
    }

    pub fn rename_queue(&self, qid: &str, name: &str) -> OpResult {
        if qid == MAIN_QUEUE {
            return OpResult::err("The Main queue is the default queue and cannot be renamed.");
        }
        let n = clean_name(name);
        {
            let mut g = self.inner.lock();
            if !g.state.queues.iter().any(|q| q.id == qid) {
                return OpResult::err("Queue not found.");
            }
            if n.is_empty() {
                return OpResult::err("Enter a queue name (1-40 characters).");
            }
            if g.state.queues.iter().any(|q| q.id != qid && q.name.to_lowercase() == n.to_lowercase()) {
                return OpResult::err("A queue with that name already exists.");
            }
            if let Some(q) = g.state.queues.iter_mut().find(|q| q.id == qid) {
                q.name = n;
            }
        }
        self.meta_changed();
        OpResult::ok()
    }

    pub fn delete_queue(self: &Arc<Self>, qid: &str) -> OpResult {
        if qid == MAIN_QUEUE {
            return OpResult::err("The Main queue is the default queue and cannot be deleted.");
        }
        let moved: Vec<String> = {
            let mut g = self.inner.lock();
            if !g.state.queues.iter().any(|q| q.id == qid) {
                return OpResult::err("Queue not found.");
            }
            let mut moved = vec![];
            for d in g.state.downloads.iter_mut().filter(|d| d.queue_id == qid) {
                d.queue_id = MAIN_QUEUE.into();
                moved.push(d.id.clone());
            }
            g.state.queues.retain(|q| q.id != qid);
            moved
        };
        self.emit(Event::Meta);
        self.changed(moved, vec![]);
        self.schedule();
        OpResult::ok()
    }

    pub fn set_queue_enabled(self: &Arc<Self>, qid: &str, on: bool) {
        if on {
            return self.start_queue(qid);
        }
        {
            let mut g = self.inner.lock();
            let Some(q) = g.state.queues.iter_mut().find(|q| q.id == qid) else { return };
            q.enabled = false;
        }
        self.meta_changed();
    }

    /// Start a queue: let it run, and release anything that was added "without starting".
    pub fn start_queue(self: &Arc<Self>, qid: &str) {
        let released: Vec<String> = {
            let mut g = self.inner.lock();
            let Some(q) = g.state.queues.iter_mut().find(|q| q.id == qid) else { return };
            q.enabled = true;
            let mut released = vec![];
            for d in g.state.downloads.iter_mut().filter(|d| d.queue_id == qid && d.status == Status::Queued && d.hold) {
                d.hold = false;
                released.push(d.id.clone());
            }
            released
        };
        self.emit(Event::Meta);
        self.changed(released, vec![]);
        self.schedule();
    }

    // ---- settings and token -------------------------------------------------------------------------------

    pub fn set_settings(self: &Arc<Self>, p: &Value) {
        let Some(o) = p.as_object() else { return };
        {
            let mut g = self.inner.lock();
            let s = &mut g.state.settings;
            if let Some(v) = o.get("maxSimultaneous") {
                s.max_simultaneous = clamp_int(v, 1, 10, s.max_simultaneous);
            }
            if let Some(v) = o.get("connections") {
                s.connections = clamp_int(v, 1, 16, s.connections);
            }
            if let Some(v) = o.get("bridgePort") {
                s.bridge_port = clamp_int(v, 1024, 65535, s.bridge_port);
            }
            if let Some(k) = o.get("bypassKey").and_then(Value::as_str) {
                if key_ok(k) {
                    s.bypass_key = k.to_string();
                }
            }
            if let Some(d) = o.get("downloadDir").and_then(Value::as_str) {
                if Path::new(d).is_absolute() {
                    s.download_dir = d.to_string();
                }
            }
            for (k, slot) in [
                ("minimizeToTray", &mut s.minimize_to_tray),
                ("runAtStartup", &mut s.run_at_startup),
                ("notify", &mut s.notify),
                ("removeMissing", &mut s.remove_missing),
                ("autoUpdate", &mut s.auto_update),
            ] {
                if let Some(b) = o.get(k).and_then(Value::as_bool) {
                    *slot = b;
                }
            }
        }
        self.meta_changed();
        self.schedule();
    }

    pub fn regenerate_token(&self) -> String {
        let t = new_token();
        self.inner.lock().state.token = t.clone();
        self.meta_changed();
        t
    }

    // ---- themes -------------------------------------------------------------------------------------------

    pub fn all_themes(&self) -> Vec<Theme> {
        let mut v = theme::builtin_themes();
        v.extend(self.inner.lock().state.themes.custom.iter().cloned());
        v
    }

    pub fn active_theme(&self) -> Theme {
        let active = self.inner.lock().state.themes.active.clone();
        let all = self.all_themes();
        all.iter().find(|t| t.id == active).cloned().unwrap_or_else(|| all[0].clone())
    }

    pub fn theme_save(&self, input: &Value) -> OpResult {
        let mut body = input.clone();
        let tid = body.as_object_mut().and_then(|o| o.remove("id")).and_then(|v| v.as_str().map(str::to_string));
        let t = match theme::validate_theme(&body) {
            Ok(t) => t,
            Err(e) => return OpResult::err(e),
        };
        let id = {
            let mut g = self.inner.lock();
            let existing = tid.as_ref().and_then(|tid| g.state.themes.custom.iter().position(|x| &x.id == tid));
            let id = match existing {
                Some(i) => {
                    let id = g.state.themes.custom[i].id.clone();
                    g.state.themes.custom[i] = Theme { id: id.clone(), ..t };
                    id
                }
                None => {
                    if g.state.themes.custom.len() >= 50 {
                        return OpResult::err("Theme limit reached (50).");
                    }
                    let id = new_id("t_");
                    g.state.themes.custom.push(Theme { id: id.clone(), ..t });
                    id
                }
            };
            g.state.themes.active = id.clone();
            id
        };
        self.meta_changed();
        OpResult::ok_id(id)
    }

    pub fn theme_apply(&self, id: &str) {
        if !self.all_themes().iter().any(|t| t.id == id) {
            return;
        }
        self.inner.lock().state.themes.active = id.to_string();
        self.meta_changed();
    }

    pub fn theme_delete(&self, id: &str) {
        {
            let mut g = self.inner.lock();
            let Some(i) = g.state.themes.custom.iter().position(|t| t.id == id) else { return };
            g.state.themes.custom.remove(i);
            if g.state.themes.active == id {
                g.state.themes.active = "dark".into();
            }
        }
        self.meta_changed();
    }

    /// `(suggested file name, file text)` for exporting a theme.
    pub fn theme_export(&self, id: &str) -> Option<(String, String)> {
        let t = self.all_themes().into_iter().find(|t| t.id == id)?;
        let safe: String = t.name.chars().filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ' ')).collect();
        let safe = safe.trim();
        let base = if safe.is_empty() { "theme" } else { safe };
        Some((format!("{base}.dantheme"), theme::serialize_theme(&t)))
    }

    pub fn theme_import_text(&self, text: &str) -> OpResult {
        let t = match theme::parse_theme_text(text) {
            Ok(t) => t,
            Err(e) => return OpResult::err(e),
        };
        let (id, name) = {
            let mut g = self.inner.lock();
            if g.state.themes.custom.len() >= 50 {
                return OpResult::err("Theme limit reached (50).");
            }
            let id = new_id("t_");
            let name = t.name.clone();
            g.state.themes.custom.push(Theme { id: id.clone(), ..t });
            (id, name)
        };
        self.meta_changed();
        OpResult { ok: true, id: Some(id), name: Some(name), error: None }
    }

    // ---- scheduler ----------------------------------------------------------------------------------------

    /// Start as many waiting downloads as the "simultaneous downloads" setting allows.
    pub fn schedule(self: &Arc<Self>) {
        let mut jobs: Vec<(String, JobSpec, Control, watch::Sender<bool>)> = vec![];
        let mut started: Vec<String> = vec![];
        {
            let mut g = self.inner.lock();
            let max = g.state.settings.max_simultaneous.max(1) as usize;
            let connections = g.state.settings.connections.max(1) as usize;
            let active = g.tasks.len();
            if active >= max {
                return;
            }
            let order: HashMap<&str, usize> = g.state.queues.iter().enumerate().map(|(n, q)| (q.id.as_str(), n)).collect();
            let enabled: HashMap<&str, bool> = g.state.queues.iter().map(|q| (q.id.as_str(), q.enabled)).collect();
            let mut next: Vec<(bool, usize, i64, String)> = g
                .state
                .downloads
                .iter()
                .filter(|d| d.status == Status::Queued && !d.hold && (d.force || enabled.get(d.queue_id.as_str()).copied().unwrap_or(false)))
                .map(|d| (d.force, order.get(d.queue_id.as_str()).copied().unwrap_or(99), d.added_at, d.id.clone()))
                .collect();
            next.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
            let ids: Vec<String> = next.into_iter().map(|t| t.3).take(max - active).collect();
            for id in ids {
                let Some(d) = g.state.downloads.iter_mut().find(|d| d.id == id) else { continue };
                d.status = Status::Downloading;
                d.error = None;
                let resume = match (&d.segments, &d.part_path) {
                    (Some(segs), Some(part)) if !segs.is_empty() => Some(Resume {
                        final_url: d.final_url.clone(),
                        size: d.size,
                        accept_ranges: d.accept_ranges,
                        part_path: PathBuf::from(part),
                        filename: d.filename.clone().unwrap_or_default(),
                        segments: segs.clone(),
                    }),
                    _ => None,
                };
                let spec = JobSpec {
                    url: d.url.clone(),
                    referer: d.referer.clone(),
                    cookie: d.cookie.clone(),
                    dir: PathBuf::from(&d.dir),
                    filename: d.filename.clone(),
                    connections,
                    resume,
                };
                let ctl = Control::new();
                let (tx, rx) = watch::channel(false);
                g.tasks.insert(id.clone(), Running { ctl: ctl.clone(), done: rx, last_save: 0 });
                started.push(id.clone());
                jobs.push((id, spec, ctl, tx));
            }
        }
        if jobs.is_empty() {
            return;
        }
        self.changed(started, vec![]);
        for (id, spec, ctl, tx) in jobs {
            let me = self.clone();
            let client = self.client.clone();
            self.rt.spawn(async move {
                let sink = {
                    let me = me.clone();
                    let id = id.clone();
                    Arc::new(move |u: Update| me.apply_update(&id, u))
                };
                let outcome = engine::run(client, spec, ctl, sink).await;
                me.finish(&id, outcome);
                let _ = tx.send(true);
            });
        }
    }

    fn apply_update(&self, id: &str, u: Update) {
        let mut save_now = false;
        {
            let mut g = self.inner.lock();
            let now = now_ms();
            let Some(d) = g.state.downloads.iter_mut().find(|d| d.id == id) else { return };
            match u {
                Update::Note(n) => d.note = n,
                Update::Started { final_url, size, accept_ranges, filename, part_path, segments } => {
                    d.final_url = Some(final_url);
                    d.size = size;
                    d.accept_ranges = accept_ranges;
                    d.filename = Some(filename);
                    d.part_path = Some(part_path);
                    d.segments = Some(segments);
                    save_now = true;
                }
                Update::Progress { downloaded, speed, eta, segments, clear_note } => {
                    d.downloaded = downloaded;
                    d.speed = speed;
                    d.eta = eta;
                    d.segments = Some(segments);
                    if clear_note {
                        d.note = None;
                    }
                }
            }
            if let Some(t) = g.tasks.get_mut(id) {
                if now - t.last_save > 3000 {
                    t.last_save = now; // keeps the resume data fresh without writing the file twice a second
                    save_now = true;
                }
            }
        }
        self.emit(Event::Patch { ids: vec![id.to_string()], removed: vec![] });
        if save_now {
            self.save();
        }
    }

    fn finish(self: &Arc<Self>, id: &str, outcome: Outcome) {
        let mut completed: Option<Summary> = None;
        {
            let mut g = self.inner.lock();
            g.tasks.remove(id);
            if let Some(d) = g.state.downloads.iter_mut().find(|d| d.id == id) {
                d.speed = 0;
                d.eta = None;
                d.note = None;
                match outcome {
                    Outcome::Completed { filename, size, downloaded } => {
                        d.status = Status::Completed;
                        d.cookie = None;
                        d.force = false;
                        d.segments = None;
                        d.part_path = None;
                        d.filename = Some(filename);
                        d.size = size;
                        d.downloaded = downloaded;
                        d.completed_at = Some(now_ms());
                        completed = Some(Summary::from(&*d));
                    }
                    Outcome::Paused { downloaded, segments } => {
                        d.status = Status::Paused;
                        d.force = false;
                        d.downloaded = downloaded;
                        if segments.is_some() {
                            d.segments = segments;
                        }
                    }
                    Outcome::Cancelled => {}
                    Outcome::Error { message, restart, downloaded, segments } => {
                        d.status = Status::Error;
                        d.error = Some(message);
                        d.force = false;
                        d.downloaded = downloaded;
                        if restart {
                            d.segments = None;
                            d.downloaded = 0;
                            if let Some(p) = d.part_path.take() {
                                let _ = std::fs::remove_file(p);
                            }
                        } else if segments.is_some() {
                            d.segments = segments;
                        }
                    }
                }
            }
        }
        if let Some(s) = completed {
            self.emit(Event::Completed(s));
        }
        self.changed(vec![id.to_string()], vec![]);
        self.schedule();
    }

    /// Stop everything for quitting: running downloads pause (their progress is kept) and the state is saved.
    pub async fn shutdown(&self) {
        let waits: Vec<(Control, watch::Receiver<bool>)> = {
            let g = self.inner.lock();
            g.tasks.values().map(|t| (t.ctl.clone(), t.done.clone())).collect()
        };
        for (ctl, _) in &waits {
            ctl.stop(StopReason::Pause);
        }
        let all = async {
            for (_, mut done) in waits {
                while !*done.borrow() {
                    if done.changed().await.is_err() {
                        break;
                    }
                }
            }
        };
        let _ = tokio::time::timeout(Duration::from_millis(2500), all).await;
        {
            let mut g = self.inner.lock();
            for d in g.state.downloads.iter_mut().filter(|d| d.status == Status::Downloading) {
                d.status = Status::Paused;
                d.speed = 0;
                d.eta = None;
            }
        }
        self.flush();
    }
}

async fn join_all<F: std::future::Future>(futs: impl IntoIterator<Item = F>) -> Vec<F::Output> {
    // The checks are tiny filesystem lookups, so they are simply awaited one after another.
    let mut out = vec![];
    for f in futs {
        out.push(f.await);
    }
    out
}

/// Read the state file. A file that exists but cannot be read is kept aside (never silently overwritten).
fn load_state(path: &Path) -> State {
    let Ok(bytes) = std::fs::read(path) else { return State::default() };
    match serde_json::from_slice::<State>(&bytes) {
        Ok(s) => s,
        Err(_) => {
            let aside = path.with_extension(format!("corrupt-{}.json", now_ms()));
            let _ = std::fs::rename(path, aside);
            State::default()
        }
    }
}

/// Fix up anything an older or hand-edited file could contain, and tidy what was running when the app closed.
fn normalise(state: &mut State, default_dir: &str) {
    let s = &mut state.settings;
    if s.download_dir.is_empty() || !Path::new(&s.download_dir).is_absolute() {
        s.download_dir = default_dir.to_string();
    }
    s.max_simultaneous = s.max_simultaneous.clamp(1, 10);
    s.connections = s.connections.clamp(1, 16);
    s.bridge_port = s.bridge_port.clamp(1024, 65535);
    if !key_ok(&s.bypass_key) {
        s.bypass_key = "F8".into();
    }
    if s.bypass_key_v != 3 {
        // the default key changed twice (Alt, then Scroll Lock, now F8): move people still on an old default
        if s.bypass_key == "Alt" || s.bypass_key == "ScrollLock" {
            s.bypass_key = "F8".into();
        }
        s.bypass_key_v = 3;
    }
    if state.token.is_empty() {
        state.token = new_token();
    }
    if !state.queues.iter().any(|q| q.id == MAIN_QUEUE) {
        state.queues.insert(0, Queue { id: MAIN_QUEUE.into(), name: MAIN_NAME.into(), enabled: true });
    }
    for q in state.queues.iter_mut() {
        if q.id == MAIN_QUEUE {
            q.name = MAIN_NAME.into(); // the default queue always has this fixed name
        }
    }
    for q in state.queues.iter_mut() {
        if q.id != MAIN_QUEUE && q.name.to_lowercase() == MAIN_NAME.to_lowercase() {
            q.name = "Main (2)".into(); // a user queue that already used the name "Main" is renamed
        }
    }
    let queue_ids: Vec<String> = state.queues.iter().map(|q| q.id.clone()).collect();
    for d in state.downloads.iter_mut() {
        if !queue_ids.contains(&d.queue_id) {
            d.queue_id = MAIN_QUEUE.into();
        }
        if d.status == Status::Downloading {
            d.status = Status::Paused; // it was running when the app closed
            d.speed = 0;
            d.eta = None;
        }
        if d.auto_resume {
            d.auto_resume = false; // set just before an update restart: carry on automatically
            if d.status == Status::Paused {
                d.status = Status::Queued;
                d.force = true;
            }
        }
    }
}
