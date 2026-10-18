//! Shared application state and the plumbing that tells the windows what changed.

use downloadan_core::bridge::{Bridge, BridgeStatus};
use downloadan_core::api::Api;
use downloadan_core::engine::Inspect;
use downloadan_core::manager::Manager;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

/// Keep in sync with PORTS in extension/bridge-client.js
pub const PORT_RANGE: std::ops::RangeInclusive<u16> = 18642..=18651;

/// A link caught by the browser extension.
#[derive(Clone, Debug)]
pub struct Caught {
    pub url: String,
    pub referer: Option<String>,
    pub cookie: Option<String>,
}

pub struct PromptItem {
    pub pid: String,
    pub caught: Caught,
}

#[derive(Default)]
pub struct PromptState {
    pub cur: Option<PromptItem>,
    pub waiting: VecDeque<PromptItem>,
}

/// "Refresh download address": waiting for the browser to catch the file again.
pub struct RefreshWait {
    pub id: String,
    pub token: u64,
    pub seq: u64,
    pub pending: Option<(Caught, Inspect)>,
    pub timeout: Option<tauri::async_runtime::JoinHandle<()>>,
}

#[derive(Default)]
struct PatchBuf {
    ids: HashSet<String>,
    removed: HashSet<String>,
    scheduled: bool,
}

/// Sends events to the main window. Download progress is batched (a quarter of a second) so a busy queue
/// does not flood the UI.
pub struct Notifier {
    pub app: AppHandle,
    pub ready: AtomicBool,
    pub mgr: OnceLock<Arc<Manager>>,
    buf: Mutex<PatchBuf>,
}

#[derive(Serialize, Clone)]
struct PatchPayload {
    items: Vec<downloadan_core::model::Summary>,
    removed: Vec<String>,
}

impl Notifier {
    pub fn new(app: AppHandle) -> Arc<Notifier> {
        Arc::new(Notifier { app, ready: AtomicBool::new(false), mgr: OnceLock::new(), buf: Mutex::new(PatchBuf::default()) })
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    pub fn send<S: Serialize + Clone>(&self, event: &str, payload: S) {
        if self.is_ready() {
            let _ = self.app.emit_to("main", event, payload);
        }
    }

    pub fn patch(self: &Arc<Self>, ids: Vec<String>, removed: Vec<String>) {
        {
            let mut b = self.buf.lock();
            b.ids.extend(ids);
            b.removed.extend(removed);
            if b.scheduled {
                return;
            }
            b.scheduled = true;
        }
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let (ids, removed) = {
                let mut b = me.buf.lock();
                b.scheduled = false;
                (std::mem::take(&mut b.ids), std::mem::take(&mut b.removed))
            };
            if let Some(mgr) = me.mgr.get() {
                let ids: Vec<String> = ids.into_iter().collect();
                me.send("patch", PatchPayload { items: mgr.summaries(&ids), removed: removed.into_iter().collect() });
            }
        });
    }
}

pub struct AppState {
    pub mgr: Arc<Manager>,
    pub api: Arc<Api>,
    pub bridge: Arc<Bridge>,
    pub notifier: Arc<Notifier>,
    pub bridge_status: Mutex<BridgeStatus>,
    pub quitting: AtomicBool,
    pub prompts: Mutex<PromptState>,
    pub refresh: Mutex<Option<RefreshWait>>,
    pub refresh_counter: AtomicU64,
    pub updater: crate::updater::UpdaterState,
    pub manual_check: AtomicBool,
    pub notified_version: Mutex<Option<String>>,
}
