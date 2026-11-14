//! `udm-cli`, the command-line tool: its arguments, what it prints and
//! its exit codes. What each command does is a use case in
//! `udm_application::remote`; this module turns the arguments into
//! their requests (`parse`) and their results into text (`Terminal` for a
//! download's progress, `table` and `json` for the list, `Report` for the
//! rest). It writes to standard error only through `Terminal`; the binary
//! prints each `Report`.
//!
//! ```text
//! udm-cli get URL|MAGNET|FILE.torrent [-d DIR] [-n NAME] [-c N] [-H "Name: value"]... [--checksum SUM] [--no-wait] [-q]
//! udm-cli list [--json]
//! udm-cli pause ID | --all
//! udm-cli resume ID | --all
//! udm-cli remove ID [--delete-files]
//! udm-cli queue start|stop [QUEUE]
//! ```

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use udm_application::remote::{GetRequest, Got, Listing, RemoteError, Resumed, Watch};
use udm_domain::address::Address;
use udm_domain::bytes::ByteCount;
use udm_domain::checksum::{Checksum, ChecksumError};
use udm_domain::download::{DownloadEntry, Status};
use udm_domain::filename::FileName;
use udm_domain::id::{DeviceId, DownloadId};
use udm_domain::pairing::{DeviceInfo, RemoteState};
use udm_domain::settings::CONNECTIONS_RANGE;

/// The exit codes, for scripts to test.
pub mod exit {
    /// Done: the file is saved, or the daemon did what was asked.
    pub const OK: i32 = 0;
    /// The download failed or did not match its checksum, or the daemon
    /// refused the request.
    pub const FAILED: i32 = 1;
    /// The command line is wrong.
    pub const USAGE: i32 = 2;
    /// The daemon could not be reached or started, or went away.
    pub const UNREACHABLE: i32 = 3;
    /// The download stopped before it finished: paused, removed, or put in
    /// a queue from the save-as dialog.
    pub const STOPPED: i32 = 4;
}

/// What `udm-cli --help` prints.
pub const USAGE: &str = "\
Usage: udm-cli <command> [options]

Commands:
  get URL            Download URL now and show its progress until it ends; URL may
                     also be a magnet link or a .torrent file
    -d, --folder DIR       save in DIR instead of the category's folder
    -n, --name NAME        save as NAME instead of the server's name
    -c, --connections N    open N connections (1 to 16) instead of the setting's
    -H, --header 'N: V'    send this header too (may be repeated)
        --checksum SUM     fail unless the file matches SUM (sha256:HEX, or a bare hex digest)
        --no-wait          print the download's id and return once UDM has it
    -q, --quiet            show no progress
  list [--json]      List the downloads and the queues
  pause ID|--all     Stop a download, keeping what it has, or every download
  resume ID|--all    Start a stopped download again, or everything paused
  remove ID [--delete-files]
                     Take a download out of the list, and its files with --delete-files
  queue start|stop [QUEUE]
                     Start or stop a queue, by name or id (the main queue by default)
  pair [--host HOST[:PORT]]
                     Show a QR code and a one-time code that pair a phone with the
                     remote-control page (remote control must be on); --host puts the
                     address the phone reaches in the link, as for a container
  devices            List the phones paired with the remote-control page
  unpair ID          Unpair a phone at once
  help, version

UDM starts in the background if it is not running. Exit codes: 0 done, 1 failed or
refused, 2 bad command line, 3 UDM could not be reached, 4 the download stopped
before it finished.";

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Get {
        request: GetRequest,
        quiet: bool,
    },
    List {
        json: bool,
    },
    Pause(Which),
    Resume(Which),
    Remove {
        id: DownloadId,
        delete_files: bool,
    },
    Queue {
        start: bool,
        name: Option<String>,
    },
    /// Show a pairing code; `host` replaces the link's address and port.
    Pair {
        host: Option<String>,
    },
    Devices,
    Unpair(DeviceId),
    Help,
    Version,
}

/// One download, or all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    One(DownloadId),
    All,
}

