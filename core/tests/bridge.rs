use downloadan_core::api::{Api, ApiConfig, ALLOWED_EXTENSION_IDS};
use downloadan_core::bridge::Bridge;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;

const EXT: &str = "chrome-extension://hlceennagceocjoffimpobpeidkkmpnc";

struct Rig {
    port: u16,
    http: reqwest::Client,
    added: Arc<Mutex<Vec<String>>>,
    api: Arc<Api>,
    _bridge: Arc<Bridge>,
}

async fn rig() -> Rig {
    let added = Arc::new(Mutex::new(vec![]));
    let a = added.clone();
    let api = Api::new(ApiConfig {
        get_token: Box::new(|| "tok-123".into()),
        get_config: Box::new(|| json!({ "bypassKey": "F8" })),
        on_add: Box::new(move |r| a.lock().push(r.url)),
        on_pair_request: Box::new(|_| {}),
        on_pair_end: Box::new(|_| {}),
        version: "2.0.0".into(),
        allowed_ids: ALLOWED_EXTENSION_IDS.iter().map(|s| s.to_string()).collect(),
        now: Box::new(downloadan_core::util::now_ms),
    });
    let bridge = Bridge::new(api.clone(), tokio::runtime::Handle::current());
    let status = bridge.listen_any(&(28650..28660).collect::<Vec<u16>>()).await;
    assert!(status.ok, "{status:?}");
    Rig { port: status.port.unwrap(), http: reqwest::Client::new(), added, api, _bridge: bridge }
}

impl Rig {
    fn url(&self, p: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, p)
    }
    async fn post(&self, p: &str, origin: Option<&str>, token: Option<&str>, body: &Value) -> (u16, Value) {
        let mut r = self.http.post(self.url(p)).header("content-type", "application/json").body(body.to_string());
        if let Some(o) = origin {
            r = r.header("origin", o);
        }
        if let Some(t) = token {
            r = r.header("x-downloadan-token", t);
        }
        let resp = r.send().await.unwrap();
        let code = resp.status().as_u16();
        (code, serde_json::from_slice(&resp.bytes().await.unwrap()).unwrap_or(Value::Null))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_is_open_and_has_no_cors_headers() {
    let r = rig().await;
    let resp = r.http.get(r.url("/hello")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get("access-control-allow-origin").is_none(), "web pages must not be able to read it");
    assert_eq!(resp.headers()["cache-control"], "no-store");
    let v: Value = serde_json::from_slice(&resp.bytes().await.unwrap()).unwrap();
    assert_eq!(v["app"], "DownloaDan");
    assert_eq!(v["protocol"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_foreign_hosts_and_web_page_origins() {
    let r = rig().await;
    let resp = r.http.get(r.url("/hello")).header("host", "evil.example").send().await.unwrap();
    assert_eq!(resp.status(), 403, "DNS-rebinding protection");
    let resp = r.http.get(r.url("/hello")).header("origin", "https://evil.example").send().await.unwrap();
    assert_eq!(resp.status(), 403, "ordinary web pages are refused");
    let resp = r.http.get(r.url("/hello")).header("origin", EXT).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let resp = r.http.get(r.url("/hello")).header("host", format!("localhost:{}", r.port)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn add_needs_the_token_and_valid_json() {
    let r = rig().await;
    let body = json!({ "url": "https://example.org/f.zip", "cookie": "a=b" });
    assert_eq!(r.post("/add", Some(EXT), None, &body).await.0, 401);
    assert_eq!(r.post("/add", Some(EXT), Some("wrong"), &body).await.0, 401);
    assert!(r.added.lock().is_empty());
    let (code, v) = r.post("/add", Some(EXT), Some("tok-123"), &body).await;
    assert_eq!(code, 200);
    assert_eq!(v["config"]["bypassKey"], "F8");
    assert_eq!(r.added.lock().as_slice(), ["https://example.org/f.zip"]);
    assert_eq!(r.post("/add", Some(EXT), Some("tok-123"), &json!({ "url": "file:///etc/passwd" })).await.0, 400);

    // not JSON
    let resp = r.http.post(r.url("/add")).header("content-type", "text/plain").body("x").send().await.unwrap();
    assert_eq!(resp.status(), 415);
    let resp = r.http.post(r.url("/add")).header("content-type", "application/json").body("{nope").send().await.unwrap();
    assert_eq!(resp.status(), 400);
    // too big
    let big = "x".repeat(70_000);
    let resp = r.http.post(r.url("/add")).header("content-type", "application/json").body(format!("{{\"url\":\"{big}\"}}")).send().await.unwrap();
    assert_eq!(resp.status(), 413);
    // unknown paths and methods
    assert_eq!(r.http.get(r.url("/nothing")).send().await.unwrap().status(), 404);
    assert_eq!(r.http.put(r.url("/add")).send().await.unwrap().status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn full_pairing_flow_over_http() {
    let r = rig().await;
    // a page or an unknown extension cannot even start
    assert_eq!(r.post("/pair/start", None, None, &json!({})).await.0, 403);
    assert_eq!(r.post("/pair/start", Some("chrome-extension://abcdefabcdef"), None, &json!({})).await.0, 403);
    let (code, v) = r.post("/pair/start", Some(EXT), None, &json!({})).await;
    assert_eq!(code, 200);
    let pid = v["pairId"].as_str().unwrap().to_string();
    let (_, s) = r.post("/pair/status", Some(EXT), None, &json!({ "pairId": pid })).await;
    assert_eq!(s["status"], "pending");
    assert!(r.api.resolve_pairing(&pid, true));
    let (_, s) = r.post("/pair/status", Some(EXT), None, &json!({ "pairId": pid })).await;
    assert_eq!(s["status"], "allowed");
    assert_eq!(s["token"], "tok-123");
    let resp = r.http.get(r.url("/ping")).header("x-downloadan-token", "tok-123").header("origin", EXT).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}
