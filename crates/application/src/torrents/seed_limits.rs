use udm_domain::download::Status;
use udm_domain::id::DownloadId;
use udm_domain::ports::{DownloadList, SettingsStore, Timestamps, Torrents};
use udm_domain::torrent::TorrentPhase;

/// Stop each seeding torrent that has seeded enough: its own limits, else
/// the settings', by share ratio or time (see `SeedLimits`). A torrent that
/// seeds with no download in the list left (it was removed meanwhile) stops
/// too. Runs with the scheduler. Returns the downloads that stopped.
pub struct ApplySeedLimits<'a, D, T: ?Sized, S, C> {
    downloads: &'a D,
    torrents: &'a T,
    settings: &'a S,
    clock: &'a C,
}

impl<'a, D: DownloadList, T: Torrents + ?Sized, S: SettingsStore, C: Timestamps> ApplySeedLimits<'a, D, T, S, C> {
    pub fn new(downloads: &'a D, torrents: &'a T, settings: &'a S, clock: &'a C) -> Self {
        ApplySeedLimits { downloads, torrents, settings, clock }
    }

    pub fn run(&self) -> Vec<DownloadId> {
        let seeding = self.torrents.seeding();
        if seeding.is_empty() {
            return Vec::new();
        }
        let limits = self.settings.settings().torrents.seed_limits;
        let now = self.clock.unix_now();
        let mut stopped = Vec::new();
        for seed in seeding {
            let hash = seed.info_hash;
            let row = self
                .downloads
                .find_last(|e| e.status == Status::Done && e.torrent.as_ref().is_some_and(|t| t.info_hash == hash));
            let Some(entry) = row else {
                self.torrents.stop_seeding(hash);
                continue;
            };
            let Some(mut torrent) = entry.torrent else { continue };
            torrent.uploaded = torrent.uploaded.max(seed.uploaded);
            torrent.seeding_since.get_or_insert(now);
            if !torrent.seeded_enough(&torrent.limits(&limits), now) {
                continue;
            }
            self.torrents.stop_seeding(hash);
            self.downloads.update(entry.id, |e| match e.torrent.as_mut() {
                Some(t) => {
                    t.phase = TorrentPhase::Complete;
                    t.uploaded = torrent.uploaded;
                    true
                }
                None => false,
            });
            tracing::info!(download_id = entry.id.get(), uploaded = torrent.uploaded.get(), "seeded enough");
            stopped.push(entry.id);
        }
        stopped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{row, Downloads, Prefs};
    use crate::torrents::testing::{hash, info, magnet, Client};
    use udm_domain::bytes::ByteCount;
    use udm_domain::download::DownloadEntry;
    use udm_domain::ports::Seeding;
    use udm_domain::settings::Settings;
    use udm_domain::torrent::{InfoHash, Ratio, SeedLimits};

    struct At(u64);

    impl Timestamps for At {
        fn unix_now(&self) -> u64 {
            self.0
        }
    }

    fn seeding_row(id: u64, since: u64, uploaded: u64) -> DownloadEntry {
        let mut e = row(id, Status::Done);
        e.url = magnet().into();
        e.set_torrent_info(&info(&["a.flac", "b.flac"]));
        let t = e.torrent.as_mut().unwrap();
        t.phase = TorrentPhase::Seeding;
        t.seeding_since = Some(since);
        t.uploaded = ByteCount::new(uploaded);
        e
    }

    fn seeds(client: &Client, list: &[(InfoHash, u64)]) {
        *client.seeding.lock().unwrap() =
            list.iter().map(|(h, up)| Seeding { info_hash: *h, uploaded: ByteCount::new(*up) }).collect();
    }

    #[test]
    fn seeding_stops_at_the_settings_limits() {
        let downloads = Downloads::rows([seeding_row(1, 1_000, 0)]);
        let client = Client::default();
        let settings = Settings::default();
        let prefs = Prefs::with(settings);
        seeds(&client, &[(hash(), 100)]);
        let apply = |now| ApplySeedLimits::new(&downloads, &client, &prefs, &At(now)).run();
        assert!(apply(1_000 + 59 * 60).is_empty(), "ratio 0.5, under the hour");
        assert_eq!(apply(1_000 + 60 * 60), [DownloadId::new(1)], "an hour");
        assert_eq!(client.stopped.lock().unwrap().as_slice(), [hash()]);
        let t = downloads.entry(DownloadId::new(1)).unwrap().torrent.unwrap();
        assert_eq!((t.phase, t.uploaded), (TorrentPhase::Complete, ByteCount::new(100)));
    }

    #[test]
    fn a_torrents_own_limits_win_and_the_ratio_counts_uploads() {
        let mut e = seeding_row(1, 1_000, 0);
        e.torrent.as_mut().unwrap().seed_limits =
            Some(SeedLimits { ratio: Some(Ratio::new(2.0).unwrap()), minutes: None });
        let downloads = Downloads::rows([e]);
        let client = Client::default();
        let prefs = Prefs::with(Settings::default());
        let apply = || ApplySeedLimits::new(&downloads, &client, &prefs, &At(1_000_000)).run();
        seeds(&client, &[(hash(), 399)]);
        assert!(apply().is_empty(), "days, but under ratio 2 of 200 bytes");
        seeds(&client, &[(hash(), 400)]);
        assert_eq!(apply(), [DownloadId::new(1)]);
    }

    #[test]
    fn a_torrent_no_download_names_stops_seeding() {
        let downloads = Downloads::rows([row(1, Status::Done)]);
        let client = Client::default();
        seeds(&client, &[(hash(), 0)]);
        let prefs = Prefs::with(Settings::default());
        assert!(ApplySeedLimits::new(&downloads, &client, &prefs, &At(0)).run().is_empty());
        assert_eq!(client.stopped.lock().unwrap().as_slice(), [hash()]);
    }
}
