use std::path::PathBuf;

use udm_domain::bytes::ByteCount;
use udm_domain::download::{AfterProbe, Download};
use udm_domain::filename::FileName;
use udm_domain::id::DownloadId;
use udm_domain::ports::{
    Answers, ControlError, DownloadControl, DownloadFiles, DownloadList, EarlyStarts, SettingsStore,
};

/// What the server said about a download the dialog is to ask about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probed {
    /// The file name it proposes.
    pub filename: FileName,
    /// Where the file would go: the folder for its category, then the name.
    pub path: PathBuf,
    pub total: Option<ByteCount>,
    /// Whether it sends the file in ranges, so a download can resume.
    pub resumable: bool,
    /// For a torrent: what its metadata says, so the dialog can list its
    /// files.
    pub torrent: Option<udm_domain::torrent::TorrentInfo>,
}

/// What [`HoldForAnswer`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Held {
    /// It was canceled while the server was asked.
    Canceled,
    /// Its type starts without asking, so it went to the engine.
    Started(Result<(), ControlError>),
    /// It waits for the dialog's answer, started early or not.
    Asked { early: bool },
}

/// Once the server has named a new download's file: hold it for the
/// save-as dialog with the file proposed (starting it early when the
/// settings say so), or start it at once when its type starts without
/// asking.
pub struct HoldForAnswer<'a, D, X, F, S, E, A> {
    downloads: &'a D,
    control: &'a X,
    files: &'a F,
    settings: &'a S,
    early: &'a E,
    answers: &'a A,
}

