use std::collections::BTreeMap;
use std::path::Path;

use udm_domain::address::{Address, Link};
use udm_domain::bytes::ByteCount;
use udm_domain::checksum::Checksum;
use udm_domain::download::{Download, Status};
use udm_domain::filename::FileName;
use udm_domain::id::DownloadId;
use udm_domain::ports::FileHashes;
use udm_domain::settings::Duplicate;

use super::{RemoteError, Session};
use crate::daemon::{Command, DaemonClient, Event};
use crate::downloads::VerifyChecksum;

/// A file to download, as a program asks for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetRequest {
    pub url: Address,
    /// The folder to save in, a full path; missing goes by the settings
    /// and the file's category.
    pub folder: Option<String>,
    /// The name to save it as; missing takes the server's.
    pub filename: Option<FileName>,
    /// Headers to send with the request, as name and value.
    pub headers: Vec<(String, String)>,
    /// How many connections to open; missing takes the setting's.
    pub connections: Option<usize>,
    /// A checksum the file must match once it is saved.
    pub checksum: Option<Checksum>,
    /// Follow the download until it ends (`true`), or return once the
    /// daemon has taken it.
    pub follow: bool,
}

impl GetRequest {
    /// Download `url` with the settings' folder, name and connections,
    /// following it to the end.
    pub fn new(url: Address) -> Self {
        GetRequest {
            url,
            folder: None,
            filename: None,
            headers: Vec::new(),
            connections: None,
            checksum: None,
            follow: true,
        }
    }

    /// The download the daemon is asked for. It starts at once with no
    /// save-as dialog, whatever the settings say, and an address already in
    /// the list is downloaded again under a new name: a script asked for
    /// this file now.
    fn download(&self) -> Download {
        Download {
            url: Link::from(self.url.clone()),
            folder: self.folder.clone(),
            filename: self.filename.as_ref().map(|f| f.as_str().to_string()),
            headers: self.headers.iter().cloned().collect::<BTreeMap<_, _>>(),
            connections: self.connections,
            checksum: self.checksum.clone(),
            start: Some(true),
            ask: Some(false),
            duplicate: Some(Duplicate::NewName),
            ..Download::default()
        }
    }
}

/// How a download a program asked for ended, or stands when it was not
/// followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Got {
    /// The daemon took it; it was not followed.
    Added(DownloadId),
    /// Saved at `path`, matching its checksum if it had one.
    Saved { id: DownloadId, path: String, note: Option<String> },
    /// It failed, and why.
    Failed { id: DownloadId, error: String },
    /// Saved at `path`, but the file is not the one its checksum names.
    Mismatch { id: DownloadId, path: String, expected: String, actual: String },
    /// Saved at `path`, but its checksum could not be checked, and why.
    Unchecked { id: DownloadId, path: String, why: String },
    /// Stopped before it finished (paused, or the save-as dialog canceled);
    /// its partial file is kept.
    Stopped(DownloadId),
    /// Put in a queue to start later, from the save-as dialog.
    Waiting(DownloadId),
    /// Taken out of the list before it finished.
    Removed(DownloadId),
}

/// What a followed download is doing, for a program to show.
pub trait Watch {
    /// The daemon took the download as `id`.
    fn added(&mut self, id: DownloadId);
    /// It began writing `path`, of `total` bytes if the server said, over
    /// `connections` connections.
    fn started(&mut self, path: &str, total: Option<ByteCount>, connections: usize);
    /// `done` of `total` bytes are in.
    fn progress(&mut self, done: ByteCount, total: Option<ByteCount>);
    /// It waits for an answer in the window's save-as dialog (its folder
    /// cannot be written, or its name is taken and the setting says to
    /// ask), about `path`.
    fn needs_answer(&mut self, path: Option<&str>);
}

/// Hand a file to the daemon to download now, and follow it to the end:
/// `udm-cli get`. The download goes on in the daemon if the program
/// stops following it.
pub struct GetFile<'a, 's, C: DaemonClient + ?Sized, H: ?Sized> {
    session: &'a Session<'s, C>,
    hashes: &'a H,
}

impl<'a, 's, C: DaemonClient + ?Sized, H: FileHashes + ?Sized> GetFile<'a, 's, C, H> {
    pub fn new(session: &'a Session<'s, C>, hashes: &'a H) -> Self {
        GetFile { session, hashes }
    }

    pub fn run(&self, request: &GetRequest, watch: &mut impl Watch) -> Result<Got, RemoteError> {
        self.session.send(Command::Add(request.download()))?;
        // The daemon announces the new download, and may start it, before it
        // answers; those events are kept for once the id is known.
        let mut early = Vec::new();
        let id = loop {
            match self.session.answer()? {
                Event::Ack { id, .. } => break id,
                Event::Rejected { error, .. } | Event::Error { error } => return Err(RemoteError::Refused(error)),
                event => early.push(event),
            }
        };
        if !request.follow {
            return Ok(Got::Added(id));
        }
        watch.added(id);
        let mut events = early.into_iter();
        let got = loop {
            let event = match events.next() {
                Some(event) => event,
                None => self.session.next()?,
            };
            if let Some(got) = follow(id, event, watch) {
                break got;
            }
        };
        Ok(match (got, &request.checksum) {
            (Got::Saved { id, path, .. }, Some(checksum)) => self.check(id, path, checksum),
            (got, _) => got,
        })
    }

