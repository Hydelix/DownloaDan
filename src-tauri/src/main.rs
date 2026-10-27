// No console window behind the app on Windows release builds.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod commands;
mod state;
mod updater;
mod windows;

use downloadan_core::api::{Api, ApiConfig, ALLOWED_EXTENSION_IDS};
use downloadan_core::bridge::{Bridge, BridgeStatus};
use downloadan_core::manager::{Event, Manager};
use parking_lot::Mutex;
use serde_json::json;
use state::{AppState, Caught, Notifier, PromptState};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager as _, RunEvent, WindowEvent};

/// Close everything cleanly: downloads pause (their progress is kept), state is saved, then the app exits.
fn quit(app: &AppHandle) {
    let st = app.state::<Arc<AppState>>().inner().clone();
    if st.quitting.swap(true, Ordering::SeqCst) {
        return;
    }
    windows::save_geometry(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        st.bridge.close();
        st.mgr.shutdown().await;
        app.exit(0);
    });
}

/// One-time carry-over from the Electron version: keeps downloads, queues, settings, themes and the browser pairing token.
fn import_old_state(app: &AppHandle, target: &Path) {
    if target.exists() {
        return;
    }
    let Ok(roaming) = app.path().data_dir() else { return };
    for old in ["DownloaDan", "FluxDM"] {
        let candidate = roaming.join(old).join("state.json");
        if candidate.exists() {
            if let Some(dir) = target.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::copy(&candidate, target);
            return;
        }
    }
}

fn build_state(app: &AppHandle) -> Result<Arc<AppState>, Box<dyn std::error::Error>> {
    let data_dir: PathBuf = app.path().app_data_dir()?;
    let state_file = data_dir.join("state.json");
    import_old_state(app, &state_file);
    let default_dir = app.path().download_dir().unwrap_or_else(|_| data_dir.join("Downloads")).to_string_lossy().into_owned();
    let rt = tauri::async_runtime::handle().inner().clone();

    let notifier = Notifier::new(app.clone());
    let events_notifier = notifier.clone();
    let mgr = Manager::new(
        state_file,
        default_dir,
        rt.clone(),
        Arc::new(move |e| match e {
            Event::Patch { ids, removed } => events_notifier.patch(ids, removed),
            Event::Meta => {
                if let Some(mgr) = events_notifier.mgr.get() {
                    events_notifier.send("meta", mgr.snapshot());
                    sync_limit_item(mgr);
                }
                windows::apply_chrome(&events_notifier.app);
            }
            Event::Pruned(n) => {
                let text = format!("Removed {n} download{} whose file was moved or deleted", if n == 1 { "" } else { "s" });
                events_notifier.send("notice", text);
            }
            Event::Completed(s) => {
                if events_notifier.mgr.get().is_some_and(|m| m.settings().notify) {
                    commands::notify(&events_notifier.app, "Download complete", s.filename.as_deref().unwrap_or(&s.url));
                }
            }
        }),
    );
    let _ = notifier.mgr.set(mgr.clone());
    mgr.set_trash(Arc::new(|p: &Path| trash::delete(p).map_err(|e| e.to_string()))); // deleted files go to the Recycle Bin

    // The browser extension talks to the app through the Api; the bridge is only its HTTP transport.
    let (m1, m2) = (mgr.clone(), mgr.clone());
    let app_for_add = app.clone();
    let app_for_pair = app.clone();
    let app_for_pair_end = app.clone();
    let api = Api::new(ApiConfig {
        get_token: Box::new(move || m1.token()),
        get_config: Box::new(move || json!({ "bypassKey": m2.bypass_key() })),
        on_add: Box::new(move |r| {
            let st = app_for_add.state::<Arc<AppState>>().inner().clone();
            commands::prompt_add(&app_for_add, &st, Caught { url: r.url, referer: r.referer, cookie: r.cookie });
        }),
        on_pair_request: Box::new(move |_| windows::open_pair(&app_for_pair)),
        on_pair_end: Box::new(move |_| windows::close_window(&app_for_pair_end, "pair")),
        version: app.package_info().version.to_string(),
        allowed_ids: ALLOWED_EXTENSION_IDS.iter().map(|s| s.to_string()).collect(),
        now: Box::new(downloadan_core::util::now_ms),
    });
    let bridge = Bridge::new(api.clone(), rt);

    let updater = updater::UpdaterState::default();
    updater.init(app);
    Ok(Arc::new(AppState {
        mgr,
        api,
        bridge,
        notifier,
        bridge_status: Mutex::new(BridgeStatus { ok: false, port: None, error: Some("Not started".into()) }),
        quitting: AtomicBool::new(false),
        prompts: Mutex::new(PromptState::default()),
        refresh: Mutex::new(None),
        refresh_counter: AtomicU64::new(0),
        updater,
        manual_check: AtomicBool::new(false),
        notified_version: Mutex::new(None),
    }))
}

/// The "Limit download speed" tray item, kept so it can follow changes made in Settings.
static LIMIT_ITEM: OnceLock<CheckMenuItem<tauri::Wry>> = OnceLock::new();

fn limit_label(kbps: i64) -> String {
    if kbps >= 1024 && kbps % 1024 == 0 {
        format!("Limit download speed ({} MB/s)", kbps / 1024)
    } else if kbps >= 1024 {
        format!("Limit download speed ({:.1} MB/s)", kbps as f64 / 1024.0)
    } else {
        format!("Limit download speed ({kbps} KB/s)")
    }
}

