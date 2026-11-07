//! `Pending`, the result of one click's work on a worker: the dialog that
//! asked for it starts it, and takes the result on a later frame with
//! `try_recv` (AGENTS.md section 7), so the frame never waits on the disk.

use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::thread;

use eframe::egui;

/// Work started on a worker, until its result is taken.
pub struct Pending<T>(Option<Receiver<T>>);

impl<T> Default for Pending<T> {
    fn default() -> Self {
        Pending(None)
    }
}

impl<T: Send + 'static> Pending<T> {
    /// Run `work` on a worker, waking the window when it is done. Work
    /// started earlier and not taken yet is dropped.
    pub fn start(&mut self, ctx: &egui::Context, work: impl FnOnce() -> T + Send + 'static) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            if tx.send(work()).is_ok() {
                ctx.request_repaint();
            }
        });
        self.0 = Some(rx);
    }

    /// Whether work is running whose result is not taken yet.
    pub fn busy(&self) -> bool {
        self.0.is_some()
    }

    /// The result, once the work is done; then the work is forgotten.
    pub fn take(&mut self) -> Option<T> {
        match self.0.as_ref()?.try_recv() {
            Ok(result) => {
                self.0 = None;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.0 = None;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn the_result_is_taken_once_when_it_is_ready() {
        let ctx = egui::Context::default();
        let mut p = Pending::default();
        assert!(!p.busy());
        p.start(&ctx, || 7);
        assert!(p.busy());
        let until = Instant::now() + Duration::from_secs(5);
        let got = loop {
            if let Some(v) = p.take() {
                break v;
            }
            assert!(Instant::now() < until, "never done");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(got, 7);
        assert!(!p.busy());
        assert_eq!(p.take(), None);
    }

    #[test]
    fn a_new_start_replaces_the_work_before() {
        let ctx = egui::Context::default();
        let mut p = Pending::default();
        p.start(&ctx, || {
            std::thread::sleep(Duration::from_millis(200));
            1
        });
        p.start(&ctx, || 2);
        let until = Instant::now() + Duration::from_secs(5);
        let got = loop {
            if let Some(v) = p.take() {
                break v;
            }
            assert!(Instant::now() < until, "never done");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(got, 2);
    }
}
