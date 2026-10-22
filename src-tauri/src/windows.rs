//! The app's windows: the main window, and the two small always-on-top boxes ("New download" and
//! "Connect browser") that exist only while they are needed.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::window::Color;
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Window};

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

// ---- remembering where the main window was ----
// Position and size are kept in `window.json` next to state.json. The window is only ever tracked in memory while it
// is open (cheap), and written to disk when it closes, loses focus, or the app quits.

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Geometry {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    #[serde(default)]
    maximized: bool,
}

/// The last good geometry of the main window. For a maximized window this is its normal (restored) size and position.
static LAST: Mutex<Option<Geometry>> = Mutex::new(None);

/// Set while a freshly opened window is being put back where it was. Nothing is remembered until it has settled,
/// so a half-finished resize can never be saved as the window's position.
static TARGET: Mutex<Option<Geometry>> = Mutex::new(None);
static SETTLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn get_target() -> Option<Geometry> {
    *TARGET.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_target(g: Option<Geometry>) {
    *TARGET.lock().unwrap_or_else(|e| e.into_inner()) = g;
}

fn geometry_file(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("window.json"))
}

fn load_geometry(app: &AppHandle) -> Option<Geometry> {
    let bytes = std::fs::read(geometry_file(app)?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The scale factor of the monitor the saved window sits on, or None if it would end up off-screen
/// (monitor unplugged, resolution changed), in which case the window is simply centred.
fn scale_for(app: &AppHandle, g: &Geometry) -> Option<f64> {
    let monitors = app.available_monitors().ok()?;
    monitors
        .iter()
        .find(|m| {
            let (p, s) = (m.position(), m.size());
            let overlap_x = (g.x + g.w as i32).min(p.x + s.width as i32) - g.x.max(p.x);
            let overlap_y = (g.y + g.h as i32).min(p.y + s.height as i32) - g.y.max(p.y);
            overlap_x >= 160 && overlap_y >= 80
        })
        .map(|m| m.scale_factor())
}

/// Note where the main window is. Called on every move and resize, so it only touches memory.
pub fn track(w: &Window) {
    if get_target().is_some() {
        return; // still being put back in place
    }
    // Windows parks a minimized window far off-screen; never remember that.
    if w.is_minimized().unwrap_or(false) {
        return;
    }
    if w.is_maximized().unwrap_or(false) {
        if let Some(g) = LAST.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            g.maximized = true; // keep the normal size and position from before it was maximized
        }
        return;
    }
    let (Ok(p), Ok(s)) = (w.outer_position(), w.inner_size()) else { return };
    if s.width < 200 || s.height < 200 || p.x <= -30000 || p.y <= -30000 {
        return;
    }
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(Geometry { x: p.x, y: p.y, w: s.width, h: s.height, maximized: false });
}

/// Write the remembered geometry to disk.
pub fn save_geometry(app: &AppHandle) {
    let Some(g) = *LAST.lock().unwrap_or_else(|e| e.into_inner()) else { return };
    let Some(path) = geometry_file(app) else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(json) = serde_json::to_vec(&g) else { return };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
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
    let saved = load_geometry(app).and_then(|g| scale_for(app, &g).map(|scale| (g, scale)));
    let builder = base(app, "main", "index.html", "DownloaDan").min_inner_size(860.0, 540.0).resizable(true);
    let builder = match saved {
        // The builder works in scaled units; the exact pixels are applied right after it is built.
        Some((g, scale)) => {
            *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(g);
            set_target(if g.maximized { None } else { Some(g) });
            builder.inner_size(g.w as f64 / scale, g.h as f64 / scale).position(g.x as f64 / scale, g.y as f64 / scale)
        }
        None => {
            set_target(None);
            builder.inner_size(1080.0, 700.0).center()
        }
    };
    match builder.build() {
        Ok(w) => {
            if let Some((g, _)) = saved {
                let _ = w.set_position(PhysicalPosition::new(g.x, g.y));
                let _ = w.set_size(PhysicalSize::new(g.w, g.h));
                if g.maximized {
                    let _ = w.maximize();
                }
            }
            // safety net: never leave the window hidden if the page fails to report in
            let w2 = w.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                reveal(&w2);
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
    if w.label() == "main" {
        tauri::async_runtime::spawn(settle(w.clone()));
    }
}

/// Once the window is on screen, check that it really sits where and as big as it was saved, and nudge it if not.
/// Windows reports slightly different frame sizes for a hidden borderless window than for a visible one, which
/// would otherwise make the window creep down and grow a little each time it is reopened.
async fn settle(w: WebviewWindow) {
    use std::sync::atomic::Ordering::SeqCst;
    if SETTLING.swap(true, SeqCst) {
        return;
    }
    if let Some(g) = get_target() {
        for _ in 0..4 {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let (Ok(p), Ok(s)) = (w.outer_position(), w.inner_size()) else { break };
            if p.x == g.x && p.y == g.y && s.width == g.w && s.height == g.h {
                break;
            }
            eprintln!("main window landed at {},{} {}x{}, wanted {},{} {}x{}: correcting", p.x, p.y, s.width, s.height, g.x, g.y, g.w, g.h);
            let _ = w.set_position(PhysicalPosition::new(g.x, g.y));
            // ask for whatever size lands on the saved one, allowing for the difference the first request picked up
            let width = (2 * g.w as i64 - s.width as i64).max(200) as u32;
            let height = (2 * g.h as i64 - s.height as i64).max(200) as u32;
            let _ = w.set_size(PhysicalSize::new(width, height));
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await; // let the last nudge land
    }
    set_target(None); // from here on the window is tracked normally
    SETTLING.store(false, SeqCst);
}
