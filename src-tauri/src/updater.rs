//! Update checker. Flow: check -> "available" -> the user clicks Download -> "downloading" -> "ready" -> the user
//! clicks Restart. Nothing is downloaded or installed without the user's click.
//!
//! Every update is signed with a private key that only the project owner has; the app refuses anything that does not
//! verify against the public key in tauri.conf.json (plugins.updater.pubkey).

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UpState {
    pub state: String,
    pub current: String,
    pub version: Option<String>,
    pub percent: u8,
    pub last_checked: Option<i64>,
    pub message: String,
}

#[derive(Default)]
pub struct UpdaterState {
    st: Mutex<Option<UpState>>,
    update: Mutex<Option<Update>>,
    bytes: Mutex<Option<Vec<u8>>>,
    usable: Mutex<bool>,
}

fn friendly(e: &str) -> String {
    let l = e.to_lowercase();
    if l.contains("404") || l.contains("not found") || l.contains("could not fetch") && l.contains("json") || l.contains("valid release json") {
        return "No release found yet. Publish a release on the GitHub releases page first.".into();
    }
    if l.contains("dns") || l.contains("timed out") || l.contains("connect") || l.contains("network") || l.contains("error sending request") {
        return "Could not reach GitHub. Check your internet connection.".into();
    }
    if l.contains("signature") || l.contains("pubkey") || l.contains("public key") {
        return "The update failed its signature check and was discarded.".into();
    }
    e.lines().next().unwrap_or("Update check failed.").chars().take(200).collect()
}

fn pubkey_configured(app: &AppHandle) -> bool {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|v| v.get("pubkey"))
        .and_then(|v| v.as_str())
        .is_some_and(|k| !k.is_empty() && !k.starts_with("REPLACE"))
}

impl UpdaterState {
    pub fn init(&self, app: &AppHandle) {
        let current = app.package_info().version.to_string();
        let (usable, message) = if cfg!(debug_assertions) {
            (false, "Updates work in the installed version of DownloaDan (not when started with npm run tauri dev).".to_string())
        } else if !pubkey_configured(app) {
            (false, "Updates are not set up yet: put your update public key in tauri.conf.json.".to_string())
        } else {
            (true, String::new())
        };
        *self.usable.lock() = usable;
        *self.st.lock() = Some(UpState { state: if usable { "idle" } else { "disabled" }.into(), current, version: None, percent: 0, last_checked: None, message });
    }

    pub fn state(&self) -> UpState {
        self.st.lock().clone().unwrap_or(UpState { state: "disabled".into(), current: String::new(), version: None, percent: 0, last_checked: None, message: String::new() })
    }

    fn set(&self, app: &AppHandle, f: impl FnOnce(&mut UpState)) -> UpState {
        let s = {
            let mut g = self.st.lock();
            let mut s = g.clone().unwrap_or_else(|| self.state());
            f(&mut s);
            *g = Some(s.clone());
            s
        };
        let _ = app.emit_to("main", "update:state", s.clone());
        s
    }

    /// Returns the version when a new update has just been found.
    pub async fn check(&self, app: &AppHandle) -> Option<String> {
        if !*self.usable.lock() || matches!(self.state().state.as_str(), "checking" | "downloading" | "ready") {
            return None;
        }
        self.set(app, |s| {
            s.state = "checking".into();
            s.message.clear();
        });
        let now = downloadan_core::util::now_ms();
        let updater = match app.updater() {
            Ok(u) => u,
            Err(e) => {
                self.set(app, |s| {
                    s.state = "error".into();
                    s.message = friendly(&e.to_string());
                    s.last_checked = Some(now);
                });
                return None;
            }
        };
        match updater.check().await {
            Ok(Some(update)) => {
                let v = update.version.clone();
                *self.update.lock() = Some(update);
                self.set(app, |s| {
                    s.state = "available".into();
                    s.version = Some(v.clone());
                    s.message.clear();
                    s.last_checked = Some(now);
                });
                Some(v)
            }
            Ok(None) => {
                self.set(app, |s| {
                    s.state = "none".into();
                    s.version = None;
                    s.message.clear();
                    s.last_checked = Some(now);
                });
                None
            }
            Err(e) => {
                self.set(app, |s| {
                    s.state = "error".into();
                    s.message = friendly(&e.to_string());
                    s.last_checked = Some(now);
                });
                None
            }
        }
    }

    pub async fn download(&self, app: &AppHandle) {
        if !*self.usable.lock() || self.state().state != "available" {
            return;
        }
        let Some(update) = self.update.lock().clone() else { return };
        self.set(app, |s| {
            s.state = "downloading".into();
            s.percent = 0;
            s.message.clear();
        });
        let mut got: u64 = 0;
        let mut last_pct: u8 = 0;
        let app2 = app.clone();
        let result = update
            .download(
                |chunk, total| {
                    got += chunk as u64;
                    if let Some(total) = total.filter(|t| *t > 0) {
                        let pct = ((got as f64 / total as f64) * 100.0).round().clamp(0.0, 100.0) as u8;
                        if pct != last_pct {
                            last_pct = pct;
                            let st = app2.state::<std::sync::Arc<crate::state::AppState>>();
                            st.updater.set(&app2, |s| s.percent = pct);
                        }
                    }
                },
                || {},
            )
            .await;
        match result {
            Ok(bytes) => {
                *self.bytes.lock() = Some(bytes);
                let v = update.version.clone();
                self.set(app, |s| {
                    s.state = "ready".into();
                    s.version = Some(v);
                    s.percent = 100;
                    s.message.clear();
                });
            }
            Err(e) => {
                self.set(app, |s| {
                    s.state = "available".into();
                    s.message = friendly(&e.to_string());
                });
            }
        }
    }

    /// Install the downloaded update. On Windows the installer closes the app and starts it again.
    pub fn install(&self, app: &AppHandle) -> bool {
        if !*self.usable.lock() || self.state().state != "ready" {
            return false;
        }
        let (Some(update), Some(bytes)) = (self.update.lock().clone(), self.bytes.lock().clone()) else { return false };
        let st = app.state::<std::sync::Arc<crate::state::AppState>>();
        st.quitting.store(true, std::sync::atomic::Ordering::SeqCst);
        st.mgr.mark_resume_on_start(); // unfinished downloads carry on by themselves after the restart
        match update.install(bytes) {
            Ok(()) => true,
            Err(e) => {
                st.quitting.store(false, std::sync::atomic::Ordering::SeqCst);
                self.set(app, |s| {
                    s.state = "error".into();
                    s.message = friendly(&e.to_string());
                });
                false
            }
        }
    }
}
