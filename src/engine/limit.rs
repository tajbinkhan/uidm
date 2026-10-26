//! A token bucket shared by every segment of every download, so the total
//! download speed stays within one limit.
//!
//! Each read takes as many tokens as it read bytes. The balance may go
//! negative: the reader then sleeps until the bucket has refilled to zero, so
//! the average rate holds even when one read is larger than a second's worth
//! of tokens. The limit can change or turn off at any time; sleepers notice
//! within `SLICE` and stop waiting on the old rate.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// Longest single sleep, so a limit change or cancel is noticed promptly.
const SLICE: Duration = Duration::from_millis(100);
/// Most tokens that can build up while idle, as a fraction of a second.
const BURST_SECS: f64 = 0.25;
/// Smallest read while a limit is on, so tiny limits still make progress.
const MIN_CHUNK: usize = 1024;

struct State {
    /// Bytes per second, or `None` when unlimited.
    rate: Option<u64>,
    tokens: f64,
    last: Instant,
    /// Bumped on every rate change so sleepers drop a debt priced at the old
    /// rate.
    generation: u64,
}

pub struct RateLimiter {
    state: Mutex<State>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(None)
    }
}

impl RateLimiter {
    pub fn new(rate: Option<u64>) -> Self {
        RateLimiter {
            state: Mutex::new(State {
                rate: rate.filter(|r| *r > 0),
                tokens: 0.0,
                last: Instant::now(),
                generation: 0,
            }),
        }
    }

    /// Change the limit; `None` (or 0) turns it off. Takes effect at once for
    /// every running download.
    pub fn set_rate(&self, rate: Option<u64>) {
        let mut s = self.state.lock().unwrap();
        let rate = rate.filter(|r| *r > 0);
        if s.rate != rate {
            s.rate = rate;
            s.tokens = 0.0;
            s.last = Instant::now();
            s.generation += 1;
        }
    }

    pub fn rate(&self) -> Option<u64> {
        self.state.lock().unwrap().rate
    }

    /// How many bytes one read should ask for: the full buffer when unlimited,
    /// otherwise about a tenth of a second's worth, so throttled downloads
    /// flow smoothly instead of in bursts.
    pub fn chunk_size(&self, buf_len: usize) -> usize {
        match self.rate() {
            None => buf_len,
            Some(rate) => ((rate / 10) as usize).clamp(MIN_CHUNK, buf_len.max(MIN_CHUNK)).min(buf_len),
        }
    }

    /// Account for `n` bytes just read, sleeping as long as the limit needs.
    /// Returns early if `cancel` is set.
    pub fn consume(&self, n: usize, cancel: &AtomicBool) {
        let (deadline, generation) = {
            let mut s = self.state.lock().unwrap();
            let Some(rate) = s.rate else {
                return;
            };
            let now = Instant::now();
            let rate_f = rate as f64;
            let refill = now.duration_since(s.last).as_secs_f64() * rate_f;
            s.tokens = (s.tokens + refill).min(rate_f * BURST_SECS);
            s.last = now;
            s.tokens -= n as f64;
            if s.tokens >= 0.0 {
                return;
            }
            let wait = Duration::from_secs_f64(-s.tokens / rate_f);
            (now + wait, s.generation)
        };

        loop {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            if self.state.lock().unwrap().generation != generation {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            thread::sleep((deadline - now).min(SLICE));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn unlimited_never_waits() {
        let l = RateLimiter::new(None);
        let cancel = AtomicBool::new(false);
        let t = Instant::now();
        for _ in 0..1000 {
            l.consume(1 << 20, &cancel);
        }
        assert!(t.elapsed() < Duration::from_millis(200));
        assert_eq!(l.chunk_size(65536), 65536);
    }

    #[test]
    fn holds_threads_to_the_limit() {
        // Four threads share 1 MiB/s and move 1.5 MiB in total: about 1.5s.
        let rate = 1024 * 1024;
        let l = Arc::new(RateLimiter::new(Some(rate)));
        let total: usize = 3 * 512 * 1024;
        let start = Instant::now();
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let l = Arc::clone(&l);
                thread::spawn(move || {
                    let cancel = AtomicBool::new(false);
                    let chunk = l.chunk_size(64 * 1024);
                    let mut left = total / 4;
                    while left > 0 {
                        let n = chunk.min(left);
                        l.consume(n, &cancel);
                        left -= n;
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let secs = start.elapsed().as_secs_f64();
        let expected = total as f64 / rate as f64;
        assert!(
            (secs - expected).abs() / expected < 0.10,
            "took {secs:.2}s, expected about {expected:.2}s"
        );
    }

    #[test]
    fn turning_the_limit_off_releases_sleepers() {
        // 1 KB/s and a 64 KiB read would sleep about a minute.
        let l = Arc::new(RateLimiter::new(Some(1024)));
        let l2 = Arc::clone(&l);
        let t = Instant::now();
        let h = thread::spawn(move || l2.consume(64 * 1024, &AtomicBool::new(false)));
        thread::sleep(Duration::from_millis(200));
        l.set_rate(None);
        h.join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn cancel_releases_sleepers() {
        let l = Arc::new(RateLimiter::new(Some(1024)));
        let cancel = Arc::new(AtomicBool::new(false));
        let (l2, c2) = (Arc::clone(&l), Arc::clone(&cancel));
        let t = Instant::now();
        let h = thread::spawn(move || l2.consume(64 * 1024, &c2));
        thread::sleep(Duration::from_millis(200));
        cancel.store(true, Ordering::Relaxed);
        h.join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn chunk_size_scales_with_rate() {
        let l = RateLimiter::new(Some(100 * 1024));
        assert_eq!(l.chunk_size(65536), 10 * 1024);
        l.set_rate(Some(10));
        assert_eq!(l.chunk_size(65536), MIN_CHUNK);
        l.set_rate(Some(1 << 30));
        assert_eq!(l.chunk_size(65536), 65536);
    }
}
