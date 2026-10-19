//! The commands the windows can call. Every command checks which window is asking and validates its input: the
//! page is treated as untrusted, exactly as a web page would be.

use crate::state::{AppState, Caught, PromptItem, RefreshWait};
use crate::windows;
use downloadan_core::engine;
use downloadan_core::manager::{AddReq, OpResult};
use downloadan_core::theme;
use downloadan_core::util::{random_hex, same_name};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, State, WebviewWindow};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

type St<'a> = State<'a, Arc<AppState>>;
type Res<T> = Result<T, String>;

fn only(w: &WebviewWindow, label: &str) -> Res<()> {
    if w.label() == label {
        Ok(())
    } else {
        Err("Rejected".into())
    }
}

fn str_arr(v: Option<Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.into_iter().filter_map(|x| x.as_str().map(str::to_string)).take(500).collect(),
        _ => vec![],
    }
}

fn opt_str(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

fn base_name(u: &str) -> String {
    url::Url::parse(u)
        .ok()
        .and_then(|u| u.path_segments().and_then(|s| s.filter(|x| !x.is_empty()).last().map(str::to_string)))
        .map(|s| percent_encoding::percent_decode_str(&s).decode_utf8_lossy().into_owned())
        .unwrap_or_default()
}

fn path_to_string(p: tauri_plugin_dialog::FilePath) -> Option<String> {
    p.into_path().ok().map(|p| p.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------------------------------------
// Bridge and notifications (also used by main.rs)

pub async fn start_bridge(st: &Arc<AppState>) {
    let preferred = st.mgr.settings().bridge_port as u16;
    let mut ports: Vec<u16> = vec![preferred];
    ports.extend(crate::state::PORT_RANGE.filter(|p| *p != preferred)); // preferred port first, then the next free one in the range
    let status = st.bridge.listen_any(&ports).await;
    *st.bridge_status.lock() = status.clone();
    st.notifier.send("bridge:status", status);
}

pub fn notify(app: &AppHandle, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}

// ---------------------------------------------------------------------------------------------------------
// "New download" box

fn prompt_payload(st: &Arc<AppState>) -> Option<Value> {
    let (pid, url, waiting) = {
        let g = st.prompts.lock();
        let c = g.cur.as_ref()?;
        (c.pid.clone(), c.caught.url.clone(), g.waiting.len())
    };
    let snap = st.mgr.snapshot();
    Some(json!({
        "pid": pid, "url": url, "theme": st.mgr.active_theme(), "queues": snap.queues,
        "settings": { "downloadDir": snap.settings.download_dir }, "waiting": waiting
    }))
}

/// A link arrived from the browser: ask the user what to do with it (or hand it to a waiting "refresh").
pub fn prompt_add(app: &AppHandle, st: &Arc<AppState>, p: Caught) {
    if st.refresh.lock().is_some() {
        let (app, st) = (app.clone(), st.clone());
        tauri::async_runtime::spawn(async move { refresh_capture(&app, &st, p).await });
        return;
    }
    {
        let mut g = st.prompts.lock();
        let dup = |x: &Caught| x.url == p.url;
        if g.cur.as_ref().is_some_and(|c| dup(&c.caught)) || g.waiting.iter().any(|w| dup(&w.caught)) {
            drop(g);
            if let Some(w) = tauri::Manager::get_webview_window(app, "prompt") {
                let _ = w.set_focus();
            }
            return;
        }
        g.waiting.push_back(PromptItem { pid: random_hex(4), caught: p });
    }
    windows::open_prompt(app);
}

fn finish_prompt(app: &AppHandle, st: &Arc<AppState>) {
    let has_next = {
        let mut g = st.prompts.lock();
        g.cur = g.waiting.pop_front();
        g.cur.is_some()
    };
    if has_next {
        if let Some(payload) = prompt_payload(st) {
            use tauri::Emitter;
            let _ = app.emit_to("prompt", "prompt:next", payload);
            return;
        }
    }
    windows::close_window(app, "prompt");
}

/// The prompt window was closed (by any means): closing the box counts as Cancel.
pub fn prompt_closed(app: &AppHandle, st: &Arc<AppState>) {
    let more = {
        let mut g = st.prompts.lock();
        g.cur = None;
        !g.waiting.is_empty()
    };
    if more && !st.quitting.load(Ordering::SeqCst) {
        windows::open_prompt(app);
    }
}

#[tauri::command]
pub async fn prompt_init(window: WebviewWindow, state: St<'_>) -> Res<Option<Value>> {
    only(&window, "prompt")?;
    let st = state.inner().clone();
    {
        let mut g = st.prompts.lock();
        if g.cur.is_none() {
            g.cur = g.waiting.pop_front();
        }
    }
    let payload = prompt_payload(&st);
    windows::reveal(&window);
    Ok(payload)
}

#[tauri::command]
pub async fn prompt_probe(window: WebviewWindow, state: St<'_>, pid: String) -> Res<Value> {
    only(&window, "prompt")?;
    let st = state.inner().clone();
    let c = {
        let g = st.prompts.lock();
        g.cur.as_ref().filter(|c| c.pid == pid).map(|c| c.caught.clone())
    };
    let Some(c) = c else { return Ok(json!({ "ok": false, "error": "Request changed" })) };
    Ok(serde_json::to_value(engine::inspect(&st.mgr.client(), &c.url, c.referer, c.cookie).await).unwrap_or(Value::Null))
}

#[tauri::command]
pub async fn prompt_free(window: WebviewWindow, dir: Option<String>) -> Res<Option<u64>> {
    only(&window, "prompt")?;
    let dir = dir.unwrap_or_default();
    Ok(downloadan_core::util::free_space(Path::new(&dir)))
}

#[tauri::command]
pub async fn prompt_dir(window: WebviewWindow, app: AppHandle, start: Option<String>) -> Res<Option<String>> {
    only(&window, "prompt")?;
    let mut d = app.dialog().file().set_parent(&window);
    if let Some(s) = start.filter(|s| Path::new(s).is_absolute()) {
        d = d.set_directory(s);
    }
    Ok(d.blocking_pick_folder().and_then(path_to_string))
}

#[tauri::command]
pub async fn prompt_confirm(window: WebviewWindow, app: AppHandle, state: St<'_>, p: Option<Value>) -> Res<Value> {
    only(&window, "prompt")?;
    let st = state.inner().clone();
    let p = p.unwrap_or(Value::Null);
    let cur = {
        let g = st.prompts.lock();
        g.cur.as_ref().map(|c| (c.pid.clone(), c.caught.clone()))
    };
    let Some((pid, c)) = cur else { return Ok(json!({ "ok": false })) };
    if opt_str(&p, "pid").as_deref() != Some(pid.as_str()) {
        return Ok(json!({ "ok": false }));
    }
    let start = p.get("startQueue").and_then(Value::as_bool).unwrap_or(false);
    let r = st.mgr.add(AddReq {
        urls: vec![c.url],
        queue_id: opt_str(&p, "queueId"),
        dir: opt_str(&p, "dir"),
        filename: opt_str(&p, "filename"),
        referer: c.referer,
        cookie: c.cookie,
        start: false,
        hold: !start,
    });
    let Some(first) = r.added.first() else { return Ok(json!({ "ok": false })) };
    if start {
        if let Some(item) = st.mgr.item(first) {
            st.mgr.start_queue(&item.queue_id);
        }
    }
    finish_prompt(&app, &st);
    Ok(json!({ "ok": true }))
}

#[tauri::command]
pub async fn prompt_cancel(window: WebviewWindow, app: AppHandle, state: St<'_>, pid: String) -> Res<()> {
    only(&window, "prompt")?;
    let st = state.inner().clone();
    let is_cur = st.prompts.lock().cur.as_ref().is_some_and(|c| c.pid == pid);
    if is_cur {
        finish_prompt(&app, &st);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------
// "Connect browser" box

#[tauri::command]
pub async fn pair_init(window: WebviewWindow, state: St<'_>) -> Res<Option<Value>> {
    only(&window, "pair")?;
    let st = state.inner().clone();
    let out = st.api.pending_pairing().map(|p| {
        let left = (60 - ((downloadan_core::util::now_ms() - p.created_at) / 1000)).max(0);
        json!({ "theme": st.mgr.active_theme(), "browser": p.browser, "extensionId": p.extension_id, "verified": p.verified, "secondsLeft": left })
    });
    windows::reveal(&window);
    Ok(out)
}

#[tauri::command]
pub async fn pair_answer(window: WebviewWindow, app: AppHandle, state: St<'_>, allow: bool) -> Res<Value> {
    only(&window, "pair")?;
    let st = state.inner().clone();
    if let Some(p) = st.api.pending_pairing() {
        st.api.resolve_pairing(&p.id, allow);
    }
    windows::close_window(&app, "pair");
    Ok(json!({ "ok": true }))
}

// ---------------------------------------------------------------------------------------------------------
// "Refresh download address"

fn refresh_send(st: &Arc<AppState>, payload: Value) {
    st.notifier.send("refresh:event", payload);
}

fn refresh_clear(st: &Arc<AppState>) {
    if let Some(w) = st.refresh.lock().take() {
        if let Some(t) = w.timeout {
            t.abort();
        }
    }
}

async fn refresh_capture(app: &AppHandle, st: &Arc<AppState>, p: Caught) {
    let (id, token, my) = {
        let mut g = st.refresh.lock();
        let Some(w) = g.as_mut() else { return };
        w.seq += 1;
        w.pending = None;
        (w.id.clone(), w.token, w.seq)
    };
    let item = st.mgr.item(&id);
    let usable = item.as_ref().is_some_and(|d| !matches!(d.status, downloadan_core::model::Status::Completed | downloadan_core::model::Status::Downloading));
    if !usable {
        refresh_clear(st);
        refresh_send(st, json!({ "type": "error", "message": "That download is no longer waiting for a new link." }));
        return;
    }
    let item = item.unwrap();
    refresh_send(st, json!({ "type": "checking" }));
    let info = engine::inspect(&st.mgr.client(), &p.url, p.referer.clone().or(item.referer.clone()), p.cookie.clone().or(item.cookie.clone())).await;
    let still_mine = st.refresh.lock().as_ref().is_some_and(|w| w.token == token && w.seq == my);
    if !still_mine {
        return; // cancelled, or a newer link arrived meanwhile
    }
    if !info.ok {
        refresh_send(st, json!({ "type": "error", "message": format!("That link did not work ({}). Still waiting for another one…", info.error) }));
        return;
    }
    let verdict = st.mgr.compare_for_refresh(&id, &info);
    if verdict.ok {
        refresh_finish(app, st, &id, p, &info, false);
        return;
    }
    if !st.notifier.is_ready() {
        refresh_clear(st); // nobody to ask: fall back to the normal New download box
        prompt_add(app, st, p);
        return;
    }
    refresh_send(
        st,
        json!({
            "type": "confirm", "reasons": verdict.reasons, "discards": item.downloaded,
            "expected": { "name": item.filename.clone().unwrap_or_else(|| base_name(&item.url)), "size": item.size },
            "got": { "name": info.filename, "size": info.size, "resumable": info.resumable }
        }),
    );
    if let Some(w) = st.refresh.lock().as_mut() {
        w.pending = Some((p, info));
    }
}

fn refresh_finish(app: &AppHandle, st: &Arc<AppState>, id: &str, p: Caught, info: &engine::Inspect, restart: bool) {
    refresh_clear(st);
    let Some(d) = st.mgr.item(id) else { return };
    let keep_name = d.filename.as_deref().map_or(true, |f| same_name(f, &info.filename));
    st.mgr.refresh_address(id, p.url.clone(), p.referer, p.cookie, restart, restart && !keep_name);
    refresh_send(st, json!({ "type": "done", "restarted": restart }));
    if !st.notifier.is_ready() {
        notify(app, "Download link refreshed", &d.filename.unwrap_or_else(|| base_name(&p.url)));
    }
}

#[tauri::command]
pub async fn refresh_start(window: WebviewWindow, state: St<'_>, id: String) -> Res<Value> {
    only(&window, "main")?;
    let st = state.inner().clone();
    let Some(d) = st.mgr.item(&id) else { return Ok(json!({ "ok": false, "error": "This download cannot be refreshed right now." })) };
    if !matches!(d.status, downloadan_core::model::Status::Error | downloadan_core::model::Status::Paused | downloadan_core::model::Status::Queued) {
        return Ok(json!({ "ok": false, "error": "This download cannot be refreshed right now." }));
    }
    refresh_clear(&st);
    let token = st.refresh_counter.fetch_add(1, Ordering::SeqCst) + 1;
    let timeout = {
        let st = st.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(10 * 60)).await;
            let mine = st.refresh.lock().as_ref().is_some_and(|w| w.token == token);
            if mine {
                st.refresh.lock().take();
                refresh_send(&st, json!({ "type": "timeout" }));
            }
        })
    };
    *st.refresh.lock() = Some(RefreshWait { id: d.id.clone(), token, seq: 0, pending: None, timeout: Some(timeout) });
    Ok(json!({ "ok": true, "name": d.filename.clone().unwrap_or_else(|| base_name(&d.url)), "size": d.size.max(0), "downloaded": d.downloaded.max(0) }))
}

#[tauri::command]
pub async fn refresh_cancel(window: WebviewWindow, state: St<'_>) -> Res<()> {
    only(&window, "main")?;
    refresh_clear(state.inner());
    Ok(())
}

#[tauri::command]
pub async fn refresh_url(window: WebviewWindow, app: AppHandle, state: St<'_>, u: Option<String>) -> Res<Value> {
    only(&window, "main")?;
    let st = state.inner().clone();
    if st.refresh.lock().is_none() {
        return Ok(json!({ "ok": false, "error": "Not waiting for a link any more." }));
    }
    let Ok(url) = url::Url::parse(u.unwrap_or_default().trim()) else { return Ok(json!({ "ok": false, "error": "That is not a valid link." })) };
    if url.scheme() != "http" && url.scheme() != "https" {
        return Ok(json!({ "ok": false, "error": "Only http:// and https:// links are supported." }));
    }
    let caught = Caught { url: url.to_string(), referer: None, cookie: None };
    tauri::async_runtime::spawn(async move { refresh_capture(&app, &st, caught).await });
    Ok(json!({ "ok": true }))
}

#[tauri::command]
pub async fn refresh_decide(window: WebviewWindow, app: AppHandle, state: St<'_>, choice: Option<String>) -> Res<Value> {
    only(&window, "main")?;
    let st = state.inner().clone();
    let taken = {
        let mut g = st.refresh.lock();
        match g.as_mut() {
            Some(w) if w.pending.is_some() => Some((w.id.clone(), w.pending.take().unwrap())),
            _ => None,
        }
    };
    let Some((id, (p, info))) = taken else { return Ok(json!({ "ok": false })) };
    match choice.as_deref() {
        Some("replace") => refresh_finish(&app, &st, &id, p, &info, true),
        Some("new") => {
            refresh_clear(&st);
            prompt_add(&app, &st, p);
        }
        _ => refresh_send(&st, json!({ "type": "waiting" })), // keep waiting
    }
    Ok(json!({ "ok": true }))
}

// ---------------------------------------------------------------------------------------------------------
// Main window

#[tauri::command]
pub async fn state_get(window: WebviewWindow, state: St<'_>) -> Res<Value> {
    only(&window, "main")?;
    let st = state.inner().clone();
    st.mgr.prune_missing().await;
    st.notifier.ready.store(true, Ordering::SeqCst);
    let mut v = serde_json::to_value(st.mgr.snapshot()).map_err(|e| e.to_string())?;
    if let Some(o) = v.as_object_mut() {
        o.insert("builtinThemes".into(), serde_json::to_value(theme::builtin_themes()).unwrap_or_default());
        o.insert("bridge".into(), serde_json::to_value(st.bridge_status.lock().clone()).unwrap_or_default());
        o.insert("pending".into(), json!([]));
    }
    windows::reveal(&window); // the page has its data: show the window
    Ok(v)
}

#[tauri::command]
pub async fn clip_read(window: WebviewWindow, app: AppHandle) -> Res<String> {
    only(&window, "main")?;
    Ok(app.clipboard().read_text().map(|s| s.chars().take(4096).collect()).unwrap_or_default())
}

#[tauri::command]
pub async fn clip_write(window: WebviewWindow, app: AppHandle, text: Option<String>) -> Res<()> {
    only(&window, "main")?;
    if let Some(t) = text {
        let _ = app.clipboard().write_text(t.chars().take(8192).collect::<String>());
    }
    Ok(())
}

#[tauri::command]
pub async fn dl_add(window: WebviewWindow, state: St<'_>, p: Option<Value>) -> Res<Value> {
    only(&window, "main")?;
    let p = p.unwrap_or(Value::Null);
    let urls = str_arr(p.get("urls").cloned());
    let r = state.mgr.add(AddReq {
        urls,
        queue_id: opt_str(&p, "queueId"),
        dir: opt_str(&p, "dir"),
        filename: opt_str(&p, "filename"),
        referer: opt_str(&p, "referer"),
        cookie: opt_str(&p, "cookie"),
        start: p.get("start").and_then(Value::as_bool).unwrap_or(false),
        hold: p.get("hold").and_then(Value::as_bool).unwrap_or(false),
    });
    serde_json::to_value(r).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn dl_pause(window: WebviewWindow, state: St<'_>, ids: Option<Value>) -> Res<()> {
    only(&window, "main")?;
    state.mgr.pause(&str_arr(ids));
    Ok(())
}

#[tauri::command]
pub async fn dl_pause_all(window: WebviewWindow, state: St<'_>) -> Res<()> {
    only(&window, "main")?;
    state.mgr.pause_all();
    Ok(())
}

#[tauri::command]
pub async fn dl_resume(window: WebviewWindow, state: St<'_>, ids: Option<Value>) -> Res<()> {
    only(&window, "main")?;
    state.mgr.resume(&str_arr(ids));
    Ok(())
}

#[tauri::command]
pub async fn dl_remove(window: WebviewWindow, state: St<'_>, ids: Option<Value>, del: Option<bool>) -> Res<Value> {
    only(&window, "main")?;
    let r = state.mgr.remove(&str_arr(ids), del.unwrap_or(false)).await;
    serde_json::to_value(r).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn dl_clear_completed(window: WebviewWindow, state: St<'_>) -> Res<()> {
    only(&window, "main")?;
    state.mgr.clear_completed();
    Ok(())
}

#[tauri::command]
pub async fn dl_move(window: WebviewWindow, state: St<'_>, ids: Option<Value>, q: Option<String>) -> Res<()> {
    only(&window, "main")?;
    state.mgr.move_to_queue(&str_arr(ids), &q.unwrap_or_default());
    Ok(())
}

/// Returns "" when the file was opened, otherwise a short message.
#[tauri::command]
pub async fn dl_open(window: WebviewWindow, app: AppHandle, state: St<'_>, id: String) -> Res<String> {
    only(&window, "main")?;
    match state.mgr.file_path(&id) {
        Some(f) if f.exists() => Ok(match app.opener().open_path(f.to_string_lossy(), None::<&str>) {
            Ok(()) => String::new(),
            Err(e) => e.to_string(),
        }),
        _ => Ok("File not found".into()),
    }
}

#[tauri::command]
pub async fn dl_show(window: WebviewWindow, app: AppHandle, state: St<'_>, id: String) -> Res<()> {
    only(&window, "main")?;
    if let Some(f) = state.mgr.locate_path(&id) {
        let _ = app.opener().reveal_item_in_dir(f);
    }
    Ok(())
}

#[tauri::command]
pub async fn q_create(window: WebviewWindow, state: St<'_>, name: Option<String>) -> Res<OpResult> {
    only(&window, "main")?;
    Ok(state.mgr.create_queue(&name.unwrap_or_default()))
}

#[tauri::command]
pub async fn q_rename(window: WebviewWindow, state: St<'_>, id: String, name: Option<String>) -> Res<OpResult> {
    only(&window, "main")?;
    Ok(state.mgr.rename_queue(&id, &name.unwrap_or_default()))
}

#[tauri::command]
pub async fn q_delete(window: WebviewWindow, state: St<'_>, id: String) -> Res<OpResult> {
    only(&window, "main")?;
    Ok(state.mgr.delete_queue(&id))
}

#[tauri::command]
pub async fn q_enable(window: WebviewWindow, state: St<'_>, id: String, on: Option<bool>) -> Res<()> {
    only(&window, "main")?;
    state.mgr.set_queue_enabled(&id, on.unwrap_or(false));
    Ok(())
}

#[tauri::command]
pub async fn q_start(window: WebviewWindow, state: St<'_>, id: String) -> Res<()> {
    only(&window, "main")?;
    state.mgr.start_queue(&id);
    Ok(())
}

#[tauri::command]
pub async fn settings_set(window: WebviewWindow, app: AppHandle, state: St<'_>, p: Option<Value>) -> Res<()> {
    only(&window, "main")?;
    let st = state.inner().clone();
    let p = p.unwrap_or(Value::Null);
    let before = st.mgr.settings().bridge_port;
    st.mgr.set_settings(&p);
    if p.get("removeMissing").and_then(Value::as_bool) == Some(true) {
        let mgr = st.mgr.clone();
        tauri::async_runtime::spawn(async move {
            mgr.prune_missing().await;
        });
    }
    let s = st.mgr.settings();
    if !cfg!(debug_assertions) {
        use tauri_plugin_autostart::ManagerExt;
        let launch = app.autolaunch();
        let _ = if s.run_at_startup { launch.enable() } else { launch.disable() };
    }
    if s.bridge_port != before {
        start_bridge(&st).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn dir_choose(window: WebviewWindow, app: AppHandle, state: St<'_>) -> Res<Option<String>> {
    only(&window, "main")?;
    let current = state.mgr.settings().download_dir;
    Ok(app.dialog().file().set_parent(&window).set_directory(current).blocking_pick_folder().and_then(path_to_string))
}

#[tauri::command]
pub async fn token_regen(window: WebviewWindow, state: St<'_>) -> Res<String> {
    only(&window, "main")?;
    Ok(state.mgr.regenerate_token())
}

// ---- updates ---------------------------------------------------------------------------------------------

#[tauri::command]
pub async fn update_get(window: WebviewWindow, state: St<'_>) -> Res<Value> {
    only(&window, "main")?;
    serde_json::to_value(state.updater.state()).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_check(window: WebviewWindow, app: AppHandle, state: St<'_>) -> Res<Value> {
    only(&window, "main")?;
    let st = state.inner().clone();
    st.manual_check.store(true, Ordering::SeqCst);
    st.updater.check(&app).await;
    st.manual_check.store(false, Ordering::SeqCst);
    serde_json::to_value(st.updater.state()).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_download(window: WebviewWindow, app: AppHandle, state: St<'_>) -> Res<()> {
    only(&window, "main")?;
    state.updater.download(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn update_install(window: WebviewWindow, app: AppHandle, state: St<'_>) -> Res<bool> {
    only(&window, "main")?;
    Ok(state.updater.install(&app))
}

// ---- themes ----------------------------------------------------------------------------------------------

#[tauri::command]
pub async fn theme_validate(window: WebviewWindow, t: Option<Value>) -> Res<Value> {
    only(&window, "main")?;
    Ok(match theme::validate_theme(&t.unwrap_or(Value::Null)) {
        Ok(_) => json!({ "ok": true }),
        Err(e) => json!({ "ok": false, "error": e }),
    })
}

#[tauri::command]
pub async fn theme_save(window: WebviewWindow, state: St<'_>, t: Option<Value>) -> Res<OpResult> {
    only(&window, "main")?;
    Ok(state.mgr.theme_save(&t.unwrap_or(Value::Null)))
}

#[tauri::command]
pub async fn theme_apply(window: WebviewWindow, state: St<'_>, tid: String) -> Res<()> {
    only(&window, "main")?;
    state.mgr.theme_apply(&tid);
    Ok(())
}

#[tauri::command]
pub async fn theme_delete(window: WebviewWindow, state: St<'_>, tid: String) -> Res<()> {
    only(&window, "main")?;
    state.mgr.theme_delete(&tid);
    Ok(())
}

#[tauri::command]
pub async fn theme_export(window: WebviewWindow, app: AppHandle, state: St<'_>, tid: String) -> Res<Value> {
    only(&window, "main")?;
    let Some((file_name, text)) = state.mgr.theme_export(&tid) else { return Ok(json!({ "ok": false, "error": "Theme not found." })) };
    let picked = app.dialog().file().set_parent(&window).add_filter("DownloaDan theme", &["dantheme"]).set_file_name(file_name).blocking_save_file();
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else { return Ok(json!({ "ok": false, "cancelled": true })) };
    Ok(match std::fs::write(&path, text) {
        Ok(()) => json!({ "ok": true }),
        Err(_) => json!({ "ok": false, "error": "Could not write that file." }),
    })
}

#[tauri::command]
pub async fn theme_import(window: WebviewWindow, app: AppHandle, state: St<'_>) -> Res<Value> {
    only(&window, "main")?;
    let picked = app.dialog().file().set_parent(&window).add_filter("DownloaDan theme", &["dantheme", "fluxtheme"]).blocking_pick_file();
    let Some(file) = picked.and_then(|p| p.into_path().ok()).map(PathBuf::from) else { return Ok(json!({ "ok": false, "cancelled": true })) };
    match std::fs::metadata(&file) {
        Ok(m) if m.len() > theme::MAX_BYTES as u64 => return Ok(json!({ "ok": false, "error": "Theme file is too large (max 8 KB)." })), // size check BEFORE reading
        Err(_) => return Ok(json!({ "ok": false, "error": "Could not read that file." })),
        _ => {}
    }
    let Ok(bytes) = std::fs::read(&file) else { return Ok(json!({ "ok": false, "error": "Could not read that file." })) };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    serde_json::to_value(state.mgr.theme_import_text(&text)).map_err(|e| e.to_string())
}