/// Why a command line cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UsageError {
    #[error("no command given")]
    NoCommand,
    #[error("{0} is not a command")]
    UnknownCommand(String),
    #[error("{0} is not an option of this command")]
    UnknownOption(String),
    /// The option, and what it needs after it.
    #[error("{0} needs {1}")]
    NeedsValue(String, &'static str),
    /// What is missing.
    #[error("missing {0}")]
    Missing(&'static str),
    #[error("did not expect {0}")]
    Extra(String),
    #[error("{0} is not an http, https, ftp or ftps address, a magnet link or a .torrent file")]
    NotDownloadAddress(String),
    /// A `.torrent` file that could not be read, and why.
    #[error("{0} cannot be read as a torrent: {1}")]
    BadTorrent(String, String),
    #[error("{0} is not a file name; give the folder with --folder")]
    NotFileName(String),
    #[error("{0} is not a folder UDM can save in")]
    BadFolder(String),
    #[error("{0} is not a number of connections from {low} to {high}", low = CONNECTIONS_RANGE.0, high = CONNECTIONS_RANGE.1)]
    BadConnections(String),
    #[error("{0} is not a header; write it as 'Name: value'")]
    BadHeader(String),
    #[error("{0} is not a checksum: {1}")]
    BadChecksum(String, #[source] ChecksumError),
    #[error("{0} is not a download id")]
    BadId(String),
    #[error("{0} is not a device number")]
    BadDevice(String),
    #[error("{0} is not a host name or address, with or without a port")]
    BadHost(String),
}

/// The command line's words after the command, as options and the rest.
struct Words<'a> {
    words: std::slice::Iter<'a, String>,
    /// The words that are not options, in order.
    plain: Vec<String>,
}

impl<'a> Words<'a> {
    /// The next option and its value if it was written `--name=value`, or
    /// `None` once every word is read; plain words are kept aside.
    fn next_option(&mut self) -> Option<(String, Option<String>)> {
        for word in self.words.by_ref() {
            if word == "-" || !word.starts_with('-') {
                self.plain.push(word.clone());
                continue;
            }
            return Some(match word.split_once('=').filter(|_| word.starts_with("--")) {
                Some((name, value)) => (name.to_string(), Some(value.to_string())),
                None => (word.clone(), None),
            });
        }
        None
    }

    /// The value of option `name`: written after `=`, or the next word.
    fn value(&mut self, name: &str, inline: Option<String>, what: &'static str) -> Result<String, UsageError> {
        inline.or_else(|| self.words.next().cloned()).ok_or_else(|| UsageError::NeedsValue(name.to_string(), what))
    }
}

/// Read `args` (without the program name). `cwd` resolves a relative
/// `--folder`.
pub fn parse<S: AsRef<str>>(args: &[S], cwd: &Path) -> Result<Invocation, UsageError> {
    let args: Vec<String> = args.iter().map(|a| a.as_ref().to_string()).collect();
    let Some((command, rest)) = args.split_first() else {
        return Err(UsageError::NoCommand);
    };
    let mut words = Words { words: rest.iter(), plain: Vec::new() };
    let invocation = match command.as_str() {
        "get" => return parse_get(words, cwd),
        "help" | "--help" | "-h" | "/?" => Invocation::Help,
        "version" | "--version" | "-V" => Invocation::Version,
        "list" => {
            let mut json = false;
            while let Some((name, _)) = words.next_option() {
                match name.as_str() {
                    "--json" => json = true,
                    _ => return Err(UsageError::UnknownOption(name)),
                }
            }
            if let Some(extra) = words.plain.into_iter().next() {
                return Err(UsageError::Extra(extra));
            }
            Invocation::List { json }
        }
        "pause" | "resume" => {
            let mut all = false;
            while let Some((name, _)) = words.next_option() {
                match name.as_str() {
                    "--all" | "-a" => all = true,
                    _ => return Err(UsageError::UnknownOption(name)),
                }
            }
            let which = match (all, one(&words.plain)?) {
                (true, None) => Which::All,
                (false, Some(id)) => Which::One(download_id(&id)?),
                (true, Some(id)) => return Err(UsageError::Extra(id)),
                (false, None) => return Err(UsageError::Missing("a download id or --all")),
            };
            if command == "pause" {
                Invocation::Pause(which)
            } else {
                Invocation::Resume(which)
            }
        }
        "remove" => {
            let mut delete_files = false;
            while let Some((name, _)) = words.next_option() {
                match name.as_str() {
                    "--delete-files" => delete_files = true,
                    _ => return Err(UsageError::UnknownOption(name)),
                }
            }
            let id = one(&words.plain)?.ok_or(UsageError::Missing("a download id"))?;
            Invocation::Remove { id: download_id(&id)?, delete_files }
        }
        "queue" => {
            if let Some((name, _)) = words.next_option() {
                return Err(UsageError::UnknownOption(name));
            }
            let mut plain = words.plain.into_iter();
            let start = match plain.next().as_deref() {
                Some("start") => true,
                Some("stop") => false,
                Some(other) => return Err(UsageError::UnknownCommand(format!("queue {other}"))),
                None => return Err(UsageError::Missing("start or stop")),
            };
            // A name may have spaces without quotes: `queue start Main queue`.
            let name = plain.collect::<Vec<_>>().join(" ");
            Invocation::Queue { start, name: (!name.is_empty()).then_some(name) }
        }
        "pair" => {
            let mut host = None;
            while let Some((name, inline)) = words.next_option() {
                match name.as_str() {
                    "--host" => {
                        let text = words.value(&name, inline, "a host name or address")?;
                        host = Some(page_host(&text).ok_or(UsageError::BadHost(text))?);
                    }
                    _ => return Err(UsageError::UnknownOption(name)),
                }
            }
            if let Some(extra) = words.plain.into_iter().next() {
                return Err(UsageError::Extra(extra));
            }
            Invocation::Pair { host }
        }
        "devices" => {
            if let Some((name, _)) = words.next_option() {
                return Err(UsageError::UnknownOption(name));
            }
            if let Some(extra) = words.plain.into_iter().next() {
                return Err(UsageError::Extra(extra));
            }
            Invocation::Devices
        }
        "unpair" => {
            if let Some((name, _)) = words.next_option() {
                return Err(UsageError::UnknownOption(name));
            }
            let id = one(&words.plain)?.ok_or(UsageError::Missing("a device number"))?;
            let n = id.trim().parse::<u64>().map_err(|_| UsageError::BadDevice(id.clone()))?;
            Invocation::Unpair(DeviceId::new(n))
        }
        other => return Err(UsageError::UnknownCommand(other.to_string())),
    };
    Ok(invocation)
}

/// `text` as the host (and port) of an `https://` address, if it is one.
fn page_host(text: &str) -> Option<String> {
    let text = text.trim();
    let url = url::Url::parse(&format!("https://{text}/")).ok()?;
    let host = url.host_str()?;
    (url.path() == "/" && url.username().is_empty() && url.query().is_none()).then(|| match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// `link` with its host and port replaced by `host` (a `HOST[:PORT]`);
/// without a port in `host`, the link's own port stays.
pub fn link_at(link: &str, host: &str) -> String {
    let (Ok(mut url), Ok(given)) = (url::Url::parse(link), url::Url::parse(&format!("https://{host}/"))) else {
        return link.to_string();
    };
    if url.set_host(given.host_str()).is_err() {
        return link.to_string();
    }
    if let Some(port) = given.port() {
        let _ = url.set_port(Some(port));
    }
    url.to_string()
}

/// `text` as a QR code in the terminal: two rows of modules per line of
/// half blocks, dark on light, with the quiet zone around it.
pub fn qr_text(text: &str) -> Option<String> {
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::L).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    const QUIET: usize = 2;
    let side = width + 2 * QUIET;
    let dark = |x: usize, y: usize| {
        x >= QUIET && y >= QUIET && x < QUIET + width && y < QUIET + width && {
            colors[(y - QUIET) * width + (x - QUIET)] == qrcode::Color::Dark
        }
    };
    let mut out = String::new();
    for y in (0..side).step_by(2) {
        for x in 0..side {
            // Light modules print as blocks, so the code reads on a dark
            // terminal as on paper.
            out.push(match (dark(x, y), dark(x, y + 1)) {
                (false, false) => '\u{2588}',
                (false, true) => '\u{2580}',
                (true, false) => '\u{2584}',
                (true, true) => ' ',
            });
        }
        out.push('\n');
    }
    Some(out)
}

/// The one plain word, if there is one.
fn one(plain: &[String]) -> Result<Option<String>, UsageError> {
    match plain {
        [] => Ok(None),
        [word] => Ok(Some(word.clone())),
        [_, extra, ..] => Err(UsageError::Extra(extra.clone())),
    }
}

fn download_id(text: &str) -> Result<DownloadId, UsageError> {
    text.trim().parse::<u64>().map(DownloadId::new).map_err(|_| UsageError::BadId(text.to_string()))
}

fn parse_get(mut words: Words<'_>, cwd: &Path) -> Result<Invocation, UsageError> {
    let mut request = None::<GetRequest>;
    let (mut folder, mut filename, mut connections, mut checksum) = (None, None, None, None);
    let (mut headers, mut follow, mut quiet) = (Vec::new(), true, false);
    while let Some((name, inline)) = words.next_option() {
        match name.as_str() {
            "-d" | "--folder" => folder = Some(folder_path(&words.value(&name, inline, "a folder")?, cwd)?),
            "-n" | "--name" => {
                let text = words.value(&name, inline, "a file name")?;
                filename = Some(FileName::parse(&text).map_err(|_| UsageError::NotFileName(text))?);
            }
            "-c" | "--connections" => {
                let text = words.value(&name, inline, "a number")?;
                let (low, high) = CONNECTIONS_RANGE;
                let n = text.trim().parse::<usize>().ok().filter(|n| (low..=high).contains(n));
                connections = Some(n.ok_or(UsageError::BadConnections(text))?);
            }
            "-H" | "--header" => headers.push(header(&words.value(&name, inline, "a header")?)?),
            "--checksum" => {
                let text = words.value(&name, inline, "a checksum")?;
                checksum = Some(Checksum::parse(&text).map_err(|e| UsageError::BadChecksum(text, e))?);
            }
            "--no-wait" => follow = false,
            "-q" | "--quiet" => quiet = true,
            _ => return Err(UsageError::UnknownOption(name)),
        }
    }
    for word in words.plain {
        if request.is_some() {
            return Err(UsageError::Extra(word));
        }
        request = Some(match Address::parse(word.trim()) {
            Ok(url) => GetRequest::new(url),
            Err(_) => torrent_file(&word, cwd)?,
        });
    }
    let mut request = request.ok_or(UsageError::Missing("the address to download"))?;
    request.folder = folder;
    request.filename = filename;
    request.connections = connections;
    request.checksum = checksum;
    request.headers = headers;
    request.follow = follow;
    Ok(Invocation::Get { request, quiet })
}

/// The `.torrent` file at `word` (under `cwd` when relative), to download
/// its torrent: read here, and sent to UDM, which keeps it.
fn torrent_file(word: &str, cwd: &Path) -> Result<GetRequest, UsageError> {
    let path = cwd.join(word.trim());
    let named = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("torrent"));
    if !named || !path.is_file() {
        return Err(UsageError::NotDownloadAddress(word.to_string()));
    }
    let bad = |why: String| UsageError::BadTorrent(word.to_string(), why);
    let size = std::fs::metadata(&path).map_err(|e| bad(e.to_string()))?.len();
    // Sent base64-encoded in one message, which has a size limit.
    if size > udm_domain::torrent::MAX_SENT_METAINFO_BYTES as u64 {
        let max = udm_domain::bytes::ByteCount::new(udm_domain::torrent::MAX_SENT_METAINFO_BYTES as u64).megabytes();
        return Err(bad(format!(
            "it is larger than {max}, the most a .torrent file sent to UDM may be; add it from its web address instead"
        )));
    }
    let bytes = std::fs::read(&path).map_err(|e| bad(e.to_string()))?;
    let meta = crate::torrent::metainfo::Metainfo::read(&bytes).map_err(|e| bad(e.to_string()))?;
    let url = Address::parse(&meta.magnet().to_uri()).map_err(|e| bad(e.to_string()))?;
    let mut request = GetRequest::new(url);
    request.metainfo = Some(bytes);
    Ok(request)
}

/// `text` as a full path, under `cwd` when it is relative.
fn folder_path(text: &str, cwd: &Path) -> Result<String, UsageError> {
    let bad = || UsageError::BadFolder(text.to_string());
    if text.trim().is_empty() {
        return Err(bad());
    }
    let path = cwd.join(text);
    if !path.is_absolute() {
        return Err(bad());
    }
    path.to_str().map(str::to_string).ok_or_else(bad)
}

/// `Name: value` as a header's name and value.
fn header(text: &str) -> Result<(String, String), UsageError> {
    let bad = || UsageError::BadHeader(text.to_string());
    let (name, value) = text.split_once(':').ok_or_else(bad)?;
    let name = name.trim();
    let token = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b);
    if name.is_empty() || !name.bytes().all(token) || value.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
        return Err(bad());
    }
    Ok((name.to_string(), value.trim().to_string()))
}

