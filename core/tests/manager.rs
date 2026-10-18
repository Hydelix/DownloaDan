mod common;
use common::{pattern, start};
use downloadan_core::manager::{AddReq, Event, Manager};
use downloadan_core::model::Status;
use parking_lot::Mutex;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Env {
    mgr: Arc<Manager>,
    dir: tempfile::TempDir,
    events: Arc<Mutex<Vec<Event>>>,
}

fn env_at(dir: tempfile::TempDir) -> Env {
    let events = Arc::new(Mutex::new(vec![]));
    let e = events.clone();
    let mgr = Manager::new(
        dir.path().join("state.json"),
        dir.path().join("downloads").to_string_lossy().into_owned(),
        tokio::runtime::Handle::current(),
        Arc::new(move |ev| e.lock().push(ev)),
    );
    Env { mgr, dir, events }
}

fn env() -> Env {
    env_at(tempfile::tempdir().unwrap())
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(20), "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn status_of(m: &Manager, id: &str) -> Status {
    m.summary_of(id).unwrap().status
}

fn req(urls: Vec<String>) -> AddReq {
    AddReq { urls, ..Default::default() }
}

#[tokio::test(flavor = "multi_thread")]
async fn adds_downloads_and_completes_them() {
    let srv = start().await;
    let e = env();
    let r = e.mgr.add(AddReq { start: true, ..req(vec![srv.url("/ranged/2500000"), "ftp://nope/x".into(), "not a url".into()]) });
    assert_eq!(r.added.len(), 1);
    assert_eq!(r.rejected.len(), 2);
    let id = r.added[0].clone();
    until("completion", || status_of(&e.mgr, &id) == Status::Completed).await;
    let s = e.mgr.summary_of(&id).unwrap();
    let file = e.dir.path().join("downloads").join(s.filename.clone().unwrap());
    assert_eq!(std::fs::read(&file).unwrap(), pattern(2_500_000));
    assert!(e.events.lock().iter().any(|ev| matches!(ev, Event::Completed(c) if c.id == id)));
    assert_eq!(e.mgr.file_path(&id).unwrap(), file);
}