impl<'a, D, X, F, S, E, A> HoldForAnswer<'a, D, X, F, S, E, A>
where
    D: DownloadList,
    X: DownloadControl,
    F: DownloadFiles,
    S: SettingsStore,
    E: EarlyStarts,
    A: Answers,
{
    pub fn new(downloads: &'a D, control: &'a X, files: &'a F, settings: &'a S, early: &'a E, answers: &'a A) -> Self {
        HoldForAnswer { downloads, control, files, settings, early, answers }
    }

    /// Hold download `id`, made from `download` and `probed`. `duplicate_of`
    /// is an older download with its address, for the dialog to ask about.
    pub fn run(&self, id: DownloadId, download: &Download, probed: Probed, duplicate_of: Option<DownloadId>) -> Held {
        // The other one may have been removed while this was probed.
        let duplicate_of = duplicate_of.filter(|o| self.downloads.status(*o).is_some());
        let settings = self.settings.settings();
        let dir = probed.path.parent().unwrap_or(&probed.path);
        let writable = download.elevate || self.files.can_write(dir);
        let size_known = probed.total.is_some();
        let step = download.after_probe(
            &settings,
            &probed.filename,
            probed.resumable,
            size_known,
            writable,
            duplicate_of.is_some(),
        );
        let start_now = step == AfterProbe::StartNow;
        let held = self.downloads.update(id, |e| {
            let held = e.hold_for_answer(start_now, &probed.path, probed.total, probed.resumable, duplicate_of);
            if let (true, Some(info)) = (held, &probed.torrent) {
                e.set_torrent_info(info);
                // The files the download was added with are the dialog's
                // ticks to start from.
                let picked = download.torrent_files.as_ref().and_then(|s| s.within(info.files.len()));
                if let (Some(picked), Some(t)) = (picked, e.torrent.as_mut()) {
                    t.selection = Some(picked);
                    e.total = Some(t.selected_size());
                }
            }
            held
        });
        if held != Some(true) {
            return Held::Canceled;
        }
        match step {
            AfterProbe::StartNow => Held::Started(self.control.dispatch(id)),
            AfterProbe::Ask { start_early } => {
                if start_early {
                    self.early.start(id, download.clone(), &probed.filename);
                }
                self.answers.await_answer(id);
                Held::Asked { early: start_early }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{row, Control, Downloads, Early, Files, Prefs, Waiting};
    use udm_domain::download::Status;
    use udm_domain::settings::Settings;

    fn probed(name: &str) -> Probed {
        Probed {
            filename: FileName::parse(name).unwrap(),
            path: PathBuf::from("/dl").join(name),
            total: Some(ByteCount::new(100)),
            resumable: true,
            torrent: None,
        }
    }

    struct World {
        downloads: Downloads,
        control: Control,
        files: Files,
        prefs: Prefs,
        early: Early,
        waiting: Waiting,
    }

    impl World {
        fn new(settings: Settings) -> Self {
            World {
                downloads: Downloads::rows([row(1, Status::Queued), row(2, Status::Done)]),
                control: Control::default(),
                files: Files::default(),
                prefs: Prefs::with(settings),
                early: Early::default(),
                waiting: Waiting::default(),
            }
        }

        fn hold(&self, name: &str, duplicate_of: Option<u64>) -> Held {
            let w = self;
            HoldForAnswer::new(&w.downloads, &w.control, &w.files, &w.prefs, &w.early, &w.waiting).run(
                DownloadId::new(1),
                &Download::default(),
                probed(name),
                duplicate_of.map(DownloadId::new),
            )
        }
    }

    #[test]
    fn a_type_that_starts_without_asking_starts() {
        let w = World::new(Settings { auto_start_types: vec!["zip".into()], ..Settings::default() });
        assert_eq!(w.hold("a.zip", None), Held::Started(Ok(())));
        assert_eq!(w.control.dispatched(), [1]);
        assert_eq!(w.downloads.entry(DownloadId::new(1)).unwrap().status, Status::Waiting);
        assert!(w.waiting.awaited().is_empty());
    }

    #[test]
    fn otherwise_it_waits_for_the_dialog_and_may_start_early() {
        let w = World::new(Settings { start_while_asking: true, ..Settings::default() });
        assert_eq!(w.hold("a.iso", Some(2)), Held::Asked { early: false }, "a duplicate does not start early");
        let e = w.downloads.entry(DownloadId::new(1)).unwrap();
        assert_eq!(
            (e.status, e.duplicate_of, e.total),
            (Status::Confirming, Some(DownloadId::new(2)), Some(ByteCount::new(100)))
        );
        assert_eq!(w.waiting.awaited(), [1]);
        w.downloads.set(1, Status::Queued);
        assert_eq!(w.hold("a.iso", Some(9)), Held::Asked { early: true }, "a duplicate of nothing is no duplicate");
        assert_eq!(w.early.began(), [(1, "a.iso".to_string())]);
    }

    #[test]
    fn a_folder_udm_may_not_write_to_asks_first() {
        let settings = Settings { auto_start_types: vec!["zip".into()], ..Settings::default() };
        let w = World { files: Files::read_only("/dl"), ..World::new(settings) };
        assert_eq!(w.hold("a.zip", None), Held::Asked { early: false });
        assert!(w.control.dispatched().is_empty());
    }

    #[test]
    fn a_torrent_added_with_some_files_offers_those_ticked() {
        use udm_domain::torrent::{FileSelection, InfoHash, TorrentFiles, TorrentInfo};
        let w = World::new(Settings::default());
        let raw = ["a", "b", "c"].iter().map(|n| (vec![n.to_string()], 10, false)).collect();
        let info = TorrentInfo {
            info_hash: InfoHash::from_bytes([3; 20]),
            name: "t".into(),
            files: TorrentFiles::parse(raw).unwrap(),
            piece_size: ByteCount::new(16_384),
            pieces: 1,
            private: false,
        };
        let download = Download { torrent_files: FileSelection::parse(&[0, 2], 3).ok(), ..Download::default() };
        let probed = Probed { torrent: Some(info), ..probed("t") };
        let held = HoldForAnswer::new(&w.downloads, &w.control, &w.files, &w.prefs, &w.early, &w.waiting).run(
            DownloadId::new(1),
            &download,
            probed,
            None,
        );
        assert_eq!(held, Held::Asked { early: false });
        let e = w.downloads.entry(DownloadId::new(1)).unwrap();
        assert_eq!(e.torrent.and_then(|t| t.selection), FileSelection::parse(&[0, 2], 3).ok());
        assert_eq!(e.total, Some(ByteCount::new(20)));
    }

    #[test]
    fn a_download_canceled_while_probed_stays_canceled() {
        let w = World::new(Settings::default());
        w.downloads.set(1, Status::Canceled);
        assert_eq!(w.hold("a.iso", None), Held::Canceled);
        assert!(w.waiting.awaited().is_empty());
    }
}