/// `bytes` for people: `512 B`, `1.5 KB`, `12.3 MB`, `4.0 GB`.
pub fn size(bytes: ByteCount) -> String {
    let b = bytes.get();
    let unit = |n: u32| 1024f64.powi(n as i32);
    match b {
        0..=1023 => format!("{b} B"),
        1024..=1_048_575 => format!("{:.1} KB", b as f64 / unit(1)),
        1_048_576..=1_073_741_823 => format!("{:.1} MB", b as f64 / unit(2)),
        _ => format!("{:.1} GB", b as f64 / unit(3)),
    }
}

/// The progress line: how much is in, of how much, and how fast.
pub fn progress_line(done: ByteCount, total: Option<ByteCount>, bytes_per_sec: Option<f64>) -> String {
    let mut line = match total.filter(|t| !t.is_zero()) {
        Some(t) => {
            let pct = done.get().saturating_mul(100) / t.get();
            format!("{pct:>3}%  {} of {}", size(done), size(t))
        }
        None => size(done),
    };
    if let Some(speed) = bytes_per_sec.filter(|s| s.is_finite() && *s >= 0.0) {
        line.push_str(&format!("  {}/s", size(ByteCount::new(speed as u64))));
    }
    line
}

/// How often the progress line is redrawn at most.
const REDRAW: Duration = Duration::from_millis(250);

