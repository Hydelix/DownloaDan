//! The app's request handler for the browser extension. It knows NOTHING about how a request arrived: the HTTP
//! bridge calls it today, and a native-messaging host could call it tomorrow with the same operations. Each
//! transport describes the caller in a `Ctx`.
//!
//! Operations: hello (open), pair.start / pair.status (open, but only for allowed extensions, and the user must
//! approve), ping, add (need the token, or a trusted transport).

use crate::manager::{valid_cookie, valid_referer};
use crate::util::{constant_time_eq, random_hex};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

pub const PROTOCOL: u32 = 1; // bump when the message format changes incompatibly
pub const PAIR_TTL_MS: i64 = 60_000; // how long the user has to click Allow
const DENY_COOLDOWN_MS: i64 = 30_000; // after a Deny, the same extension must wait before asking again
const MAX_STARTS: usize = 5; // at most this many permission boxes ...
const START_WINDOW_MS: i64 = 10 * 60 * 1000; // ... per this window

/// Browser extensions that are allowed to ask to be paired. The first ID is this project's own extension; it is
/// fixed by the "key" field in extension/manifest.json. When the extension is published on a store, add the ID
/// that store gives it.
pub const ALLOWED_EXTENSION_IDS: [&str; 1] = ["hlceennagceocjoffimpobpeidkkmpnc"];

/// Who is calling, as described by the transport.
#[derive(Default, Clone, Debug)]
pub struct Ctx {
    /// The transport itself proves who the caller is.
    pub trusted: bool,
    /// The pairing token the caller presented.
    pub token: Option<String>,
    /// The Chromium extension ID the transport verified (HTTP: taken from the Origin header).
    pub extension_id: Option<String>,
    /// Firefox only: the random per-install address Firefox gives an extension (moz-extension://<this>).
    /// It is NOT the add-on's ID, so it cannot be checked against an allowlist; see `pair_start`.
    pub firefox_origin: Option<String>,
    /// Only used to show "Chrome" or "Edge" in the permission box.
    pub user_agent: Option<String>,
}

pub struct Reply {
    pub code: u16,
    pub body: Value,
}

fn reply(code: u16, body: Value) -> Reply {
    Reply { code, body }
}

#[derive(Clone, Debug)]
pub struct Pending {
    pub id: String,
    pub extension_id: String,
    pub browser: String,
    /// True when the extension's identity was checked against the allowlist (Chromium). Firefox cannot be checked.
    pub verified: bool,
    pub created_at: i64,
    /// None while the user has not answered; otherwise allowed | denied | expired.
    pub decision: Option<String>,
}

pub fn browser_name(ua: &str) -> &'static str {
    if ua.contains("Edg/") {
        "Microsoft Edge"
    } else if ua.contains("OPR/") {
        "Opera"
    } else if ua.contains("Firefox/") {
        "Mozilla Firefox"
    } else if ua.contains("Chrome/") {
        "Google Chrome"
    } else {
        "your browser"
    }
}

/// An add request from the browser, already checked.
pub struct AddRequest {
    pub url: String,
    pub referer: Option<String>,
    pub cookie: Option<String>,
}

pub struct ApiConfig {
    pub get_token: Box<dyn Fn() -> String + Send + Sync>,
    pub get_config: Box<dyn Fn() -> Value + Send + Sync>,
    pub on_add: Box<dyn Fn(AddRequest) + Send + Sync>,
    pub on_pair_request: Box<dyn Fn(Pending) + Send + Sync>,
    pub on_pair_end: Box<dyn Fn(Pending) + Send + Sync>,
    pub version: String,
    pub allowed_ids: Vec<String>,
    pub now: Box<dyn Fn() -> i64 + Send + Sync>,
}

#[derive(Default)]
struct PairState {
    pending: Option<Pending>,
    denied_at: HashMap<String, i64>,
    starts: Vec<i64>,
}

pub struct Api {
    cfg: ApiConfig,
    st: Mutex<PairState>,
}

impl Api {
    pub fn new(cfg: ApiConfig) -> Arc<Api> {
        Arc::new(Api { cfg, st: Mutex::new(PairState::default()) })
    }

    fn now(&self) -> i64 {
        (self.cfg.now)()
    }

    /// Who is calling, as one string: the Chromium extension ID, or "moz:<uuid>" for Firefox.
    fn caller_key(ctx: &Ctx) -> Option<String> {
        ctx.extension_id.clone().or_else(|| ctx.firefox_origin.as_ref().map(|u| format!("moz:{u}")))
    }

    fn authed(&self, ctx: &Ctx) -> bool {
        ctx.trusted || ctx.token.as_deref().is_some_and(|t| constant_time_eq(t, &(self.cfg.get_token)()))
    }

