//! A small HTTP server for the tests: range support, slow streaming, redirects, errors.
#![allow(dead_code)]

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use parking_lot::Mutex;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

#[derive(Clone, Debug)]
pub struct Hit {
    pub path: String,
    pub host: String,
    pub range: Option<String>,
    pub cookie: Option<String>,
    pub referer: Option<String>,
}

pub struct TestServer {
    pub addr: SocketAddr,
    pub hits: Arc<Mutex<Vec<Hit>>>,
    pub bytes_sent: Arc<AtomicU64>,
}

impl TestServer {
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.addr.port(), path)
    }
}

/// Deterministic file content, so a download can be compared byte for byte.
pub fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 31 + 7) % 251) as u8).collect()
}

type Body = BoxBody<Bytes, Infallible>;

fn full(code: u16, bytes: Vec<u8>) -> Response<Body> {
    let mut r = Response::new(Full::new(Bytes::from(bytes)).boxed());
    *r.status_mut() = StatusCode::from_u16(code).unwrap();
    r
}

fn parse_range(h: &str, size: usize) -> Option<(usize, usize)> {
    let r = h.strip_prefix("bytes=")?;
    let (a, b) = r.split_once('-')?;
    let a: usize = a.parse().ok()?;
    let b: usize = if b.is_empty() { size - 1 } else { b.parse().ok()? };
    (a <= b && a < size).then_some((a, b.min(size - 1)))
}

async fn handle(req: Request<Incoming>, hits: Arc<Mutex<Vec<Hit>>>, sent: Arc<AtomicU64>, port: u16) -> Result<Response<Body>, Infallible> {
    let h = req.headers();
    let get = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
    let range = get("range");
    hits.lock().push(Hit { path: req.uri().path().to_string(), host: get("host").unwrap_or_default(), range: range.clone(), cookie: get("cookie"), referer: get("referer") });
    let path = req.uri().path().to_string();
    let seg: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let size_of = |i: usize| seg.get(i).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);

    let resp = match seg[0] {
        "missing" => full(404, b"nope".to_vec()),
        "redir" => {
            let mut r = full(302, vec![]);
            r.headers_mut().insert("location", format!("http://localhost:{port}/ranged/{}", size_of(1)).parse().unwrap());
            r
        }
        "loop" => {
            let mut r = full(302, vec![]);
            r.headers_mut().insert("location", "/loop".parse().unwrap());
            r
        }
        "plain" => {
            let size = size_of(1);
            sent.fetch_add(size as u64, Ordering::SeqCst);
            full(200, pattern(size))
        }
        "ranged" | "named" | "slow" => {
            let size = size_of(1);
            let data = pattern(size);
            let (status, a, b) = match range.as_deref().and_then(|r| parse_range(r, size)) {
                Some((a, b)) => (206, a, b),
                None => (200, 0, size.saturating_sub(1)),
            };
            let slice = data[a..=b].to_vec();
            let mut r = if seg[0] == "slow" {
                let stream = futures_util::stream::unfold((slice, 0usize, sent.clone()), |(data, off, sent)| async move {
                    if off >= data.len() {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    let end = (off + 64 * 1024).min(data.len());
                    let chunk = Bytes::copy_from_slice(&data[off..end]);
                    sent.fetch_add(chunk.len() as u64, Ordering::SeqCst);
                    Some((Ok::<_, Infallible>(Frame::data(chunk)), (data, end, sent)))
                });
                let mut r = Response::new(BodyExt::boxed(StreamBody::new(stream)));
                r.headers_mut().insert("content-length", (b - a + 1).to_string().parse().unwrap());
                r
            } else {
                sent.fetch_add(slice.len() as u64, Ordering::SeqCst);
                full(200, slice)
            };
            *r.status_mut() = StatusCode::from_u16(status).unwrap();
            if status == 206 {
                r.headers_mut().insert("content-range", format!("bytes {a}-{b}/{size}").parse().unwrap());
            }
            r.headers_mut().insert("accept-ranges", "bytes".parse().unwrap());
            if seg[0] == "named" {
                r.headers_mut().insert("content-disposition", "attachment; filename*=UTF-8''na%C3%AFve%20file.bin".parse().unwrap());
            }
            r
        }
        _ => full(404, vec![]),
    };
    Ok(resp)
}

pub async fn start() -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(Mutex::new(vec![]));
    let sent = Arc::new(AtomicU64::new(0));
    let (h, s) = (hits.clone(), sent.clone());
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            let (h, s) = (h.clone(), s.clone());
            tokio::spawn(async move {
                let svc = service_fn(move |req| handle(req, h.clone(), s.clone(), addr.port()));
                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), svc).await;
            });
        }
    });
    TestServer { addr, hits, bytes_sent: sent }
}