/// A followed download's progress on standard error: a line when it is
/// added and when it starts, and on a terminal one line that keeps
/// changing with its progress.
pub struct Terminal {
    quiet: bool,
    /// Standard error is a terminal, so a line can be redrawn.
    live: bool,
    /// The progress line is on screen, unended.
    drawn: bool,
    /// When the line was last drawn, and how much was in then.
    last: Option<(Instant, ByteCount)>,
    /// Bytes per second, smoothed.
    speed: Option<f64>,
}

impl Terminal {
    /// Progress for a person at a terminal, or with `quiet`, none.
    pub fn new(quiet: bool) -> Self {
        Terminal { quiet, live: !quiet && io::stderr().is_terminal(), drawn: false, last: None, speed: None }
    }

    /// End the progress line, so what is printed next starts on a line of
    /// its own.
    pub fn finish(&mut self) {
        if self.drawn {
            self.write("\n");
            self.drawn = false;
        }
    }

    fn write(&self, text: &str) {
        let mut err = io::stderr().lock();
        // Nothing can be told about a standard error that cannot be written.
        let _ = err.write_all(text.as_bytes()).and_then(|()| err.flush());
    }

    /// Say `text` on a line of its own, unless quiet.
    fn say(&mut self, text: &str) {
        if self.quiet {
            return;
        }
        self.finish();
        self.write(&format!("{text}\n"));
    }
}

impl Watch for Terminal {
    fn added(&mut self, id: DownloadId) {
        self.say(&format!("Download {id} added"));
    }