    /// Expire an unanswered request, and forget finished ones. Returns a request that just expired, so the caller can
    /// close its permission box after releasing the lock.
    fn tick(&self, st: &mut PairState) -> Option<Pending> {
        let p = st.pending.as_mut()?;
        let age = self.now() - p.created_at;
        let mut expired = None;
        if p.decision.is_none() && age > PAIR_TTL_MS {
            p.decision = Some("expired".into());
            expired = Some(p.clone());
        }
        if p.decision.is_some() && age > PAIR_TTL_MS + 30_000 {
            st.pending = None;
        }
        expired
    }

    pub fn handle(&self, op: &str, body: &Value, ctx: &Ctx) -> Reply {
        match op {
            "hello" => reply(200, json!({ "app": "DownloaDan", "version": self.cfg.version, "protocol": PROTOCOL })),
            "ping" => {
                if self.authed(ctx) {
                    reply(200, json!({ "ok": true, "app": "DownloaDan", "version": self.cfg.version, "protocol": PROTOCOL, "config": (self.cfg.get_config)() }))
                } else {
                    reply(401, json!({ "error": "bad-token" }))
                }
            }
            "add" => self.add(body, ctx),
            "pair.start" => self.pair_start(ctx),
            "pair.status" => self.pair_status(body, ctx),
            _ => reply(404, json!({ "error": "not-found" })),
        }
    }

    fn add(&self, b: &Value, ctx: &Ctx) -> Reply {
        if !self.authed(ctx) {
            return reply(401, json!({ "error": "bad-token" }));
        }
        let url = b.get("url").and_then(Value::as_str).map(str::trim).unwrap_or("");
        let ok = url.len() <= 8192 && url::Url::parse(url).map(|u| u.scheme() == "http" || u.scheme() == "https").unwrap_or(false);
        if !ok {
            return reply(400, json!({ "error": "bad-url" }));
        }
        let referer = b.get("referer").and_then(Value::as_str).and_then(valid_referer);
        let cookie = b.get("cookie").and_then(Value::as_str).and_then(valid_cookie);
        (self.cfg.on_add)(AddRequest { url: url.to_string(), referer, cookie });
        reply(200, json!({ "ok": true, "config": (self.cfg.get_config)() }))
    }

    fn pair_start(&self, ctx: &Ctx) -> Reply {
        // Chromium: the extension ID must be on the allowlist. Firefox: the browser only reveals a random per-install
        // address, so the ID cannot be checked. Those requests still need the user to click Allow, and they share the
        // same rate limits and cool-down, but the box tells the user the extension could not be verified.
        let Some(id) = Self::caller_key(ctx) else {
            return reply(403, json!({ "error": "unknown-extension" }));
        };
        let verified = !id.starts_with("moz:");
        if verified && !self.cfg.allowed_ids.contains(&id) {
            return reply(403, json!({ "error": "unknown-extension" }));
        }
        let mut expired = None;
        let (out, created) = {
            let mut st = self.st.lock();
            expired = expired.or(self.tick(&mut st));
            let now = self.now();
            if let Some(denied) = st.denied_at.get(&id) {
                if now - denied < DENY_COOLDOWN_MS {
                    let retry = ((DENY_COOLDOWN_MS - (now - denied)) as f64 / 1000.0).ceil() as i64;
                    return reply(429, json!({ "error": "denied-recently", "retryInSec": retry }));
                }
            }
            if let Some(p) = &st.pending {
                if p.decision.is_none() {
                    return if p.extension_id == id {
                        // the same extension asking again: reuse the open box
                        let left = (((PAIR_TTL_MS - (now - p.created_at)) as f64 / 1000.0).ceil() as i64).max(1);
                        reply(200, json!({ "pairId": p.id, "expiresInSec": left }))
                    } else {
                        reply(429, json!({ "error": "busy" }))
                    };
                }
            }
            st.starts.retain(|t| now - t < START_WINDOW_MS);
            if st.starts.len() >= MAX_STARTS {
                return reply(429, json!({ "error": "too-many" }));
            }
            st.starts.push(now);
            let p = Pending {
                id: random_hex(16),
                extension_id: id,
                browser: browser_name(ctx.user_agent.as_deref().unwrap_or("")).to_string(),
                verified,
                created_at: now,
                decision: None,
            };
            st.pending = Some(p.clone());
            (reply(200, json!({ "pairId": p.id, "expiresInSec": PAIR_TTL_MS / 1000 })), Some(p))
        };
        if let Some(p) = expired {
            (self.cfg.on_pair_end)(p);
        }
        if let Some(p) = created {
            (self.cfg.on_pair_request)(p);
        }
        out
    }

