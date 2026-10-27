//! One download speed limit shared by every running download.
//!
//! A bucket that refills at the limit and is spent as bytes arrive. A download that has taken more than the bucket
//! holds simply waits before asking the server for more, which slows the connection down through normal TCP
//! back-pressure. All connections of all downloads draw from the same bucket, so the limit applies to the total.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Bucket {
    /// Bytes that may be used right now. Negative while the connections are paying back what they took early.
    tokens: f64,
    last: Instant,
}

#[derive(Debug)]
pub struct Limiter {
    /// Bytes per second; 0 means no limit.
    rate: AtomicU64,
    bucket: Mutex<Bucket>,
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Limiter {
    pub fn new() -> Self {
        Limiter { rate: AtomicU64::new(0), bucket: Mutex::new(Bucket { tokens: 0.0, last: Instant::now() }) }
    }

    /// Bytes per second; 0 removes the limit. Takes effect for running downloads straight away.
    pub fn set(&self, bytes_per_sec: u64) {
        self.rate.store(bytes_per_sec, Ordering::Relaxed);
        let mut b = self.bucket.lock();
        b.tokens = 0.0; // start fresh, so a change is felt at once and neither a saved-up burst nor old debt carries over
        b.last = Instant::now();
    }

    pub fn get(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Wait until `n` more bytes may be used. Returns `Err` if `token` is cancelled while waiting.
    pub async fn acquire(&self, n: usize, token: &CancellationToken) -> Result<(), ()> {
        let rate = self.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return Ok(());
        }
        let wait = {
            let rate = rate as f64;
            let mut b = self.bucket.lock();
            let now = Instant::now();
            // refill, but never save up more than a quarter of a second, so a long pause is not followed by a burst
            b.tokens = (b.tokens + now.duration_since(b.last).as_secs_f64() * rate).min(rate * 0.25);
            b.last = now;
            b.tokens -= n as f64;
            if b.tokens >= 0.0 {
                return Ok(());
            }
            Duration::from_secs_f64(-b.tokens / rate)
        };
        tokio::select! {
            _ = token.cancelled() => Err(()),
            _ = tokio::time::sleep(wait) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_limit_never_waits() {
        let l = Limiter::new();
        let t = CancellationToken::new();
        let start = Instant::now();
        for _ in 0..100 {
            l.acquire(1_000_000, &t).await.unwrap();
        }
        assert!(start.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn a_limit_spaces_out_the_bytes() {
        let l = Limiter::new();
        l.set(1_000_000); // 1 MB/s
        let t = CancellationToken::new();
        let start = Instant::now();
        for _ in 0..5 {
            l.acquire(100_000, &t).await.unwrap(); // 500 KB in all
        }
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(350), "finished too fast: {took:?}");
        assert!(took < Duration::from_millis(1500), "finished too slowly: {took:?}");
    }

    #[tokio::test]
    async fn waiting_can_be_cancelled() {
        let l = Limiter::new();
        l.set(10); // 10 bytes per second
        let t = CancellationToken::new();
        let t2 = t.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            t2.cancel();
        });
        let start = Instant::now();
        assert!(l.acquire(1_000_000, &t).await.is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn changing_the_limit_takes_effect_at_once() {
        let l = Limiter::new();
        l.set(10);
        l.set(0);
        let t = CancellationToken::new();
        let start = Instant::now();
        l.acquire(1_000_000, &t).await.unwrap();
        assert!(start.elapsed() < Duration::from_millis(100));
    }
}