    fn started(&mut self, path: &str, total: Option<ByteCount>, connections: usize) {
        let of = total.map_or_else(|| "size unknown".to_string(), size);
        let plural = if connections == 1 { "" } else { "s" };
        self.say(&format!("Saving {path} ({of}, {connections} connection{plural})"));
    }

    fn progress(&mut self, done: ByteCount, total: Option<ByteCount>) {
        if !self.live {
            return;
        }
        let now = Instant::now();
        if let Some((at, before)) = self.last {
            let elapsed = now.duration_since(at);
            if elapsed < REDRAW {
                return;
            }
            let now_speed = done.saturating_sub(before).as_f64() / elapsed.as_secs_f64();
            self.speed = Some(self.speed.map_or(now_speed, |s| s * 0.7 + now_speed * 0.3));
        }
        self.last = Some((now, done));
        // Spaces cover what a longer line before left.
        self.write(&format!("\r{}      ", progress_line(done, total, self.speed)));
        self.drawn = true;
    }

    fn needs_answer(&mut self, path: Option<&str>) {
        let about = path.map(|p| format!(" about {p}")).unwrap_or_default();
        self.say(&format!("Waiting for an answer in UDM's save-as dialog{about}"));
    }
}

/// The list as a table, a download a line.
pub fn table(listing: &Listing) -> String {
    let mut out = format!("{:>5}  {:<11}  {:>17}  NAME\n", "ID", "STATUS", "PROGRESS");
    for e in &listing.downloads {
        out.push_str(&format!("{:>5}  {:<11}  {:>17}  {}\n", e.id, status(e.status), done_of(e), name(e)));
    }
    let queues: Vec<String> = listing
        .queues
        .iter()
        .map(|q| format!("{} ({}, {})", q.name, q.id, if q.running { "started" } else { "stopped" }))
        .collect();
    out.push_str(&format!("Queues: {}", queues.join(", ")));
    out
}

/// A status as the list's JSON spells it.
fn status(s: Status) -> String {
    serde_json::to_value(s).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

fn done_of(e: &DownloadEntry) -> String {
    match e.total.filter(|t| !t.is_zero()) {
        Some(t) => format!("{}%  {}", e.done.get().saturating_mul(100) / t.get(), size(t)),
        None => size(e.done),
    }
}

/// The file's name, or the address before it has one.
fn name(e: &DownloadEntry) -> String {
    e.path
        .as_deref()
        .and_then(|p| Path::new(p).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| e.url.to_string())
}

/// The list as JSON: `{"downloads": [...], "queues": [...]}`, each as the
/// automation API's `downloads` and `queues` messages carry them.
pub fn json(listing: &Listing) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&serde_json::json!({ "downloads": listing.downloads, "queues": listing.queues }))
}

/// What a command prints and the code it exits with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub code: i32,
    /// For standard output: what a script reads, such as a path or an id.
    pub out: Option<String>,
    /// For standard error: what a person reads.
    pub err: Option<String>,
}

impl Report {
    /// Done, with nothing to say.
    pub fn ok() -> Self {
        Report { code: exit::OK, out: None, err: None }
    }

    fn printing(out: impl ToString) -> Self {
        Report { out: Some(out.to_string()), ..Self::ok() }
    }

    fn telling(code: i32, err: impl Into<String>) -> Self {
        Report { code, out: None, err: Some(err.into()) }
    }

    /// The command line was wrong.
    pub fn usage(e: &UsageError) -> Self {
        Self::telling(exit::USAGE, format!("udm-cli: {e}\nTry udm-cli --help."))
    }

    /// The request did not get done.
    pub fn error(e: &RemoteError) -> Self {
        let code = if e.unreachable() { exit::UNREACHABLE } else { exit::FAILED };
        Self::telling(code, format!("udm-cli: {e}"))
    }

    /// A pairing code shown: the QR code and what to do with it, with the
    /// link pointed at `host` when it is given.
    pub fn pairing(state: &RemoteState, host: Option<&str>, now: u64) -> Self {
        let Some(offer) = &state.pairing else {
            return Self::telling(exit::FAILED, "udm-cli: no pairing code was shown");
        };
        let link = host.map_or_else(|| offer.link.clone(), |h| link_at(&offer.link, h));
        let page = link.split('#').next().unwrap_or(&link).to_string();
        let mut out = qr_text(&link).unwrap_or_default();
        out.push_str(&format!("Scan the QR code with the phone's camera, or open {page} and type the code:\n\n"));
        out.push_str(&format!("    {}\n\n", offer.code.grouped()));
        let left = offer.expires_at.saturating_sub(now);
        out.push_str(&format!("The code works once, for {}:{:02} more.\n", left / 60, left % 60));
        if let Some(fingerprint) = &state.listener.fingerprint {
            out.push_str(&format!(
                "Before going past the phone's certificate warning, check that the certificate's\nSHA-256 fingerprint is {fingerprint}"
            ));
        }
        Self::printing(out.trim_end())
    }

