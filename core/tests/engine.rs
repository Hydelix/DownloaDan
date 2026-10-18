mod common;
use common::{pattern, start};
use downloadan_core::engine::{self, Control, JobSpec, Outcome, Resume, StopReason, Update, UpdateFn};
use downloadan_core::model::Segment;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Default, Clone)]
struct Seen {
    started: Option<(String, i64, bool, String, String, Vec<Segment>)>,
    last_segments: Option<Vec<Segment>>,
    notes: Vec<String>,
}

fn sink() -> (UpdateFn, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let s = seen.clone();
    let f: UpdateFn = Arc::new(move |u| {
        let mut g = s.lock();
        match u {
            Update::Note(Some(n)) => g.notes.push(n),
            Update::Note(None) => {}
            Update::Started { final_url, size, accept_ranges, filename, part_path, segments } => {
                g.started = Some((final_url, size, accept_ranges, filename, part_path, segments));
            }
            Update::Progress { segments, .. } => g.last_segments = Some(segments),
        }
    });
    (f, seen)
}

fn spec(url: String, dir: &std::path::Path, connections: usize) -> JobSpec {
    JobSpec { url, referer: None, cookie: None, dir: dir.to_path_buf(), filename: None, connections, resume: None }
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_a_ranged_file_over_several_connections() {
    let srv = start().await;
    let dir = tempfile::tempdir().unwrap();
    let (f, _) = sink();
    let out = engine::run(engine::build_client(), spec(srv.url("/ranged/6000000"), dir.path(), 4), Control::new(), f).await;
    let Outcome::Completed { filename, size, .. } = out else { panic!("{out:?}") };
    assert_eq!(size, 6_000_000);
    assert_eq!(filename, "6000000");
    assert_eq!(std::fs::read(dir.path().join(&filename)).unwrap(), pattern(6_000_000));
    let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(leftovers, vec!["6000000".to_string()], "no .danpart file may be left behind");
    let ranged = srv.hits.lock().iter().filter(|h| h.range.as_deref().is_some_and(|r| r != "bytes=0-0")).count();
    assert_eq!(ranged, 4, "one range request per connection");
}

#[tokio::test(flavor = "multi_thread")]
async fn falls_back_to_one_connection_when_ranges_are_not_supported() {
    let srv = start().await;
    let dir = tempfile::tempdir().unwrap();
    let (f, seen) = sink();
    let out = engine::run(engine::build_client(), spec(srv.url("/plain/2500000"), dir.path(), 8), Control::new(), f).await;
    let Outcome::Completed { filename, .. } = out else { panic!("{out:?}") };
    assert_eq!(std::fs::read(dir.path().join(filename)).unwrap(), pattern(2_500_000));
    assert_eq!(seen.lock().started.as_ref().unwrap().5.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn takes_the_file_name_from_content_disposition_and_never_overwrites() {
    let srv = start().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("naïve file.bin"), b"mine").unwrap();
    let (f, _) = sink();
    let out = engine::run(engine::build_client(), spec(srv.url("/named/300000"), dir.path(), 4), Control::new(), f).await;
    let Outcome::Completed { filename, .. } = out else { panic!("{out:?}") };
    assert_eq!(filename, "naïve file (1).bin");
    assert_eq!(std::fs::read(dir.path().join("naïve file.bin")).unwrap(), b"mine");
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_then_resume_continues_where_it_stopped() {
    let srv = start().await;
    let dir = tempfile::tempdir().unwrap();
    let size = 30_000_000i64;
    let client = engine::build_client();

    let (f, seen) = sink();
    let ctl = Control::new();
    let job = tokio::spawn(engine::run(client.clone(), spec(srv.url(&format!("/slow/{size}")), dir.path(), 8), ctl.clone(), f));
    // Wait until real progress has been reported (not a fixed time: a loaded machine may start slowly), then pause.
    let t = Instant::now();
    loop {
        let got: i64 = seen.lock().last_segments.as_ref().map(|s| s.iter().map(|x| (x.pos - x.start).max(0)).sum()).unwrap_or(0);
        if got > 1_000_000 {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "no download progress within 30 s");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    ctl.stop(StopReason::Pause);
    let out = job.await.unwrap();
    let Outcome::Paused { downloaded, segments } = out else { panic!("{out:?}") };
    assert!(downloaded > 0 && downloaded < size, "paused part-way, got {downloaded}; notes: {:?}; segments: {:?}; last progress: {:?}", seen.lock().notes, segments, seen.lock().last_segments);
    let segments = segments.unwrap();
    let (final_url, size_seen, accept, filename, part_path, _) = seen.lock().started.clone().unwrap();
    assert_eq!(size_seen, size);
    assert!(accept);
    assert!(PathBuf::from(&part_path).exists());

    let (f2, _) = sink();
    let mut s = spec(srv.url(&format!("/slow/{size}")), dir.path(), 8);
    s.resume = Some(Resume { final_url: Some(final_url), size, accept_ranges: true, part_path: PathBuf::from(part_path), filename, segments });
    let out = engine::run(client, s, Control::new(), f2).await;
    let Outcome::Completed { filename, .. } = out else { panic!("{out:?}") };
    assert_eq!(std::fs::read(dir.path().join(filename)).unwrap(), pattern(size as usize));
    let sent = srv.bytes_sent.load(std::sync::atomic::Ordering::SeqCst) as i64;
    assert!(sent < size + 3_000_000, "resume must not download everything again: server sent {sent} of {size}");
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_reports_cancelled() {
    let srv = start().await;
    let dir = tempfile::tempdir().unwrap();
    let (f, _) = sink();
    let ctl = Control::new();
    let job = tokio::spawn(engine::run(engine::build_client(), spec(srv.url("/slow/20000000"), dir.path(), 4), ctl.clone(), f));
    tokio::time::sleep(Duration::from_millis(600)).await;
    ctl.stop(StopReason::Cancel);
    assert!(matches!(job.await.unwrap(), Outcome::Cancelled));
}

#[tokio::test(flavor = "multi_thread")]
async fn cookies_are_not_sent_to_another_host_after_a_redirect() {
    let srv = start().await;
    let dir = tempfile::tempdir().unwrap();
    let (f, _) = sink();
    let mut s = spec(srv.url("/redir/1500000"), dir.path(), 2);
    s.cookie = Some("session=abc".into());
    s.referer = Some("https://example.org/page".into());
    let out = engine::run(engine::build_client(), s, Control::new(), f).await;
    assert!(matches!(out, Outcome::Completed { .. }), "{out:?}");
    let hits = srv.hits.lock().clone();
    let first = hits.iter().find(|h| h.path.starts_with("/redir")).unwrap();
    assert_eq!(first.cookie.as_deref(), Some("session=abc"));
    assert_eq!(first.referer.as_deref(), Some("https://example.org/page"));
    let after: Vec<_> = hits.iter().filter(|h| h.path.starts_with("/ranged")).collect();
    assert!(!after.is_empty());
    assert!(after.iter().all(|h| h.cookie.is_none()), "the cookie leaked to another host: {after:?}");
    assert!(after.iter().all(|h| h.referer.as_deref() == Some("https://example.org/page")));
}

#[tokio::test(flavor = "multi_thread")]
async fn http_errors_are_reported_without_retrying() {
    let srv = start().await;
    let dir = tempfile::tempdir().unwrap();
    let (f, _) = sink();
    let t = Instant::now();
    let out = engine::run(engine::build_client(), spec(srv.url("/missing"), dir.path(), 4), Control::new(), f).await;
    let Outcome::Error { message, restart, .. } = out else { panic!("{out:?}") };
    assert_eq!(message, "Server replied HTTP 404");
    assert!(!restart);
    assert!(t.elapsed() < Duration::from_secs(3));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn redirect_loops_and_other_schemes_are_refused() {
    let srv = start().await;
    let dir = tempfile::tempdir().unwrap();
    let (f, _) = sink();
    let out = engine::run(engine::build_client(), spec(srv.url("/loop"), dir.path(), 2), Control::new(), f).await;
    let Outcome::Error { message, .. } = out else { panic!("{out:?}") };
    assert_eq!(message, "Too many redirects");
    let (f, _) = sink();
    let out = engine::run(engine::build_client(), spec("ftp://example.org/x".into(), dir.path(), 2), Control::new(), f).await;
    let Outcome::Error { message, .. } = out else { panic!("{out:?}") };
    assert_eq!(message, "Only http(s) URLs are supported");
}

#[tokio::test(flavor = "multi_thread")]
async fn inspect_reports_name_size_and_resume_support() {
    let srv = start().await;
    let c = engine::build_client();
    let i = engine::inspect(&c, &srv.url("/named/12345"), None, None).await;
    assert!(i.ok);
    assert_eq!(i.size, 12345);
    assert!(i.resumable);
    assert_eq!(i.filename, "naïve file.bin");
    let i = engine::inspect(&c, &srv.url("/plain/777"), None, None).await;
    assert!(i.ok && !i.resumable);
    assert_eq!(i.size, 777);
    let i = engine::inspect(&c, &srv.url("/missing"), None, None).await;
    assert!(!i.ok);
    assert_eq!(i.error, "Server replied HTTP 404");
}
