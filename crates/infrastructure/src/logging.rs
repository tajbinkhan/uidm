//! The log file each UDM program writes its `tracing` events to (AGENTS.md
//! section 11). Each binary's `main` calls [`init`] once, first thing.
//!
//! The files sit in the data folder ([`crate::paths::daemon_log`],
//! [`crate::paths::host_log`], [`crate::paths::gui_log`],
//! [`crate::paths::cli_log`], [`crate::paths::mcp_log`]). Every event opens
//! its file for appending, writes its line in one write and closes it again,
//! so:
//!
//! - many processes can write one file at once (the browser starts a host
//!   for each port it opens), and their lines do not mix;
//! - a file moved aside by [`trim`] is never written to again, and nothing
//!   holds it open;
//! - the window and a detached daemon need no standard error, which they do
//!   not have.
//!
//! The host also writes warnings and errors to standard error: the browser
//! reads it, so a host that cannot write its file still says why it failed,
//! and its `download` and `send` modes show them on the terminal. It never
//! logs to standard output, which carries the browser's messages.
//!
//! Each file is bounded: once it is larger than [`MAX_BYTES`], the daemon
//! moves it to `<name>.old` as it starts and then every hour, replacing the
//! older one, so a program keeps at most about twice that. Only the daemon
//! that won the single-instance bind does this, so two programs never move
//! the same file at once. Nothing is truncated in place.
//!
//! A server's daemon (`udm-daemon --server`) logs to standard output
//! instead ([`init_stdout`]), where its service manager or container
//! (`docker logs`) collects it; it writes no file of its own and keeps the
//! rules below.
//!
//! Every line goes through [`redact`] before it is written, which hides the
//! password of any `scheme://user:password@host` in it.
//!
//! What may reach a log, checked for this module (keep it current when a
//! log event is added near any of these):
//!
//! - Requests from the extension and the window: the daemon logs ids,
//!   addresses and counts, never a whole `Incoming` message, its headers,
//!   cookies, form data or `Download`'s `Debug`.
//! - The host relays frames without reading them and logs none.
//! - Site logins and proxy passwords: the daemon logs the site of a password
//!   it forgets and errors from saving one, never the password; `net`'s
//!   proxy URLs carry the proxy's login, which [`redact`] hides if an error
//!   shows one.
//! - Login cookies for a crawl (`login_cookies`, `browser_cookies`): only
//!   whether any were found.
//! - Download addresses: allowed (the log is the user's own), but an FTP or
//!   HTTP address with a password in it, and reqwest's errors that end in
//!   "for url (...)", go through [`redact`].
//! - The daemon's token: written to its file only (`ipc::write_token`); the
//!   handshake sends HMAC proofs, and its errors are fixed text.
//! - Settings: the daemon logs that they changed, not what they hold.
//! - Grabber projects: names and counts; a project's login cookies stay in
//!   its file.
//! - Files that hold secrets and fail to parse: the saved requests
//!   (`history::load_requests`, decrypted), the stored passwords
//!   (`net`'s `credentials.bin`, decrypted), grabber project files
//!   (`grab::load_project_file`, cookies decrypted) and `settings.json`
//!   (older ones hold proxy passwords). serde's message quotes the value it
//!   choked on, so these go through [`json_error`], which keeps only the kind
//!   of error and where it is. The proxy password files and the sealed
//!   cookies are decrypted into a string, never parsed, and a failure there
//!   is fixed text.

use std::borrow::Cow;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::error::Category;
use tracing::Subscriber;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

/// A log file larger than this is moved aside when the daemon starts.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// The UDM program writing a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    Daemon,
    Host,
    Gui,
    /// `udm-cli`.
    Cli,
    /// `udm-mcp`, whose standard output is the protocol's.
    Mcp,
}

impl Program {
    /// Where this program's log is.
    pub fn log_path(self) -> PathBuf {
        match self {
            Program::Daemon => crate::paths::daemon_log(),
            Program::Host => crate::paths::host_log(),
            Program::Gui => crate::paths::gui_log(),
            Program::Cli => crate::paths::cli_log(),
            Program::Mcp => crate::paths::mcp_log(),
        }
    }
}

/// Send this process's `tracing` events at info level and above to
/// `program`'s log, and for the host its warnings and errors to standard
/// error too. Panics are logged as well, as standard error may go nowhere.
/// Does nothing if a subscriber is already set.
pub fn init(program: Program) {
    let to_stderr = program == Program::Host;
    install(subscriber(Sink::File(program.log_path()), to_stderr));
}

/// Send this process's `tracing` events at info level and above to standard
/// output, one line each, for a server (`udm-daemon --server`) whose
/// service manager or container keeps them. Panics are logged as well. Does
/// nothing if a subscriber is already set.
pub fn init_stdout() {
    install(subscriber(Sink::Stdout, false));
}

