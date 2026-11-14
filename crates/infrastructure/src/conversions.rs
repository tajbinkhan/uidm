//! `Conversions`, the adapter for `Converter`: converting finished downloads
//! with ffmpeg (`media::convert`), each on a thread of its own, as a row of
//! its own in the list with ffmpeg's progress.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use udm_domain::bytes::ByteCount;
use udm_domain::id::DownloadId;
use udm_domain::sync::LockExt;

use udm_domain::media::Conversion;
use udm_domain::ports::Converter;

use crate::engine::Engine;
use crate::front::Hook;
use crate::live_settings::LiveSettings;
use crate::media::convert::FfmpegError;
use crate::protocol::Outgoing;
use crate::settings;
use crate::store::DownloadStore;

/// How often a conversion reports its progress.
const PROGRESS_EVERY: Duration = Duration::from_millis(500);

type Running = Arc<Mutex<HashMap<DownloadId, Arc<AtomicBool>>>>;

/// The conversions running, by their row's id, with a flag to stop each.
pub struct Conversions {
    engine: Arc<Engine>,
    store: Arc<DownloadStore>,
    settings: Arc<LiveSettings>,
    running: Running,
    /// Removes a converted download's original with its file, through the
    /// use case.
    remove_original: Hook<DownloadId>,
}

/// Only single inserts and removals change the map, so a poisoned lock
/// still holds a usable one.
fn lock(running: &Running) -> MutexGuard<'_, HashMap<DownloadId, Arc<AtomicBool>>> {
    running.lock_or_recover()
}

impl Conversions {
    pub fn new(
        engine: Arc<Engine>,
        store: Arc<DownloadStore>,
        settings: Arc<LiveSettings>,
        remove_original: Hook<DownloadId>,
    ) -> Self {
        Conversions { engine, store, settings, running: Arc::default(), remove_original }
    }

    /// Whether a conversion is running.
    pub fn any_running(&self) -> bool {
        !lock(&self.running).is_empty()
    }
}

impl Converter for Conversions {
    fn any_running(&self) -> bool {
        Conversions::any_running(self)
    }

    fn available(&self) -> bool {
        settings::ffmpeg(&self.settings.lock()).is_some()
    }

    fn stop(&self, id: DownloadId) -> bool {
        match lock(&self.running).get(&id) {
            Some(stop) => {
                stop.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    fn start(&self, c: Conversion) {
        let Some(ffmpeg) = settings::ffmpeg(&self.settings.lock()) else {
            return;
        };
        let size = ByteCount::new(std::fs::metadata(&c.input).map(|m| m.len()).unwrap_or(0));
        let new_id = self.engine.reserve_id();
        let store = Arc::clone(&self.store);
        store.uncount(new_id);
        let url = c.url.clone();
        store.publish(&Outgoing::Added { id: new_id, url: url.clone() });
        store.publish(&Outgoing::Started {
            id: new_id,
            url: url.clone(),
            path: c.output.display().to_string(),
            total: Some(size),
            resumable: false,
            connections: 1,
            partial: None,
            validator: None,
            country: None,
        });
        let stop = Arc::new(AtomicBool::new(false));
        lock(&self.running).insert(new_id, Arc::clone(&stop));
        let (running, remove_original) = (Arc::clone(&self.running), Arc::clone(&self.remove_original));
        thread::spawn(move || {
            let mut last = Instant::now() - PROGRESS_EVERY;
            let result = crate::media::convert::convert(&ffmpeg, &c.input, &c.output, c.target, &stop, |f| {
                if last.elapsed() >= PROGRESS_EVERY {
                    last = Instant::now();
                    store.publish(&Outgoing::Progress {
                        id: new_id,
                        url: url.clone(),
                        done: ByteCount::new((f * size.get() as f64) as u64),
                        total: Some(size),
                        parts: None,
                        segments: None,
                    });
                }
            });
            lock(&running).remove(&new_id);
            let from = c.input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match result {
                Ok(()) => {
                    if c.delete_original {
                        remove_original(c.from);
                    }
                    store.publish(&Outgoing::Done {
                        id: new_id,
                        url,
                        path: c.output.display().to_string(),
                        note: Some(format!("Converted from {from}")),
                    });
                }
                Err(FfmpegError::Canceled) => store.publish(&Outgoing::Canceled { id: new_id, url }),
                Err(e) => store.publish(&Outgoing::Failed { id: new_id, url, error: e.to_string() }),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Ended, StoreEvents, StoreFiles};
    use udm_domain::settings::Settings;

    struct Quiet;

    impl StoreEvents for Quiet {
        fn announce(&self, _: &Outgoing) {}
        fn ended(&self, _: Ended) {}
        fn wake(&self) {}
    }

    #[test]
    fn without_ffmpeg_nothing_starts() {
        let dir = std::env::temp_dir().join(format!("udm_conversions_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // A folder is never ffmpeg, so this finds none whatever is on PATH.
        let missing = Settings { ffmpeg_path: dir.join("nope.exe").display().to_string(), ..Settings::default() };
        let store = Arc::new(DownloadStore::open(StoreFiles::default(), Arc::new(Quiet)).0);
        let engine = Arc::new(Engine::new(Arc::new(|_| {}), 1));
        let conversions =
            Conversions::new(engine, Arc::clone(&store), Arc::new(LiveSettings::new(missing, None)), Arc::new(|_| {}));
        assert!(!conversions.available());
        assert!(!conversions.stop(DownloadId::new(1)), "nothing runs");
        let before = store.table().entries().count();
        conversions.start(Conversion {
            from: DownloadId::new(1),
            url: "http://x/a.webm".into(),
            input: dir.join("a.webm"),
            output: dir.join("a.mp4"),
            target: udm_domain::media::Target::Mp4,
            delete_original: false,
        });
        assert_eq!(store.table().entries().count(), before, "no row without ffmpeg");
        assert!(!conversions.any_running());
        let _ = std::fs::remove_dir_all(dir);
    }
}