    /// Compare the saved file with `checksum`. The daemon checks it too and
    /// fails the download on a mismatch, but tells nobody when it matches,
    /// so the program reads the file itself.
    fn check(&self, id: DownloadId, path: String, checksum: &Checksum) -> Got {
        match VerifyChecksum::new(self.hashes).run(Path::new(&path), checksum) {
            Ok(v) if v.matches => Got::Saved { id, path, note: None },
            Ok(v) => Got::Mismatch { id, path, expected: checksum.hex(), actual: v.actual },
            Err(e) => Got::Unchecked { id, path, why: e.to_string() },
        }
    }
}

/// What `event` means for download `id`: how it ended, if it did. What it
/// is doing goes to `watch`.
fn follow(id: DownloadId, event: Event, watch: &mut impl Watch) -> Option<Got> {
    match event {
        Event::Started { id: i, path, total, connections, .. } if i == id => watch.started(&path, total, connections),
        Event::Progress { id: i, done, total, .. } if i == id => watch.progress(done, total),
        Event::Done { id: i, path, note, .. } if i == id => return Some(Got::Saved { id, path, note }),
        Event::Failed { id: i, error, .. } if i == id => return Some(Got::Failed { id, error }),
        Event::Canceled { id: i, .. } if i == id => return Some(Got::Stopped(id)),
        Event::Removed { id: i } if i == id => return Some(Got::Removed(id)),
        Event::NameTaken { id: i, path, .. } if i == id => watch.needs_answer(Some(&path)),
        Event::Updated { item } if item.id == id => match item.status {
            Status::Confirming => watch.needs_answer(item.path.as_deref()),
            Status::Waiting => return Some(Got::Waiting(id)),
            _ => {}
        },
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{Failure, Lost};
    use crate::memory::{row, FakeDaemon, Hashes};
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(5);
    const URL: &str = "https://dl.example/a.iso";

    /// What a `Watch` saw, in order.
    #[derive(Default)]
    struct Seen(Vec<String>);

    impl Watch for Seen {
        fn added(&mut self, id: DownloadId) {
            self.0.push(format!("added {id}"));
        }
        fn started(&mut self, path: &str, total: Option<ByteCount>, connections: usize) {
            self.0.push(format!("started {path} {:?} {connections}", total.map(ByteCount::get)));
        }
        fn progress(&mut self, done: ByteCount, _total: Option<ByteCount>) {
            self.0.push(format!("progress {done}"));
        }
        fn needs_answer(&mut self, path: Option<&str>) {
            self.0.push(format!("asks {path:?}"));
        }
    }

    fn id() -> DownloadId {
        DownloadId::new(7)
    }

    /// A daemon that runs an added download through `after` the ack.
    fn daemon(before: Vec<Event>, after: Vec<Event>) -> FakeDaemon {
        FakeDaemon::new(move |c| match c {
            Command::Add(_) => {
                let mut events = before.clone();
                events.push(Event::Ack { id: id(), url: URL.into(), status: "accepted".into() });
                events.extend(after.clone());
                events
            }
            _ => Vec::new(),
        })
    }

    fn started() -> Event {
        Event::Started {
            id: id(),
            url: URL.into(),
            path: "/dl/a.iso".into(),
            total: Some(ByteCount::new(4)),
            resumable: true,
            connections: 2,
            partial: None,
            validator: None,
            country: None,
        }
    }

    fn progress(done: u64) -> Event {
        Event::Progress {
            id: id(),
            url: URL.into(),
            done: ByteCount::new(done),
            total: Some(ByteCount::new(4)),
            parts: None,
            segments: None,
        }
    }

    fn done() -> Event {
        Event::Done { id: id(), url: URL.into(), path: "/dl/a.iso".into(), note: None }
    }

    fn request() -> GetRequest {
        GetRequest::new(Address::parse(URL).unwrap())
    }

    fn get(daemon: &FakeDaemon, request: &GetRequest, hashes: &Hashes) -> (Result<Got, RemoteError>, Vec<String>) {
        let session = Session::open(daemon, WAIT).unwrap();
        let mut seen = Seen::default();
        let got = GetFile::new(&session, hashes).run(request, &mut seen);
        (got, seen.0)
    }

    #[test]
    fn a_download_is_followed_to_its_file() {
        let other = Event::Progress {
            id: DownloadId::new(1),
            url: "https://x/b".into(),
            done: ByteCount::new(9),
            total: None,
            parts: None,
            segments: None,
        };
        let daemon = daemon(vec![], vec![started(), other, progress(2), progress(4), done()]);
        let (got, seen) = get(&daemon, &request(), &Hashes::default());
        assert_eq!(got, Ok(Got::Saved { id: id(), path: "/dl/a.iso".into(), note: None }));
        // Another download's progress is not this one's.
        assert_eq!(seen, ["added 7", "started /dl/a.iso Some(4) 2", "progress 2", "progress 4"]);
    }

    #[test]
    fn it_asks_for_the_file_now_as_the_program_said() {
        let daemon = daemon(vec![], vec![done()]);
        let mut request = request();
        request.folder = Some("/dl".into());
        request.filename = Some(FileName::parse("b.iso").unwrap());
        request.headers = vec![("X-Token".into(), "1".into())];
        request.connections = Some(3);
        let _ = get(&daemon, &request, &Hashes::default());
        let Some(Command::Add(d)) = daemon.sent().into_iter().next() else {
            panic!("nothing was added");
        };
        assert_eq!((d.folder.as_deref(), d.filename.as_deref()), (Some("/dl"), Some("b.iso")));
        assert_eq!(d.headers.get("X-Token").map(String::as_str), Some("1"));
        assert_eq!(d.connections, Some(3));
        // No dialog, no queue, and a new name for an address already listed.
        assert_eq!((d.start, d.ask, d.duplicate), (Some(true), Some(false), Some(Duplicate::NewName)));
    }

    #[test]
    fn what_comes_before_the_answer_still_counts() {
        // A small file can be done before the daemon answers the add.
        let early = vec![Event::Added { id: id(), url: URL.into() }, started(), progress(4), done()];
        let daemon = daemon(early, vec![]);
        let (got, seen) = get(&daemon, &request(), &Hashes::default());
        assert!(matches!(got, Ok(Got::Saved { .. })));
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn without_following_it_returns_the_new_id() {
        let daemon = daemon(vec![], vec![started()]);
        let request = GetRequest { follow: false, ..request() };
        let (got, seen) = get(&daemon, &request, &Hashes::default());
        assert_eq!(got, Ok(Got::Added(id())));
        assert!(seen.is_empty());
    }

    #[test]
    fn a_refused_download_says_why() {
        let daemon = FakeDaemon::new(|c| match c {
            Command::Add(d) => vec![Event::Rejected { url: d.url.clone(), error: Failure::Other("no".into()) }],
            _ => Vec::new(),
        });
        let (got, _) = get(&daemon, &request(), &Hashes::default());
        assert_eq!(got, Err(RemoteError::Refused(Failure::Other("no".into()))));
    }

    #[test]
    fn each_end_is_told_apart() {
        let ends = [
            (Event::Failed { id: id(), url: URL.into(), error: "HTTP 404".into() }, "Failed"),
            (Event::Canceled { id: id(), url: URL.into() }, "Stopped"),
            (Event::Removed { id: id() }, "Removed"),
            (Event::Updated { item: row(7, Status::Waiting) }, "Waiting"),
        ];
        for (end, name) in ends {
            let (got, _) = get(&daemon(vec![], vec![end]), &request(), &Hashes::default());
            assert!(format!("{got:?}").contains(name), "{got:?} is not {name}");
        }
    }

    #[test]
    fn a_dialog_it_waits_for_is_shown_and_followed_through() {
        let mut confirming = row(7, Status::Confirming);
        confirming.path = Some("/root/a.iso".into());
        let daemon = daemon(vec![], vec![Event::Updated { item: confirming }, row_queued(), done()]);
        let (got, seen) = get(&daemon, &request(), &Hashes::default());
        assert!(matches!(got, Ok(Got::Saved { .. })));
        assert_eq!(seen, ["added 7", r#"asks Some("/root/a.iso")"#]);
    }

    fn row_queued() -> Event {
        Event::Updated { item: row(7, Status::Queued) }
    }

    #[test]
    fn a_saved_file_is_checked_against_its_checksum() {
        let mut request = request();
        request.checksum = Some(Checksum::parse("crc32:d87f7e0c").unwrap());
        let daemon = daemon(vec![], vec![done()]);
        let matching = Hashes::with("/dl/a.iso", vec![0xd8, 0x7f, 0x7e, 0x0c]);
        assert!(matches!(get(&daemon, &request, &matching).0, Ok(Got::Saved { .. })));

        let daemon = super::tests::daemon(vec![], vec![done()]);
        let other = Hashes::with("/dl/a.iso", vec![1, 2, 3, 4]);
        let (got, _) = get(&daemon, &request, &other);
        assert_eq!(
            got,
            Ok(Got::Mismatch {
                id: id(),
                path: "/dl/a.iso".into(),
                expected: "d87f7e0c".into(),
                actual: "01020304".into()
            })
        );

        let daemon = super::tests::daemon(vec![], vec![done()]);
        assert!(matches!(get(&daemon, &request, &Hashes::default()).0, Ok(Got::Unchecked { .. })));
    }

    #[test]
    fn a_daemon_that_goes_ends_the_wait() {
        let daemon = daemon(vec![], vec![started(), Event::Lost(Lost::Closed)]);
        let (got, _) = get(&daemon, &request(), &Hashes::default());
        assert!(matches!(got, Err(RemoteError::Lost(_))));
    }
}