#[tokio::test(flavor = "multi_thread")]
async fn only_runs_as_many_downloads_as_allowed_at_once() {
    let srv = start().await;
    let e = env();
    e.mgr.set_settings(&json!({ "maxSimultaneous": 2, "connections": 2 }));
    let urls: Vec<String> = (0..4).map(|i| srv.url(&format!("/slow/{}", 3_000_000 + i))).collect();
    let r = e.mgr.add(req(urls));
    let ids = r.added.clone();
    let mut peak = 0;
    until("all complete", || {
        let n = ids.iter().filter(|i| status_of(&e.mgr, i) == Status::Downloading).count();
        peak = peak.max(n);
        ids.iter().all(|i| status_of(&e.mgr, i) == Status::Completed)
    })
    .await;
    assert_eq!(peak, 2, "exactly two at a time");
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_and_resume_a_running_download() {
    let srv = start().await;
    let e = env();
    let id = e.mgr.add(AddReq { start: true, ..req(vec![srv.url("/slow/20000000")]) }).added[0].clone();
    until("it is downloading with progress", || e.mgr.summary_of(&id).is_some_and(|s| s.status == Status::Downloading && s.downloaded > 0)).await;
    e.mgr.pause(&[id.clone()]);
    until("paused", || status_of(&e.mgr, &id) == Status::Paused).await;
    let at_pause = e.mgr.summary_of(&id).unwrap().downloaded;
    assert!(at_pause > 0 && at_pause < 20_000_000);
    e.mgr.resume(&[id.clone()]);
    until("completed after resume", || status_of(&e.mgr, &id) == Status::Completed).await;
    let s = e.mgr.summary_of(&id).unwrap();
    assert_eq!(std::fs::read(e.dir.path().join("downloads").join(s.filename.unwrap())).unwrap(), pattern(20_000_000));
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_can_be_retried_and_held_downloads_wait_for_their_queue() {
    let srv = start().await;
    let e = env();
    let bad = e.mgr.add(AddReq { start: true, ..req(vec![srv.url("/missing")]) }).added[0].clone();
    until("error", || status_of(&e.mgr, &bad) == Status::Error).await;
    assert_eq!(e.mgr.summary_of(&bad).unwrap().error.as_deref(), Some("Server replied HTTP 404"));

    let q = e.mgr.create_queue("Night");
    let qid = q.id.unwrap();
    e.mgr.set_queue_enabled(&qid, false);
    let held = e.mgr.add(AddReq { queue_id: Some(qid.clone()), hold: true, ..req(vec![srv.url("/ranged/1200000")]) }).added[0].clone();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(status_of(&e.mgr, &held), Status::Queued, "a held download must not start by itself");
    e.mgr.start_queue(&qid);
    until("held download runs once its queue starts", || status_of(&e.mgr, &held) == Status::Completed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_rules() {
    let e = env();
    assert!(!e.mgr.create_queue("   ").ok);
    assert!(!e.mgr.create_queue("main").ok || e.mgr.snapshot().queues.len() == 2); // "Main" is taken, case-insensitively
    let snapshot = e.mgr.snapshot();
    assert_eq!(snapshot.queues.iter().filter(|q| q.name.eq_ignore_ascii_case("main")).count(), 1);
    let a = e.mgr.create_queue("Work").id.unwrap();
    assert!(!e.mgr.create_queue("work").ok, "names are unique, ignoring case");
    assert!(!e.mgr.rename_queue("main", "Other").ok, "the default queue keeps its name");
    assert!(!e.mgr.delete_queue("main").ok);
    let dl = e.mgr.add(AddReq { queue_id: Some(a.clone()), hold: true, ..req(vec!["http://127.0.0.1:9/never".into()]) }).added[0].clone();
    assert!(e.mgr.delete_queue(&a).ok);
    assert_eq!(e.mgr.snapshot().downloads.iter().find(|d| d.id == dl).unwrap().queue_id, "main", "downloads of a deleted queue move to Main");
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_cancels_cleans_up_and_deletes_files_only_when_asked() {
    let srv = start().await;
    let e = env();
    let trashed: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(vec![]));
    let t = trashed.clone();
    e.mgr.set_trash(Arc::new(move |p: &Path| {
        t.lock().push(p.to_path_buf());
        std::fs::remove_file(p).map_err(|e| e.to_string())
    }));

    // a running download: its partial file disappears with it
    let running = e.mgr.add(AddReq { start: true, ..req(vec![srv.url("/slow/30000000")]) }).added[0].clone();
    until("running", || e.mgr.summary_of(&running).is_some_and(|s| s.downloaded > 0)).await;
    let part = e.mgr.locate_path(&running).unwrap();
    assert!(part.exists());
    let r = e.mgr.remove(&[running.clone()], true).await;
    assert_eq!((r.removed, r.deleted), (1, 0));
    assert!(!part.exists(), "unfinished data is deleted");
    assert!(e.mgr.summary_of(&running).is_none());

    // a finished one: kept unless asked
    let a = e.mgr.add(AddReq { start: true, ..req(vec![srv.url("/ranged/1100000")]) }).added[0].clone();
    let b = e.mgr.add(AddReq { start: true, ..req(vec![srv.url("/ranged/1200000")]) }).added[0].clone();
    until("both done", || status_of(&e.mgr, &a) == Status::Completed && status_of(&e.mgr, &b) == Status::Completed).await;
    let (fa, fb) = (e.mgr.file_path(&a).unwrap(), e.mgr.file_path(&b).unwrap());
    e.mgr.remove(&[a.clone()], false).await;
    assert!(fa.exists(), "the file stays when only the list entry is removed");
    let r = e.mgr.remove(&[b.clone()], true).await;
    assert_eq!(r.deleted, 1);
    assert!(!fb.exists());
    assert_eq!(trashed.lock().as_slice(), [fb], "deleted files go through the trash function");
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_are_validated() {
    let e = env();
    e.mgr.set_settings(&json!({
        "maxSimultaneous": 99, "connections": -4, "bridgePort": 80, "bypassKey": "Escape", "downloadDir": "relative/path",
        "notify": "yes", "minimizeToTray": false
    }));
    let s = e.mgr.settings();
    assert_eq!(s.max_simultaneous, 10);
    assert_eq!(s.connections, 1);
    assert_eq!(s.bridge_port, 1024);
    assert_eq!(s.bypass_key, "F8", "Escape is reserved");
    assert_ne!(s.download_dir, "relative/path");
    assert!(s.notify, "a string is not a boolean");
    assert!(!s.minimize_to_tray);
    e.mgr.set_settings(&json!({ "bypassKey": "KeyB" }));
    assert_eq!(e.mgr.settings().bypass_key, "KeyB");
    e.mgr.set_settings(&json!({ "bypassKey": "" }));
    assert_eq!(e.mgr.settings().bypass_key, "");
    e.mgr.set_settings(&json!({ "bypassKey": "a b" }));
    assert_eq!(e.mgr.settings().bypass_key, "");
    let old = e.mgr.token();
    assert_ne!(e.mgr.regenerate_token(), old);
}

#[tokio::test(flavor = "multi_thread")]
async fn state_survives_a_restart_and_running_downloads_come_back_paused() {
    let srv = start().await;
    let e = env();
    let done = e.mgr.add(AddReq { start: true, ..req(vec![srv.url("/ranged/1200000")]) }).added[0].clone();
    until("done", || status_of(&e.mgr, &done) == Status::Completed).await;
    let running = e.mgr.add(AddReq { start: true, ..req(vec![srv.url("/slow/30000000")]) }).added[0].clone();
    until("running", || e.mgr.summary_of(&running).is_some_and(|s| s.downloaded > 0)).await;
    let token = e.mgr.token();
    // Simulate a crash: keep the state file exactly as it was while the download was running, then stop the first
    // manager (in real life that process would simply be gone).
    e.mgr.flush();
    let dir = e.dir.path().to_path_buf();
    let crashed = std::fs::read(dir.join("state.json")).unwrap();
    e.mgr.shutdown().await;
    std::fs::write(dir.join("state.json"), crashed).unwrap();

    let second = Manager::new(dir.join("state.json"), dir.join("downloads").to_string_lossy().into_owned(), tokio::runtime::Handle::current(), Arc::new(|_| {}));
    assert_eq!(second.token(), token, "the browser pairing survives");
    assert_eq!(second.summary_of(&done).unwrap().status, Status::Completed);
    let r = second.summary_of(&running).unwrap();
    assert_eq!(r.status, Status::Paused);
    assert!(r.downloaded > 0);
    second.resume(&[running.clone()]);
    until("the interrupted download finishes", || second.summary_of(&running).unwrap().status == Status::Completed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_state_file_is_kept_aside_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("state.json"), b"{ this is not json").unwrap();
    let e = env_at(dir);
    assert!(e.mgr.snapshot().downloads.is_empty());
    let kept = std::fs::read_dir(e.dir.path()).unwrap().filter_map(|x| x.ok()).any(|x| x.file_name().to_string_lossy().contains("corrupt"));
    assert!(kept);
}

#[tokio::test(flavor = "multi_thread")]
async fn loads_a_state_file_written_by_the_old_electron_version() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = json!({
        "settings": { "maxSimultaneous": 3, "connections": 8, "downloadDir": dir.path().join("dl").to_string_lossy(), "minimizeToTray": true,
                      "runAtStartup": false, "notify": true, "removeMissing": true, "autoUpdate": true, "bypassKey": "F8", "bypassKeyV": 3, "bridgePort": 18642 },
        "queues": [ { "id": "main", "name": "Main", "enabled": true }, { "id": "q_abc", "name": "Movies", "enabled": false } ],
        "downloads": [
            { "id": "d_1", "url": "https://example.org/a.zip", "referer": null, "cookie": null, "dir": "/tmp", "queueId": "q_abc", "status": "downloading",
              "force": false, "hold": false, "filename": "a.zip", "size": 1000, "downloaded": 400, "speed": 55, "eta": 3, "error": null, "addedAt": 1700000000000i64,
              "finalUrl": "https://cdn.example.org/a.zip", "acceptRanges": true, "partPath": "/tmp/a.zip.danpart",
              "segments": [ { "start": 0, "end": 499, "pos": 300 }, { "start": 500, "end": 999, "pos": 600 } ] },
            { "id": "d_2", "url": "https://example.org/b.zip", "dir": "/tmp", "queueId": "gone-queue", "status": "completed", "filename": "b.zip", "size": 5, "downloaded": 5, "addedAt": 1, "completedAt": 2 }
        ],
        "themes": { "active": "midnight", "custom": [] },
        "token": "legacy-token-123",
        "window": { "x": 1, "y": 2, "width": 800, "height": 600, "maximized": false }
    });
    std::fs::write(dir.path().join("state.json"), legacy.to_string()).unwrap();
    let e = env_at(dir);
    let s = e.mgr.snapshot();
    assert_eq!(s.token, "legacy-token-123");
    assert_eq!(s.themes.active, "midnight");
    assert_eq!(s.queues.len(), 2);
    let d1 = s.downloads.iter().find(|d| d.id == "d_1").unwrap();
    assert_eq!(d1.status, Status::Paused);
    assert_eq!(d1.downloaded, 400);
    assert_eq!(s.downloads.iter().find(|d| d.id == "d_2").unwrap().queue_id, "main");
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_finished_files_are_dropped_from_the_list_but_never_touch_other_files() {
    let srv = start().await;
    let e = env();
    let id = e.mgr.add(AddReq { start: true, ..req(vec![srv.url("/ranged/1100000")]) }).added[0].clone();
    until("done", || status_of(&e.mgr, &id) == Status::Completed).await;
    assert!(e.mgr.prune_missing().await.is_empty(), "the file is there");
    std::fs::remove_file(e.mgr.file_path(&id).unwrap()).unwrap();
    assert_eq!(e.mgr.prune_missing().await, vec![id.clone()]);
    assert!(e.mgr.summary_of(&id).is_none());
    assert!(e.events.lock().iter().any(|ev| matches!(ev, Event::Pruned(1))));
}

#[tokio::test(flavor = "multi_thread")]
async fn themes_can_be_saved_applied_exported_and_deleted() {
    let e = env();
    let dark = e.mgr.active_theme();
    assert_eq!(dark.id, "dark");
    let mut t = serde_json::to_value(&dark).unwrap();
    t["name"] = json!("Mine");
    t.as_object_mut().unwrap().remove("author");
    let saved = e.mgr.theme_save(&t);
    assert!(saved.ok, "{:?}", saved.error);
    let id = saved.id.unwrap();
    assert_eq!(e.mgr.active_theme().name, "Mine");
    let (file, text) = e.mgr.theme_export(&id).unwrap();
    assert_eq!(file, "Mine.dantheme");
    let imported = e.mgr.theme_import_text(&text);
    assert!(imported.ok);
    assert_eq!(e.mgr.snapshot().themes.custom.len(), 2);
    assert!(!e.mgr.theme_save(&json!({ "name": "bad", "colors": {} })).ok);
    e.mgr.theme_delete(&id);
    assert_eq!(e.mgr.active_theme().id, "dark", "deleting the active theme falls back to the default");
}