    /// The paired devices, one a line: number, name, when paired and last
    /// seen (UTC).
    pub fn devices(devices: &[DeviceInfo]) -> Self {
        if devices.is_empty() {
            return Report { err: Some("No phone is paired.".into()), ..Self::ok() };
        }
        let when = |t: u64| {
            let (y, m, d) = crate::web_remote::certificate::civil((t / 86_400) as i64);
            format!("{y}-{m:02}-{d:02} {:02}:{:02} UTC", t % 86_400 / 3600, t % 3600 / 60)
        };
        let lines: Vec<String> = devices
            .iter()
            .map(|d| {
                let seen = d.last_seen.map(|t| format!(", last seen {}", when(t))).unwrap_or_default();
                format!("{}\t{}\tpaired {}{seen}", d.id, d.name.as_str(), when(d.paired_at))
            })
            .collect();
        Self::printing(lines.join("\n"))
    }

    /// How a download ended: its path on standard output when it is saved,
    /// its id when it was not followed.
    pub fn got(got: &Got) -> Self {
        match got {
            Got::Added(id) => Self::printing(id),
            Got::Saved { path, note, .. } => Report { err: note.clone(), ..Self::printing(path) },
            Got::Failed { id, error } => Self::telling(exit::FAILED, format!("Download {id} failed: {error}")),
            Got::Mismatch { id, path, expected, actual } => Self::telling(
                exit::FAILED,
                format!("Download {id} does not match its checksum: {path} is {actual}, not {expected}"),
            ),
            Got::Unchecked { id, path, why } => {
                Self::telling(exit::FAILED, format!("Download {id} was saved as {path}, but not checked: {why}"))
            }
            Got::Stopped(id) => Self::telling(
                exit::STOPPED,
                format!("Download {id} stopped before it finished; udm-cli resume {id} goes on with it"),
            ),
            Got::Waiting(id) => Self::telling(exit::STOPPED, format!("Download {id} waits in a queue")),
            Got::Removed(id) => Self::telling(exit::STOPPED, format!("Download {id} was removed from the list")),
        }
    }