fn sync_limit_item(mgr: &Manager) {
    if let Some(item) = LIMIT_ITEM.get() {
        let s = mgr.settings();
        let _ = item.set_checked(s.speed_limit_on);
        let _ = item.set_text(limit_label(s.speed_limit_kbps));
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open DownloaDan", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "Pause all downloads", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let limits = app.state::<Arc<AppState>>().mgr.settings();
    let limit = CheckMenuItem::with_id(app, "limit", limit_label(limits.speed_limit_kbps), true, limits.speed_limit_on, None::<&str>)?;
    let _ = LIMIT_ITEM.set(limit.clone());
    let menu = Menu::with_items(app, &[&open, &pause, &limit, &PredefinedMenuItem::separator(app)?, &quit_item])?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id("tray")
        .icon(icon)
        .tooltip("DownloaDan")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => windows::show_main(app),
            "pause" => app.state::<Arc<AppState>>().mgr.pause_all(),
            "limit" => {
                app.state::<Arc<AppState>>().mgr.toggle_speed_limit();
            }
            "quit" => quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                windows::show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Checks for updates shortly after start and then every six hours, if the user allows it.
fn start_update_timer(app: &AppHandle, st: &Arc<AppState>) {
    let (app, st) = (app.clone(), st.clone());
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(8)).await;
        loop {
            if st.mgr.settings().auto_update && !st.manual_check.load(Ordering::SeqCst) {
                if let Some(version) = st.updater.check(&app).await {
                    let first_time = {
                        let mut g = st.notified_version.lock();
                        let fresh = g.as_deref() != Some(version.as_str());
                        *g = Some(version.clone());
                        fresh
                    };
                    if first_time {
                        commands::notify(&app, "DownloaDan update available", &format!("Version {version} is ready to download."));
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
        }
    });
}

fn main() {
    let builder = tauri::Builder::default()
        // must be first: a second launch just brings the running app to the front
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| windows::show_main(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--hidden"])))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::state_get, commands::clip_read, commands::clip_write,
            commands::dl_add, commands::dl_pause, commands::dl_pause_all, commands::dl_resume, commands::dl_remove,
            commands::dl_clear_completed, commands::dl_move, commands::dl_open, commands::dl_show,
            commands::q_create, commands::q_rename, commands::q_delete, commands::q_enable, commands::q_start,
            commands::update_get, commands::update_check, commands::update_download, commands::update_install,
            commands::refresh_start, commands::refresh_cancel, commands::refresh_url, commands::refresh_decide,
            commands::settings_set, commands::dir_choose, commands::token_regen,
            commands::theme_validate, commands::theme_save, commands::theme_apply, commands::theme_delete,
            commands::theme_export, commands::theme_import,
            commands::prompt_init, commands::prompt_probe, commands::prompt_free, commands::prompt_dir,
            commands::prompt_confirm, commands::prompt_cancel,
            commands::pair_init, commands::pair_answer,
        ])
        .on_window_event(|window, event| {
            let app = window.app_handle();
            let Some(st) = app.try_state::<Arc<AppState>>() else { return };
            let st = st.inner().clone();
            match (window.label(), event) {
                ("main", WindowEvent::CloseRequested { api, .. }) => {
                    windows::save_geometry(app);
                    if !st.mgr.settings().minimize_to_tray && !st.quitting.load(Ordering::SeqCst) {
                        api.prevent_close();
                        quit(app);
                    }
                    // otherwise the window is simply destroyed (its memory is released) and the app stays in the tray
                }
                ("main", WindowEvent::Resized(_)) => windows::track(window),
                ("main", WindowEvent::Focused(false)) => windows::save_geometry(app),
                ("main", WindowEvent::Destroyed) => st.notifier.ready.store(false, Ordering::SeqCst),
                ("main", WindowEvent::Focused(true)) => {
                    let mgr = st.mgr.clone();
                    tauri::async_runtime::spawn(async move {
                        mgr.prune_missing().await;
                    });
                }
                ("prompt", WindowEvent::Destroyed) => commands::prompt_closed(app, &st),
                ("pair", WindowEvent::Destroyed) => {
                    // closing the box counts as Deny
                    if let Some(p) = st.api.pending_pairing() {
                        st.api.resolve_pairing(&p.id, false);
                    }
                }
                _ => {}
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            let st = build_state(&handle)?;
            app.manage(st.clone());
            build_tray(&handle)?;

            let start_hidden = std::env::args().any(|a| a == "--hidden");
            {
                let st = st.clone();
                tauri::async_runtime::spawn(async move {
                    commands::start_bridge(&st).await;
                    st.mgr.schedule();
                    st.mgr.prune_missing().await;
                });
            }
            // While the window is open, notice finished files that were moved or deleted.
            {
                let (handle, st) = (handle.clone(), st.clone());
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        if handle.get_webview_window("main").is_some() {
                            st.mgr.prune_missing().await;
                        }
                    }
                });
            }
            start_update_timer(&handle, &st);
            if !start_hidden {
                windows::show_main(&handle);
            }
            Ok(())
        });

    builder
        .build(tauri::generate_context!())
        .expect("error while building DownloaDan")
        .run(|_app, event| {
            // Closing the last window must not end the app: it lives on in the tray. Quitting goes through `quit()`.
            if let RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