    fn pair_status(&self, b: &Value, ctx: &Ctx) -> Reply {
        let (out, expired) = {
            let mut st = self.st.lock();
            let expired = self.tick(&mut st);
            let now = self.now();
            let pair_id = b.get("pairId").and_then(Value::as_str);
            let caller = Self::caller_key(ctx);
            let out = match (&st.pending, &caller, pair_id) {
                (Some(p), Some(ext), Some(pid)) if &p.extension_id == ext && p.id == pid => match p.decision.as_deref() {
                    None => reply(200, json!({ "status": "pending", "secondsLeft": (((PAIR_TTL_MS - (now - p.created_at)) as f64 / 1000.0).ceil() as i64).max(0) })),
                    Some("allowed") => {
                        st.pending = None; // the token is handed over exactly once
                        reply(200, json!({ "status": "allowed", "token": (self.cfg.get_token)() }))
                    }
                    Some(other) => reply(200, json!({ "status": other })),
                },
                _ => reply(404, json!({ "status": "unknown" })),
            };
            (out, expired)
        };
        if let Some(p) = expired {
            (self.cfg.on_pair_end)(p);
        }
        out
    }

    /// The request waiting for the user's answer, if any.
    pub fn pending_pairing(&self) -> Option<Pending> {
        let (p, expired) = {
            let mut st = self.st.lock();
            let expired = self.tick(&mut st);
            (st.pending.clone().filter(|p| p.decision.is_none()), expired)
        };
        if let Some(e) = expired {
            (self.cfg.on_pair_end)(e);
        }
        p
    }