    /// A resumed download's id on standard output: a retried one has a new
    /// one.
    pub fn resumed(resumed: Resumed) -> Self {
        match resumed {
            Resumed::Running(id) => {
                Report { err: Some(format!("Download {id} is running already")), ..Self::printing(id) }
            }
            Resumed::Started(id) => Self::printing(id),
            Resumed::Retried { old, new } => {
                Report { err: Some(format!("Download {old} goes on as download {new}")), ..Self::printing(new) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use udm_application::daemon::Failure;
    use udm_domain::queue::Queue;

    fn cwd() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\work")
        } else {
            PathBuf::from("/work")
        }
    }

    fn args(line: &str) -> Result<Invocation, UsageError> {
        parse(&line.split_whitespace().collect::<Vec<_>>(), &cwd())
    }

    fn get(line: &str) -> GetRequest {
        match args(line) {
            Ok(Invocation::Get { request, .. }) => request,
            other => panic!("{line}: {other:?}"),
        }
    }

    #[test]
    fn get_takes_an_address_and_its_options_in_any_order() {
        let r = get("get -c 4 https://x.example/a.iso --name b.iso -H X-Token:1 --header=Accept:*/* --no-wait");
        assert_eq!(r.url, "https://x.example/a.iso");
        assert_eq!(r.connections, Some(4));
        assert_eq!(r.filename.as_ref().map(FileName::as_str), Some("b.iso"));
        assert_eq!(r.headers, [("X-Token".to_string(), "1".to_string()), ("Accept".into(), "*/*".into())]);
        assert!(!r.follow);
        let r = get("get ftp://x.example/a.iso --checksum sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert!(r.follow && r.checksum.is_some() && r.folder.is_none());
        assert!(matches!(args("get https://x/a -q"), Ok(Invocation::Get { quiet: true, .. })));
    }

    #[test]
    fn magnet_links_and_torrent_files_are_taken() {
        let magnet = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=x";
        let r = get(&format!("get {magnet}"));
        assert_eq!((r.url.as_str(), r.metainfo.is_none()), (magnet, true));

        let dir = std::env::temp_dir().join(format!("udm_cli_torrent_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut info = b"d6:lengthi10e4:name5:a.iso12:piece lengthi16384e6:pieces20:".to_vec();
        info.extend_from_slice(&[1; 20]);
        info.push(b'e');
        let bytes = crate::torrent::metainfo::build(&info, &[]);
        std::fs::write(dir.join("a.torrent"), &bytes).unwrap();
        let Ok(Invocation::Get { request, .. }) = parse(&["get", "a.torrent"], &dir) else { panic!("taken") };
        assert!(request.url.is_torrent() && request.url.as_str().contains("&dn=a.iso"));
        assert_eq!(request.metainfo, Some(bytes));
        std::fs::write(dir.join("big.torrent"), vec![b'd'; udm_domain::torrent::MAX_SENT_METAINFO_BYTES + 1]).unwrap();
        assert!(
            matches!(parse(&["get", "big.torrent"], &dir), Err(UsageError::BadTorrent(_, why)) if why.contains("web address"))
        );
        std::fs::write(dir.join("b.torrent"), b"<html>").unwrap();
        assert!(matches!(parse(&["get", "b.torrent"], &dir), Err(UsageError::BadTorrent(..))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_relative_folder_is_under_the_current_one() {
        let r = get("get https://x/a -d sub");
        assert_eq!(r.folder.map(PathBuf::from), Some(cwd().join("sub")));
        let full = if cfg!(windows) { r"D:\Downloads" } else { "/srv/dl" };
        let r = get(&format!("get https://x/a --folder={full}"));
        assert_eq!(r.folder.as_deref(), Some(full));
    }

    #[test]
    fn a_wrong_get_says_what_is_wrong() {
        assert_eq!(args("get"), Err(UsageError::Missing("the address to download")));
        assert_eq!(args("get file:///etc/passwd"), Err(UsageError::NotDownloadAddress("file:///etc/passwd".into())));
        assert_eq!(args("get missing.torrent"), Err(UsageError::NotDownloadAddress("missing.torrent".into())));
        assert_eq!(args("get https://x/a https://x/b"), Err(UsageError::Extra("https://x/b".into())));
        assert_eq!(args("get https://x/a -c 17"), Err(UsageError::BadConnections("17".into())));
        assert_eq!(args("get https://x/a -c"), Err(UsageError::NeedsValue("-c".into(), "a number")));
        assert_eq!(args("get https://x/a -n a/b"), Err(UsageError::NotFileName("a/b".into())));
        assert_eq!(args("get https://x/a -H nocolon"), Err(UsageError::BadHeader("nocolon".into())));
        assert_eq!(args("get https://x/a -H Bad\u{0}Name:1"), Err(UsageError::BadHeader("Bad\u{0}Name:1".into())));
        assert!(matches!(args("get https://x/a --checksum zz"), Err(UsageError::BadChecksum(..))));
        assert_eq!(args("get https://x/a --fast"), Err(UsageError::UnknownOption("--fast".into())));
    }

    #[test]
    fn the_other_commands_read_their_ids_and_names() {
        let id = |n| Which::One(DownloadId::new(n));
        assert_eq!(args("list"), Ok(Invocation::List { json: false }));
        assert_eq!(args("list --json"), Ok(Invocation::List { json: true }));
        assert_eq!(args("pause 3"), Ok(Invocation::Pause(id(3))));
        assert_eq!(args("pause --all"), Ok(Invocation::Pause(Which::All)));
        assert_eq!(args("resume 9"), Ok(Invocation::Resume(id(9))));
        assert_eq!(args("resume -a"), Ok(Invocation::Resume(Which::All)));
        assert_eq!(
            args("remove 4 --delete-files"),
            Ok(Invocation::Remove { id: DownloadId::new(4), delete_files: true })
        );
        assert_eq!(args("queue start"), Ok(Invocation::Queue { start: true, name: None }));
        assert_eq!(
            args("queue stop Main queue"),
            Ok(Invocation::Queue { start: false, name: Some("Main queue".into()) })
        );
        assert_eq!(args("--help"), Ok(Invocation::Help));
        assert_eq!(args("version"), Ok(Invocation::Version));
    }

    #[test]
    fn the_other_commands_refuse_what_they_do_not_take() {
        assert_eq!(args(""), Err(UsageError::NoCommand));
        assert_eq!(args("fetch https://x/a"), Err(UsageError::UnknownCommand("fetch".into())));
        assert_eq!(args("pause"), Err(UsageError::Missing("a download id or --all")));
        assert_eq!(args("pause x"), Err(UsageError::BadId("x".into())));
        assert_eq!(args("pause 3 --all"), Err(UsageError::Extra("3".into())));
        assert_eq!(args("pause 3 4"), Err(UsageError::Extra("4".into())));
        assert_eq!(args("remove"), Err(UsageError::Missing("a download id")));
        assert_eq!(args("list everything"), Err(UsageError::Extra("everything".into())));
        assert_eq!(args("queue"), Err(UsageError::Missing("start or stop")));
        assert_eq!(args("queue run"), Err(UsageError::UnknownCommand("queue run".into())));
        assert_eq!(args("list --all"), Err(UsageError::UnknownOption("--all".into())));
    }

    #[test]
    fn sizes_and_progress_read_as_people_write_them() {
        assert_eq!(size(ByteCount::new(512)), "512 B");
        assert_eq!(size(ByteCount::new(1536)), "1.5 KB");
        assert_eq!(size(ByteCount::mib(12)), "12.0 MB");
        assert_eq!(size(ByteCount::mib(4096)), "4.0 GB");
        let line = progress_line(ByteCount::mib(1), Some(ByteCount::mib(4)), Some(2048.0));
        assert_eq!(line, " 25%  1.0 MB of 4.0 MB  2.0 KB/s");
        assert_eq!(progress_line(ByteCount::new(10), None, None), "10 B");
    }

    fn entry(id: u64, status: &str, path: Option<&str>, total: Option<u64>, done: u64) -> DownloadEntry {
        serde_json::from_value(serde_json::json!({
            "id": id, "url": format!("https://x.example/{id}.zip"), "path": path, "total": total,
            "done": done, "status": status, "error": null, "resumable": true, "connections": 0,
        }))
        .unwrap()
    }

    #[test]
    fn the_list_is_a_table_or_json() {
        let listing = Listing {
            downloads: vec![
                entry(1, "done", Some("/dl/a.zip"), Some(2048), 2048),
                entry(2, "downloading", None, None, 10),
            ],
            queues: vec![Queue::main()],
        };
        let t = table(&listing);
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[1].contains("done") && lines[1].contains("100%") && lines[1].ends_with("a.zip"), "{t}");
        assert!(lines[2].contains("downloading") && lines[2].ends_with("https://x.example/2.zip"), "{t}");
        assert_eq!(lines[3], "Queues: Main queue (1, stopped)");
        let back: serde_json::Value = serde_json::from_str(&json(&listing).unwrap()).unwrap();
        assert_eq!(back["downloads"][0]["status"], "done");
        assert_eq!(back["queues"][0]["name"], "Main queue");
    }

    #[test]
    fn each_end_has_its_exit_code() {
        let id = DownloadId::new(5);
        assert_eq!(Report::got(&Got::Added(id)), Report::printing("5"));
        let saved = Report::got(&Got::Saved { id, path: "/dl/a".into(), note: None });
        assert_eq!((saved.code, saved.out.as_deref()), (exit::OK, Some("/dl/a")));
        assert_eq!(Report::got(&Got::Failed { id, error: "HTTP 404".into() }).code, exit::FAILED);
        let mismatch = Got::Mismatch { id, path: "/dl/a".into(), expected: "aa".into(), actual: "bb".into() };
        assert_eq!(Report::got(&mismatch).code, exit::FAILED);
        assert_eq!(Report::got(&Got::Stopped(id)).code, exit::STOPPED);
        assert_eq!(Report::got(&Got::Removed(id)).code, exit::STOPPED);
        assert_eq!(Report::error(&RemoteError::Connect("no".into())).code, exit::UNREACHABLE);
        assert_eq!(Report::error(&RemoteError::Refused(Failure::Other("no".into()))).code, exit::FAILED);
        assert_eq!(Report::usage(&UsageError::NoCommand).code, exit::USAGE);
        let retried = Report::resumed(Resumed::Retried { old: id, new: DownloadId::new(8) });
        assert_eq!((retried.code, retried.out.as_deref()), (exit::OK, Some("8")));
    }

    #[test]
    fn pairing_commands_read_their_words() {
        assert_eq!(args("pair"), Ok(Invocation::Pair { host: None }));
        assert_eq!(args("pair --host 192.168.1.20"), Ok(Invocation::Pair { host: Some("192.168.1.20".into()) }));
        assert_eq!(args("pair --host=nas.local:8443"), Ok(Invocation::Pair { host: Some("nas.local:8443".into()) }));
        assert_eq!(args("pair --host a/b"), Err(UsageError::BadHost("a/b".into())));
        assert_eq!(args("devices"), Ok(Invocation::Devices));
        assert_eq!(args("unpair 3"), Ok(Invocation::Unpair(DeviceId::new(3))));
        assert_eq!(args("unpair x"), Err(UsageError::BadDevice("x".into())));
        assert_eq!(args("unpair"), Err(UsageError::Missing("a device number")));
    }

    #[test]
    fn a_pairing_link_can_point_at_the_hosts_address() {
        let link = "https://172.17.0.2:18443/#pair=ABCD";
        assert_eq!(link_at(link, "192.168.1.20"), "https://192.168.1.20:18443/#pair=ABCD");
        assert_eq!(link_at(link, "nas.local:8443"), "https://nas.local:8443/#pair=ABCD");
    }

    #[test]
    fn the_pairing_report_has_a_qr_code_the_code_and_the_fingerprint() {
        use udm_domain::pairing::{ListenerState, PairingCode, PairingOffer};
        let code = PairingCode::from_random(&[5; 32]);
        let state = RemoteState {
            listener: ListenerState { fingerprint: Some("AB:CD".into()), ..ListenerState::default() },
            pairing: Some(PairingOffer {
                link: format!("https://172.17.0.2:18443/#pair={}", code.as_str()),
                code: code.clone(),
                expires_at: 1300,
            }),
            ..RemoteState::default()
        };
        let report = Report::pairing(&state, Some("192.168.1.20"), 1000);
        let out = report.out.unwrap();
        assert!(out.contains('\u{2588}'), "a QR code");
        assert!(out.contains("open https://192.168.1.20:18443/ and type"));
        assert!(out.contains(&code.grouped()));
        assert!(out.contains("5:00 more") && out.contains("AB:CD"));
        let devices = Report::devices(&[DeviceInfo {
            id: DeviceId::new(2),
            name: udm_domain::pairing::DeviceName::clean("Pixel"),
            paired_at: 86_400 + 3_660,
            last_seen: None,
        }]);
        assert_eq!(devices.out.as_deref(), Some("2\tPixel\tpaired 1970-01-02 01:01 UTC"));
    }
}
