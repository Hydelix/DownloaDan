//! The app's windows: the main window, and the two small always-on-top boxes ("New download" and
//! "Connect browser") that exist only while they are needed.

use crate::state::AppState;
use std::sync::Arc;
use tauri::window::Color;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub fn hex_color(hex: &str) -> Color {
    let p = |i: usize| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    Color(p(1), p(3), p(5), 255)
}

fn state(app: &AppHandle) -> Arc<AppState> {
    app.state::<Arc<AppState>>().inner().clone()
}

fn base<'a>(app: &'a AppHandle, label: &str, page: &str, title: &str) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    let bg = hex_color(&state(app).mgr.active_theme().colors.bg);
    WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        .title(title)
        .decorations(false) // the app draws its own title bar so it matches the theme
        .visible(false) // shown once the page has loaded, so there is no white flash
        .background_color(bg)
}

/// Open the main window, or bring the existing one to the front.
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let st = state(app);
    st.notifier.ready.store(false, std::sync::atomic::Ordering::SeqCst);
    match base(app, "main", "index.html", "DownloaDan").inner_size(1080.0, 700.0).min_inner_size(860.0, 540.0).resizable(true).center().build() {
        Ok(w) => {
            // safety net: never leave the window hidden if the page fails to report in
            let w2 = w.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                let _ = w2.show();
            });
        }
        Err(e) => eprintln!("Could not open the main window: {e}"),
    }
}

/// Match the window's own background to the active theme.
pub fn apply_chrome(app: &AppHandle) {
    let bg = hex_color(&state(app).mgr.active_theme().colors.bg);
    for label in ["main", "prompt", "pair"] {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.set_background_color(Some(bg));
        }
    }
}

pub fn open_prompt(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("prompt") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let r = base(app, "prompt", "prompt.html", "DownloaDan: New download")
        .inner_size(500.0, 560.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .always_on_top(true)
        .center()
        .build();
    if let Err(e) = r {
        eprintln!("Could not open the New download box: {e}");
    }
}

pub fn open_pair(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("pair") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let r = base(app, "pair", "pair.html", "DownloaDan: Connect browser")
        .inner_size(460.0, 400.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .always_on_top(true)
        .center()
        .build();
    if let Err(e) = r {
        eprintln!("Could not open the Connect browser box: {e}");
    }
}

pub fn close_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        let _ = w.close();
    }
}

/// Show a small window once its page is ready (called from the page's first command).
pub fn reveal(w: &WebviewWindow) {
    let _ = w.show();
    let _ = w.set_focus();
}