    /// Called by the permission box.
    pub fn resolve_pairing(&self, pair_id: &str, allow: bool) -> bool {
        let mut st = self.st.lock();
        self.tick(&mut st);
        let now = self.now();
        let ext = match st.pending.as_mut() {
            Some(p) if p.id == pair_id && p.decision.is_none() => {
                p.decision = Some(if allow { "allowed" } else { "denied" }.into());
                p.extension_id.clone()
            }
            _ => return false,
        };
        if !allow {
            st.denied_at.insert(ext, now);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    struct Fixture {
        api: Arc<Api>,
        clock: Arc<AtomicI64>,
        added: Arc<Mutex<Vec<String>>>,
        boxes_opened: Arc<AtomicUsize>,
    }

    fn fixture() -> Fixture {
        let clock = Arc::new(AtomicI64::new(1_000_000));
        let added = Arc::new(Mutex::new(vec![]));
        let opened = Arc::new(AtomicUsize::new(0));
        let (c, a, o) = (clock.clone(), added.clone(), opened.clone());
        let api = Api::new(ApiConfig {
            get_token: Box::new(|| "secret-token".into()),
            get_config: Box::new(|| json!({ "bypassKey": "F8" })),
            on_add: Box::new(move |r| a.lock().push(r.url)),
            on_pair_request: Box::new(move |_| {
                o.fetch_add(1, Ordering::SeqCst);
            }),
            on_pair_end: Box::new(|_| {}),
            version: "2.0.0".into(),
            allowed_ids: vec!["goodext".into()],
            now: Box::new(move || c.load(Ordering::SeqCst)),
        });
        Fixture { api, clock, added, boxes_opened: opened }
    }

    fn ext(id: &str) -> Ctx {
        Ctx { extension_id: Some(id.into()), user_agent: Some("Mozilla/5.0 Chrome/130 Edg/130".into()), ..Default::default() }
    }

    fn firefox(uuid: &str) -> Ctx {
        Ctx { firefox_origin: Some(uuid.into()), user_agent: Some("Mozilla/5.0 (Windows NT 10.0; rv:140.0) Gecko/20100101 Firefox/140.0".into()), ..Default::default() }
    }

    #[test]
    fn add_needs_the_token_and_a_http_url() {
        let f = fixture();
        let bad = Ctx { token: Some("nope".into()), ..Default::default() };
        assert_eq!(f.api.handle("add", &json!({"url": "https://a.test/f.zip"}), &bad).code, 401);
        let good = Ctx { token: Some("secret-token".into()), ..Default::default() };
        assert_eq!(f.api.handle("add", &json!({"url": "file:///etc/passwd"}), &good).code, 400);
        assert_eq!(f.api.handle("add", &json!({"url": "javascript:alert(1)"}), &good).code, 400);
        assert_eq!(f.api.handle("add", &json!({"url": "https://a.test/f.zip"}), &good).code, 200);
        assert_eq!(f.added.lock().as_slice(), ["https://a.test/f.zip"]);
    }

    #[test]
    fn only_known_extensions_can_start_pairing() {
        let f = fixture();
        assert_eq!(f.api.handle("pair.start", &json!({}), &ext("evil")).code, 403);
        assert_eq!(f.api.handle("pair.start", &json!({}), &Ctx::default()).code, 403);
        assert_eq!(f.boxes_opened.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn allowed_pairing_hands_over_the_token_once() {
        let f = fixture();
        let r = f.api.handle("pair.start", &json!({}), &ext("goodext"));
        assert_eq!(r.code, 200);
        let pid = r.body["pairId"].as_str().unwrap().to_string();
        assert_eq!(f.boxes_opened.load(Ordering::SeqCst), 1);
        assert_eq!(f.api.pending_pairing().unwrap().browser, "Microsoft Edge");
        // asking again reuses the open box
        assert_eq!(f.api.handle("pair.start", &json!({}), &ext("goodext")).body["pairId"], pid);
        assert_eq!(f.boxes_opened.load(Ordering::SeqCst), 1);
        assert_eq!(f.api.handle("pair.status", &json!({"pairId": pid}), &ext("goodext")).body["status"], "pending");
        assert!(f.api.resolve_pairing(&pid, true));
        let s = f.api.handle("pair.status", &json!({"pairId": pid}), &ext("goodext"));
        assert_eq!(s.body["status"], "allowed");
        assert_eq!(s.body["token"], "secret-token");
        assert_eq!(f.api.handle("pair.status", &json!({"pairId": pid}), &ext("goodext")).code, 404);
    }

    #[test]
    fn firefox_can_pair_but_is_marked_unverified() {
        let f = fixture();
        let ff = firefox("11111111-2222-4333-8444-555555555555");
        let r = f.api.handle("pair.start", &json!({}), &ff);
        assert_eq!(r.code, 200);
        let pid = r.body["pairId"].as_str().unwrap().to_string();
        let p = f.api.pending_pairing().unwrap();
        assert_eq!(p.browser, "Mozilla Firefox");
        assert!(!p.verified, "Firefox identities cannot be checked");
        // another Firefox extension cannot collect the answer
        let other = firefox("99999999-2222-4333-8444-555555555555");
        assert_eq!(f.api.handle("pair.status", &json!({"pairId": pid}), &other).code, 404);
        assert!(f.api.resolve_pairing(&pid, true));
        let s = f.api.handle("pair.status", &json!({"pairId": pid}), &ff);
        assert_eq!(s.body["status"], "allowed");
        assert_eq!(s.body["token"], "secret-token");
    }

    #[test]
    fn a_chromium_extension_is_still_verified() {
        let f = fixture();
        f.api.handle("pair.start", &json!({}), &ext("goodext"));
        assert!(f.api.pending_pairing().unwrap().verified);
    }

    #[test]
    fn another_extension_cannot_read_the_answer() {
        let f = fixture();
        let pid = f.api.handle("pair.start", &json!({}), &ext("goodext")).body["pairId"].as_str().unwrap().to_string();
        f.api.resolve_pairing(&pid, true);
        assert_eq!(f.api.handle("pair.status", &json!({"pairId": pid}), &ext("otherext")).code, 404);
    }

    #[test]
    fn deny_means_wait_before_asking_again() {
        let f = fixture();
        let pid = f.api.handle("pair.start", &json!({}), &ext("goodext")).body["pairId"].as_str().unwrap().to_string();
        assert!(f.api.resolve_pairing(&pid, false));
        assert_eq!(f.api.handle("pair.status", &json!({"pairId": pid}), &ext("goodext")).body["status"], "denied");
        assert_eq!(f.api.handle("pair.start", &json!({}), &ext("goodext")).code, 429);
        f.clock.fetch_add(31_000, Ordering::SeqCst);
        assert_eq!(f.api.handle("pair.start", &json!({}), &ext("goodext")).code, 200);
    }

    #[test]
    fn unanswered_requests_expire() {
        let f = fixture();
        let pid = f.api.handle("pair.start", &json!({}), &ext("goodext")).body["pairId"].as_str().unwrap().to_string();
        f.clock.fetch_add(61_000, Ordering::SeqCst);
        assert!(f.api.pending_pairing().is_none());
        assert!(!f.api.resolve_pairing(&pid, true));
        assert_eq!(f.api.handle("pair.status", &json!({"pairId": pid}), &ext("goodext")).body["status"], "expired");
    }

    #[test]
    fn pairing_prompts_are_rate_limited() {
        let f = fixture();
        for _ in 0..5 {
            let pid = f.api.handle("pair.start", &json!({}), &ext("goodext")).body["pairId"].as_str().unwrap().to_string();
            f.api.resolve_pairing(&pid, true);
            f.api.handle("pair.status", &json!({"pairId": pid}), &ext("goodext")); // collects the token
        }
        assert_eq!(f.api.handle("pair.start", &json!({}), &ext("goodext")).code, 429);
    }
}
