//! HTTP transport for the browser extension.
//!
//! - Listens on 127.0.0.1 only, and checks the Host header (blocks DNS rebinding).
//! - Rejects any Origin that is not a browser extension, and sends no CORS headers, so web pages cannot use it.
//! - All decisions are made by the `Api`; this file only turns HTTP into `api.handle()` calls and back.
//! - It can bind to the first free port from a list; the extension scans the same range to find the app.

use crate::api::{Api, Ctx};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{HeaderValue, CONTENT_TYPE, HOST, ORIGIN, USER_AGENT};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const MAX_BODY: usize = 64 * 1024;

#[derive(Serialize, Clone, Debug, Default)]
pub struct BridgeStatus {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct Bridge {
    api: Arc<Api>,
    rt: tokio::runtime::Handle,
    stop: Mutex<Option<CancellationToken>>,
}

enum ExtOrigin<'a> {
    /// `chrome-extension://<32 letters>`: the ID is the extension's real, fixed ID.
    Chromium(&'a str),
    /// `moz-extension://<uuid>`: a random address Firefox makes up for each install; NOT the add-on ID.
    Firefox(&'a str),
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36 && s.bytes().enumerate().all(|(i, b)| if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_digit() || (b'a'..=b'f').contains(&b) })
}

fn parse_extension_origin(origin: &str) -> Option<ExtOrigin<'_>> {
    if let Some(id) = origin.strip_prefix("chrome-extension://") {
        return (!id.is_empty() && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())).then_some(ExtOrigin::Chromium(id));
    }
    let id = origin.strip_prefix("moz-extension://")?;
    is_uuid(id).then_some(ExtOrigin::Firefox(id))
}

fn json_response(code: u16, body: Value) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(Bytes::from(body.to_string())));
    *r.status_mut() = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let h = r.headers_mut();
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    h.insert("cache-control", HeaderValue::from_static("no-store"));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    r
}

async fn handle(api: Arc<Api>, port: u16, req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(handle_inner(api, port, req).await)
}

async fn handle_inner(api: Arc<Api>, port: u16, req: Request<Incoming>) -> Response<Full<Bytes>> {
    let headers = req.headers();
    let host = headers.get(HOST).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return json_response(403, json!({ "error": "bad-host" }));
    }
    let origin = headers.get(ORIGIN).and_then(|v| v.to_str().ok()).map(str::to_string);
    let parsed = origin.as_deref().and_then(parse_extension_origin);
    if origin.is_some() && parsed.is_none() {
        return json_response(403, json!({ "error": "bad-origin" }));
    }
    let token = headers
        .get("x-downloadan-token")
        .or_else(|| headers.get("x-fluxdm-token")) // the old header name still works until the extension is reloaded
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let ctx = Ctx {
        trusted: false,
        token,
        extension_id: match &parsed {
            Some(ExtOrigin::Chromium(id)) => Some(id.to_string()),
            _ => None,
        },
        firefox_origin: match &parsed {
            Some(ExtOrigin::Firefox(id)) => Some(id.to_string()),
            _ => None,
        },
        user_agent: headers.get(USER_AGENT).and_then(|v| v.to_str().ok()).map(str::to_string),
    };
    let content_type_json = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.to_ascii_lowercase().starts_with("application/json"));
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let reply = |r: crate::api::Reply| json_response(r.code, r.body);

    if method == Method::GET {
        return match path.as_str() {
            "/hello" => reply(api.handle("hello", &Value::Null, &ctx)),
            "/ping" => reply(api.handle("ping", &Value::Null, &ctx)),
            _ => json_response(404, json!({ "error": "not-found" })),
        };
    }
    let op = match path.as_str() {
        "/add" => "add",
        "/pair/start" => "pair.start",
        "/pair/status" => "pair.status",
        _ => return json_response(404, json!({ "error": "not-found" })),
    };
    if method != Method::POST {
        return json_response(404, json!({ "error": "not-found" }));
    }
    if !content_type_json {
        return json_response(415, json!({ "error": "json-only" }));
    }
    let collected = tokio::time::timeout(Duration::from_secs(5), Limited::new(req.into_body(), MAX_BODY).collect()).await;
    let bytes = match collected {
        Ok(Ok(c)) => c.to_bytes(),
        Ok(Err(_)) => return json_response(413, json!({ "error": "too-large" })),
        Err(_) => return json_response(408, json!({ "error": "timeout" })),
    };
    let body: Value = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => return json_response(400, json!({ "error": "bad-json" })),
        }
    };
    reply(api.handle(op, &body, &ctx))
}

impl Bridge {
    pub fn new(api: Arc<Api>, rt: tokio::runtime::Handle) -> Arc<Bridge> {
        Arc::new(Bridge { api, rt, stop: Mutex::new(None) })
    }

    pub async fn listen(&self, port: u16) -> Result<u16, String> {
        self.close();
        let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                format!("Port {port} is already in use")
            } else {
                e.to_string()
            }
        })?;
        let stop = CancellationToken::new();
        *self.stop.lock() = Some(stop.clone());
        let api = self.api.clone();
        self.rt.spawn(async move {
            loop {
                let stream = tokio::select! {
                    _ = stop.cancelled() => break,
                    accepted = listener.accept() => match accepted {
                        Ok((s, _)) => s,
                        Err(_) => {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            continue;
                        }
                    },
                };
                let api = api.clone();
                let stop = stop.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc = service_fn(move |req| handle(api.clone(), port, req));
                    let conn = http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(Duration::from_secs(5))
                        .keep_alive(false)
                        .serve_connection(io, svc);
                    tokio::select! {
                        _ = stop.cancelled() => {}
                        _ = conn => {}
                    }
                });
            }
        });
        Ok(port)
    }

    /// Try each port in turn and keep the first one that is free.
    pub async fn listen_any(&self, ports: &[u16]) -> BridgeStatus {
        let mut last = "No ports to try".to_string();
        for &p in ports {
            match self.listen(p).await {
                Ok(port) => return BridgeStatus { ok: true, port: Some(port), error: None },
                Err(e) => last = e,
            }
        }
        let range = match (ports.first(), ports.last()) {
            (Some(a), Some(b)) => format!(" (tried {a}-{b})"),
            _ => String::new(),
        };
        BridgeStatus { ok: false, port: None, error: Some(format!("{last}{range}")) }
    }

    pub fn close(&self) {
        if let Some(t) = self.stop.lock().take() {
            t.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        assert!(matches!(parse_extension_origin("chrome-extension://abcdef0123"), Some(ExtOrigin::Chromium("abcdef0123"))));
        let uuid = "0b5d3c1e-8a47-4f6b-9c21-7e3a5d9f1b04";
        assert!(matches!(parse_extension_origin(&format!("moz-extension://{uuid}")), Some(ExtOrigin::Firefox(u)) if u == uuid));
        assert!(parse_extension_origin("moz-extension://abc123").is_none(), "Firefox addresses are UUIDs");
        assert!(parse_extension_origin("moz-extension://0B5D3C1E-8A47-4F6B-9C21-7E3A5D9F1B04").is_none());
        assert!(parse_extension_origin("https://evil.example").is_none());
        assert!(parse_extension_origin("chrome-extension://UPPER").is_none());
        assert!(parse_extension_origin("chrome-extension://").is_none());
        assert!(parse_extension_origin("chrome-extension://abc/def").is_none());
    }
}