/// Make `subscriber` this process's, and log panics through it.
fn install(subscriber: impl Subscriber + Send + Sync) {
    if tracing::subscriber::set_global_default(subscriber).is_err() {
        return;
    }
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "panicked");
        default(info);
    }));
}

/// Move every program's log aside once it is larger than [`MAX_BYTES`].
/// Only the daemon calls this, once it holds the single-instance lock.
pub fn trim() {
    for program in [Program::Daemon, Program::Host, Program::Gui, Program::Cli, Program::Mcp] {
        let path = program.log_path();
        if let Err(e) = trim_file(&path, MAX_BYTES) {
            if e.kind() != io::ErrorKind::NotFound {
                tracing::warn!(path = %path.display(), error = %e, "could not move the log aside");
            }
        }
    }
}

/// Move `path` to `<path>.old` if it is larger than `max`, replacing an
/// older one. Returns whether it moved.
fn trim_file(path: &Path, max: u64) -> io::Result<bool> {
    if fs::metadata(path)?.len() <= max {
        return Ok(false);
    }
    fs::rename(path, old_path(path))?;
    Ok(true)
}

fn old_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    path.with_file_name(name)
}

/// The error for JSON that would not parse, saying what kind of error it is
/// and where, but not serde's message, which quotes the value it choked on.
/// For files whose values can be secrets.
pub fn json_error(e: &serde_json::Error) -> io::Error {
    let kind = match e.classify() {
        Category::Io => "could not be read",
        Category::Syntax => "is not valid JSON",
        Category::Data => "does not hold what UDM expects",
        Category::Eof => "ends too soon",
    };
    let text = match e.line() {
        0 => format!("the file {kind}"),
        line => format!("the file {kind} (line {line}, column {})", e.column()),
    };
    io::Error::new(io::ErrorKind::InvalidData, text)
}

/// Events at info level and above to `sink`, and with `to_stderr` warnings
/// and errors to standard error as well.
fn subscriber(sink: Sink, to_stderr: bool) -> impl Subscriber + Send + Sync {
    let file = tracing_subscriber::fmt::layer()
        .with_writer(Appender(sink))
        .with_timer(TimeAndPid)
        .with_filter(LevelFilter::INFO);
    let stderr = to_stderr.then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(Appender(Sink::Stderr))
            .with_timer(TimeAndPid)
            .with_filter(LevelFilter::WARN)
    });
    tracing_subscriber::registry().with(file).with(stderr)
}

/// The time in UTC, then the process id, so the lines of the hosts sharing
/// one file can be told apart.
struct TimeAndPid;

impl FormatTime for TimeAndPid {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        SystemTime.format_time(w)?;
        write!(w, " pid={}", std::process::id())
    }
}

/// Where a [`Line`] goes.
enum Sink {
    File(PathBuf),
    Stderr,
    Stdout,
}

/// Hands each event a [`Line`] that goes to its sink once it is done.
struct Appender(Sink);

impl<'a> MakeWriter<'a> for Appender {
    type Writer = Line<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        Line { sink: &self.0, text: Vec::new() }
    }
}

/// One event's text, collected and then written in a single write.
struct Line<'a> {
    sink: &'a Sink,
    text: Vec<u8>,
}

impl Write for Line<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.text.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Line<'_> {
    fn drop(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&self.text);
        let text = redact(&text);
        // A line that cannot be written is lost: there is nowhere else to
        // say so (the host's warnings also reach standard error).
        let _ = match self.sink {
            Sink::File(path) => append(path, text.as_bytes()),
            Sink::Stderr => io::stderr().lock().write_all(text.as_bytes()),
            Sink::Stdout => io::stdout().lock().write_all(text.as_bytes()),
        };
    }
}

/// Add `bytes` to the end of `path` in one write, creating it (readable only
/// by the user on Unix) and its folder if need be.
fn append(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = match open_append(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            open_append(path)?
        }
        other => other?,
    };
    file.write_all(bytes)
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// `text` with the password of every `scheme://user:password@host` in it
/// replaced by `***` (the user name and the rest of the address stay), and
/// every magnet link without its trackers and peers, whose addresses may
/// carry a private tracker's passkey.
pub fn redact(text: &str) -> Cow<'_, str> {
    match redact_passwords(text) {
        Cow::Borrowed(text) => udm_domain::torrent::scrub_magnets(text),
        Cow::Owned(text) => Cow::Owned(udm_domain::torrent::scrub_magnets(&text).into_owned()),
    }
}

