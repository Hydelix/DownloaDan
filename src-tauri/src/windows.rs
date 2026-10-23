//! The app's windows: the main window, and the two small always-on-top boxes ("New download" and
//! "Connect browser") that exist only while they are needed.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::window::Color;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Window};

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

// ---- remembering the size of the main window ----
// Only the size is remembered (in `window.json` next to state.json). A window opened from the tray always appears in the
// middle of the screen, so it can never end up off-screen; a minimized window is never closed, so Windows simply puts
// it back where it was. The size is tracked in memory while the window is open and written to disk when it closes,
// loses focus, or the app quits.

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Geometry {
    w: u32,
    h: u32,
    #[serde(default)]
    maximized: bool,
    /// How much bigger than the saved size the window has to be asked to be to come out at it (borderless windows on
    /// Windows end up a few pixels off). Learned the first time the window is opened, so later opens are exact.
    #[serde(default)]
    bw: i32,
    #[serde(default)]
    bh: i32,
}

/// The last size of the main window. For a maximized window this is its normal (restored) size.
static LAST: Mutex<Option<Geometry>> = Mutex::new(None);

/// The size a freshly opened window was asked for, while it is still settling in. Nothing is remembered until then.
static OPENING: Mutex<Option<Geometry>> = Mutex::new(None);
static SETTLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn get_opening() -> Option<Geometry> {
    *OPENING.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_opening(g: Option<Geometry>) {
    *OPENING.lock().unwrap_or_else(|e| e.into_inner()) = g;
}

fn geometry_file(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("window.json"))
}

fn load_geometry(app: &AppHandle) -> Option<Geometry> {
    let bytes = std::fs::read(geometry_file(app)?).ok()?;
    let g: Geometry = serde_json::from_slice(&bytes).ok()?;
    (g.w >= 200 && g.h >= 200).then_some(g)
}

/// The saved size (plus the learned correction) kept inside the primary monitor, in the logical units the builder takes.
fn builder_size(app: &AppHandle, g: &Geometry) -> (f64, f64) {
    let (mut scale, mut max_w, mut max_h) = (1.0, u32::MAX, u32::MAX);
    if let Ok(Some(m)) = app.primary_monitor() {
        scale = m.scale_factor();
        max_w = m.size().width;
        max_h = m.size().height;
    }
    let w = ((g.w as i64 + g.bw as i64).max(1) as u32).min(max_w);
    let h = ((g.h as i64 + g.bh as i64).max(1) as u32).min(max_h);
    (w as f64 / scale, h as f64 / scale)
}

/// Note the size of the main window. Called on every resize, so it only touches memory.
pub fn track(w: &Window) {
    if get_opening().is_some() {
        return; // still opening: the resizing going on is the window settling in, not the user
    }
    if w.is_minimized().unwrap_or(false) {
        return;
    }
    if w.is_maximized().unwrap_or(false) {
        if let Some(g) = LAST.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            g.maximized = true; // keep the normal size from before it was maximized
        }
        return;
    }
    let Ok(s) = w.inner_size() else { return };
    if s.width < 200 || s.height < 200 {
        return;
    }
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let (bw, bh) = (*last).map(|g| (g.bw, g.bh)).unwrap_or((0, 0));
    *last = Some(Geometry { w: s.width, h: s.height, maximized: false, bw, bh });
}

/// Write the remembered size to disk.
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
    let saved = load_geometry(app);
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = saved;
    set_opening(saved.filter(|g| !g.maximized));
    let builder = base(app, "main", "index.html", "DownloaDan").min_inner_size(860.0, 540.0).resizable(true).center();
    let builder = match saved {
        Some(g) => {
            let (w, h) = builder_size(app, &g);
            builder.inner_size(w, h).maximized(g.maximized)
        }
        None => builder.inner_size(1080.0, 700.0),
    };
    match builder.build() {
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
    if w.label() == "main" {
        tauri::async_runtime::spawn(settle(w.clone()));
    }
}

/// A moment after the window appears, see what size it really came out at. Windows can hand a borderless window a few
/// pixels more or less than asked for; the difference is remembered (`bw`/`bh`) and allowed for the next time, so the
/// window reopens at exactly the size it was closed at. Nothing is moved or resized here, so nothing is seen to change.
async fn settle(w: WebviewWindow) {
    use std::sync::atomic::Ordering::SeqCst;
    if SETTLING.swap(true, SeqCst) {
        return;
    }
    if let Some(g) = get_opening() {
        tokio::time::sleep(std::time::Duration::from_millis(450)).await;
        if let Ok(s) = w.inner_size() {
            let (dw, dh) = (s.width as i64 - g.w as i64, s.height as i64 - g.h as i64);
            if (dw != 0 || dh != 0) && dw.abs() <= 80 && dh.abs() <= 80 {
                if let Some(last) = LAST.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    last.bw = g.bw - dw as i32;
                    last.bh = g.bh - dh as i32;
                }
                log_line(w.app_handle(), &format!("opened {}x{}, wanted {}x{} (was asking for {:+},{:+}): now allowing {:+},{:+}", s.width, s.height, g.w, g.h, g.bw, g.bh, g.bw - dw as i32, g.bh - dh as i32));
            }
        }
    }
    set_opening(None); // from here on, resizing is the user's
    SETTLING.store(false, SeqCst);
}

/// A short note in `window.log` (next to window.json) whenever the opening size had to be adjusted. Kept small.
fn log_line(app: &AppHandle, line: &str) {
    use std::io::Write;
    let Some(path) = geometry_file(app).map(|p| p.with_file_name("window.log")) else { return };
    if std::fs::metadata(&path).map(|m| m.len() > 20_000).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{secs} {line}");
    }
}