/// `text` with the password of every `scheme://user:password@host` in it
/// replaced by `***`.
fn redact_passwords(text: &str) -> Cow<'_, str> {
    let mut out = String::new();
    let mut done = 0;
    let mut from = 0;
    while let Some(at) = text[from..].find("://") {
        let start = from + at + 3;
        // The authority ends where the path, query or fragment starts, or
        // where the text around the address goes on. The characters a
        // password may hold unescaped (`!$&'()*+,;=`) do not end it.
        let end = text[start..].find(|c: char| matches!(c, '/' | '?' | '#' | '"' | '<' | '>') || c.is_whitespace());
        let end = end.map_or(text.len(), |e| start + e);
        let authority = &text[start..end];
        if let Some(userinfo) = authority.rfind('@').map(|a| &authority[..a]) {
            if let Some(colon) = userinfo.find(':') {
                out.push_str(&text[done..start + colon + 1]);
                out.push_str("***");
                done = start + userinfo.len();
            }
        }
        from = end;
    }
    if done == 0 {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[done..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("udm-logging-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn redact_hides_passwords_in_addresses() {
        assert_eq!(redact("ftp://bob:hunter2@ftp.example.com/f.zip"), "ftp://bob:***@ftp.example.com/f.zip");
        assert_eq!(
            redact("error sending request for url (https://a:b%40c@x.example/p?q=1)"),
            "error sending request for url (https://a:***@x.example/p?q=1)"
        );
        assert_eq!(
            redact("proxy http://u:p@proxy:8080 then socks5://v:w)x@[::1]:1080"),
            "proxy http://u:***@proxy:8080 then socks5://v:***@[::1]:1080"
        );
        assert_eq!(redact("ftp://bob:pw@h"), "ftp://bob:***@h");
    }

    #[test]
    fn redact_hides_magnet_links_trackers() {
        let line = "add url=magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=A&tr=https://t.example/announce/PASSKEY1 done";
        let out = redact(line);
        assert!(!out.contains("PASSKEY1") && out.contains("dn=A") && out.ends_with(" done"), "{out}");
    }

    #[test]
    fn redact_leaves_text_without_passwords_alone() {
        for text in [
            "https://example.com/a?mail=a:b@c.example",
            "ftp://anonymous@ftp.example.com/pub",
            "mail bob@example.com: done",
            "http://",
            "",
        ] {
            assert!(matches!(redact(text), Cow::Borrowed(t) if t == text), "{text}");
        }
    }

    #[test]
    fn events_are_appended_with_passwords_hidden() {
        let dir = temp_dir("events");
        let path = dir.join("nested").join("udm-test.log");
        let first = subscriber(Sink::File(path.clone()), false);
        tracing::subscriber::with_default(first, || {
            tracing::info!(download_id = 7, url = "ftp://bob:hunter2@h/f", "add");
            tracing::debug!("below the level");
        });
        // A second process appends to the same file.
        tracing::subscriber::with_default(subscriber(Sink::File(path.clone()), true), || {
            tracing::warn!(error = "x", "second")
        });
        let log = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2, "{log}");
        assert!(lines[0].contains(&format!("pid={}", std::process::id())), "{log}");
        assert!(lines[0].contains("INFO") && lines[0].contains("download_id=7"), "{log}");
        assert!(lines[0].contains("ftp://bob:***@h/f") && !log.contains("hunter2"), "{log}");
        assert!(lines[1].contains("WARN") && lines[1].contains("second"), "{log}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn json_errors_do_not_quote_the_value() {
        let secret = "SESSION=hunter2";
        let typed = serde_json::from_str::<Vec<u64>>(&format!(r#"["{secret}"]"#)).unwrap_err();
        assert!(typed.to_string().contains(secret), "serde quotes it: {typed}");
        let e = json_error(&typed).to_string();
        assert_eq!(e, "the file does not hold what UDM expects (line 1, column 18)");
        let cut = serde_json::from_str::<Vec<String>>(&format!(r#"["{secret}"#)).unwrap_err();
        assert!(!json_error(&cut).to_string().contains(secret));
        let value = serde_json::json!({ "a": secret });
        let from_value = serde_json::from_value::<std::collections::HashMap<String, u64>>(value).unwrap_err();
        assert_eq!(json_error(&from_value).to_string(), "the file does not hold what UDM expects");
    }

    #[test]
    fn a_large_log_is_moved_aside_once() {
        let dir = temp_dir("trim");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("udm-daemon.log");
        let old = dir.join("udm-daemon.log.old");
        assert_eq!(trim_file(&path, 10).unwrap_err().kind(), io::ErrorKind::NotFound);
        fs::write(&path, b"0123456789").unwrap();
        assert!(!trim_file(&path, 10).unwrap(), "at the limit it stays");
        fs::write(&old, b"older").unwrap();
        append(&path, b"!").unwrap();
        assert!(trim_file(&path, 10).unwrap());
        assert!(!path.exists());
        assert_eq!(fs::read(&old).unwrap(), b"0123456789!", "the older one is replaced");
        append(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        fs::remove_dir_all(&dir).unwrap();
    }
}
