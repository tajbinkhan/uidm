//! Finding, starting and authenticating to the daemon.
//!
//! The daemon listens on a fixed loopback port. Binding that port is also the
//! single-instance lock: a second daemon fails to bind and exits. The host
//! connects to it, starting it first when nothing is listening.
//!
//! Loopback keeps other machines out but not other local processes, so both
//! ends authenticate. At start the daemon makes a random token and writes it
//! to `%LOCALAPPDATA%\UDM\daemon.token`, which only the current user (plus
//! SYSTEM and Administrators) can read under the profile's default ACL.
//!
//! The token never crosses the socket. Each side proves it holds it with an
//! HMAC-SHA256 over both sides' fresh nonces, keyed by the token:
//!
//! 1. host -> daemon: `auth_hello { client_nonce }`
//! 2. daemon -> host: `auth_challenge { server_nonce, proof }`, where `proof`
//!    is `HMAC(token, "udm-daemon", client_nonce, server_nonce)`
//! 3. host checks that proof, then sends `auth { proof }` with the
//!    `"udm-host"` label
//! 4. daemon checks it and replies `auth_ok`
//!
//! Either side closes the connection on a bad proof. A process squatting on
//! the port cannot answer step 2, so the host gives up before relaying any
//! browser frame, and the squatter learns only a random nonce.

use std::fs;
use std::io::{self, Read};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::protocol::{read_message, write_frame};

/// The daemon's port for the usual copy of UDM.
pub const DAEMON_PORT: u16 = 51515;

/// The loopback address the daemon serves on. Loopback only, so nothing off
/// this machine can reach it, and local clients must still authenticate. A
/// copy named by `UDM_INSTANCE` gets a port of its own (see
/// `paths::instance`).
pub fn daemon_addr() -> String {
    format!("127.0.0.1:{}", daemon_port(crate::paths::instance().as_deref()))
}

/// How long a connection to the daemon's port may take. On loopback the
/// system answers for a listening port at once, however busy the daemon is,
/// so this only cuts short a port nothing listens on: Windows tries such a
/// port again for about two seconds before it reports the refusal, which
/// every start of a daemon that was not running would otherwise wait out.
/// Should a connection to a running daemon ever take longer, the client
/// starts a second daemon, which cannot bind and exits, and connects on its
/// next try.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(250);

/// Open a connection to the daemon's port, failing within `CONNECT_TIMEOUT`.
fn dial() -> io::Result<TcpStream> {
    dial_port(daemon_port(crate::paths::instance().as_deref()))
}

fn dial_port(port: u16) -> io::Result<TcpStream> {
    TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), CONNECT_TIMEOUT)
}

/// The port for instance `name`: the usual one without a name, else one
/// from 52000 to 52999 picked by the name, the same on every run.
fn daemon_port(name: Option<&str>) -> u16 {
    let Some(name) = name else {
        return DAEMON_PORT;
    };
    // FNV-1a, so the port does not change between Rust versions.
    let hash = name.bytes().fold(0x811c_9dc5_u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
    52000 + (hash % 1000) as u16
}

/// How long either side waits for the other's handshake frame.
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

/// Size cap for handshake frames, far below the normal message limit so an
/// unauthenticated client cannot make the daemon allocate much.
pub const AUTH_MAX_BYTES: u32 = 1024;

/// Random bytes in a token or a nonce. Hex-encoded on disk and on the wire.
const TOKEN_BYTES: usize = 32;
const NONCE_BYTES: usize = 32;

/// Labels that make the daemon's proof and the host's proof differ, so one
/// can never be replayed as the other.
pub const DAEMON_LABEL: &str = "udm-daemon";
pub const HOST_LABEL: &str = "udm-host";

/// How long the host waits for a daemon it just started. A first launch of a
/// freshly built exe can be slow while antivirus scans it.
const STARTUP_WAIT: Duration = Duration::from_secs(10);
const STARTUP_POLL: Duration = Duration::from_millis(100);
/// How long a listener that fails the proof is given before giving up. A
/// daemon binds its port before it writes its token, so one that just
/// started can fail it for a moment.
const PROOF_WAIT: Duration = Duration::from_secs(2);

#[cfg(windows)]
const STD_INPUT_HANDLE: u32 = -10i32 as u32;
#[cfg(windows)]
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
#[cfg(windows)]
const STD_ERROR_HANDLE: u32 = -12i32 as u32;
#[cfg(windows)]
const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetStdHandle(std_handle: u32) -> *mut std::ffi::c_void;
    fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
}

/// The host <-> daemon handshake, in the order it is sent. These frames never
/// reach the browser.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Handshake {
    /// First frame a client sends.
    AuthHello { client_nonce: String },
    /// The daemon's nonce and its proof that it holds the token.
    AuthChallenge { server_nonce: String, proof: String },
    /// The client's proof that it holds the token.
    Auth { proof: String },
    /// The daemon accepted the client's proof.
    AuthOk,
}

/// Connect to the running daemon and authenticate, starting the daemon if
/// nothing answers.
///
/// A daemon that has bound but not yet written its token file looks the same
/// as one that is not up yet, so both are retried during the startup wait.
pub fn connect_or_spawn() -> io::Result<TcpStream> {
    if let Existing::Answered(connected) = connect_existing(connect_authenticated) {
        return connected;
    }
    spawn_daemon()?;
    // Give the new process time to bind and write its token. The limit is on
    // elapsed time, not attempts: a listener that accepts and then stays
    // silent costs AUTH_TIMEOUT per attempt, so the host gives up within
    // STARTUP_WAIT plus one AUTH_TIMEOUT.
    let deadline = Instant::now() + STARTUP_WAIT;
    let mut last_err = None;
    while Instant::now() < deadline {
        thread::sleep(STARTUP_POLL);
        match connect_authenticated() {
            Ok(stream) => return Ok(stream),
            Err(e) => last_err = Some(e),
        }
    }
    let detail = last_err.map(|e| e.to_string()).unwrap_or_default();
    Err(io::Error::new(io::ErrorKind::TimedOut, format!("could not connect to daemon on {}: {detail}", daemon_addr())))
}

/// Connect to the running daemon and authenticate, without starting one:
/// for a host that runs inside Safari's sandbox, where a daemon it started
/// would be sandboxed too. Fails with `NotConnected` when nothing usable
/// answers, saying why (nothing listens, the token cannot be read, ...).
pub fn connect_running() -> io::Result<TcpStream> {
    match connect_existing(connect_authenticated) {
        Existing::Answered(connected) => connected,
        Existing::Absent(why) => Err(io::Error::new(
            io::ErrorKind::NotConnected,
            format!("UDM is not running: no daemon answers on {} ({why})", daemon_addr()),
        )),
    }
}

/// What a connection to a daemon already running came to.
enum Existing {
    /// A daemon answered: its stream, or the failure of a listener that
    /// cannot prove it is the daemon.
    Answered(io::Result<TcpStream>),
    /// Nothing usable answered, for this reason, so a daemon may be started.
    Absent(io::Error),
}

/// What a daemon already running makes of a connection through `connect`
/// (`connect_authenticated`, or a stand-in in tests). A listener that fails
/// the proof is given `PROOF_WAIT`, as a daemon that just bound may not
/// have written its token yet.
fn connect_existing(connect: impl Fn() -> io::Result<TcpStream>) -> Existing {
    match connect() {
        Ok(stream) => Existing::Answered(Ok(stream)),
        // Something holds the port but cannot prove it is the daemon.
        // Starting another daemon cannot help, as it could not bind.
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            drop_package_token();
            Existing::Answered(retry_proof(e, connect))
        }
        Err(e) => Existing::Absent(e),
    }
}

/// Stop the running daemon, if any, then start a new one and connect to
/// it. For a daemon that is up but does not answer or prove itself.
pub fn restart_daemon() -> io::Result<TcpStream> {
    stop_daemons()?;
    // The old one's listener goes with its process.
    let deadline = Instant::now() + STARTUP_WAIT;
    while dial().is_ok() && Instant::now() < deadline {
        thread::sleep(STARTUP_POLL);
    }
    connect_or_spawn()
}

/// End every `udm-daemon` process this user may end, and the
/// `pullbay-daemon` and `uidm-daemon` of UDM's earlier names, which hold the
/// same port and cannot prove themselves to UDM. A named copy (see
/// `paths::instance`) ends none: it cannot tell its own daemon from the
/// installed one's, so its restart only reconnects.
fn stop_daemons() -> io::Result<()> {
    if crate::paths::instance().is_some() {
        return Ok(());
    }
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("taskkill");
        c.args(["/F", "/IM", "udm-daemon.exe", "/IM", "pullbay-daemon.exe", "/IM", "uidm-daemon.exe"]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = std::process::Command::new("pkill");
        c.args(["-x", "udm-daemon|pullbay-daemon|uidm-daemon"]);
        c
    };
    // A non-zero exit means none was running, which is fine.
    crate::platform::no_window(&mut cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|_| ())
}

/// Inside a Windows app package, delete the package's own copy of the token,
/// if a daemon once started there left one. Windows reads that copy before
/// the real file, so a stale one fails every handshake from the package.
fn drop_package_token() {
    #[cfg(windows)]
    if let Some(copy) = crate::package::data_copy() {
        let _ = fs::remove_file(copy.join("daemon.token"));
    }
}

/// Try the handshake again for `PROOF_WAIT`, for a daemon that has bound
/// but not yet written its token; else fail with `first`.
fn retry_proof(first: io::Error, connect: impl Fn() -> io::Result<TcpStream>) -> io::Result<TcpStream> {
    let deadline = Instant::now() + PROOF_WAIT;
    let mut last = first;
    while Instant::now() < deadline {
        thread::sleep(STARTUP_POLL);
        match connect() {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Connect once and run the client side of the handshake. Returns the stream
/// ready for normal frames only after the daemon has proved itself.
fn connect_authenticated() -> io::Result<TcpStream> {
    let mut stream = dial()?;
    let token = read_token()?;
    let client_nonce = new_nonce()?;
    stream.set_read_timeout(Some(AUTH_TIMEOUT))?;

    send_handshake(&mut stream, &Handshake::AuthHello { client_nonce: client_nonce.clone() })?;
    let server_nonce = match recv_handshake(&mut stream)? {
        Handshake::AuthChallenge { server_nonce, proof: given }
            if ct_eq(given.as_bytes(), proof(&token, DAEMON_LABEL, &client_nonce, &server_nonce).as_bytes()) =>
        {
            server_nonce
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("the listener on {} could not prove it is the UDM daemon", daemon_addr()),
            ))
        }
    };

    let host_proof = proof(&token, HOST_LABEL, &client_nonce, &server_nonce);
    send_handshake(&mut stream, &Handshake::Auth { proof: host_proof })?;
    match recv_handshake(&mut stream)? {
        Handshake::AuthOk => {}
        _ => return Err(io::Error::new(io::ErrorKind::PermissionDenied, "daemon rejected the host's proof")),
    }

    stream.set_read_timeout(None)?;
    Ok(stream)
}

/// Write one handshake frame.
pub fn send_handshake(stream: &mut TcpStream, msg: &Handshake) -> io::Result<()> {
    write_frame(stream, &serde_json::to_vec(msg)?)
}

/// Read one handshake frame, capped at `AUTH_MAX_BYTES`. A close or anything
/// that is not a handshake message is an error.
pub fn recv_handshake(stream: &mut impl Read) -> io::Result<Handshake> {
    let raw = read_message(stream, AUTH_MAX_BYTES)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed during auth"))?;
    serde_json::from_slice(&raw)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad handshake frame: {e}")))
}

/// `HMAC-SHA256(token, label, client_nonce, server_nonce)`, hex-encoded. Each
/// field is length-prefixed so no two different inputs feed the MAC the same
/// bytes.
pub fn proof(token: &str, label: &str, client_nonce: &str, server_nonce: &str) -> String {
    // Invariant: HMAC hashes a long key and pads a short one, so
    // `new_from_slice` never fails for it.
    #[allow(clippy::expect_used)]
    let mut mac = Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("HMAC accepts keys of any length");
    for field in [label, client_nonce, server_nonce] {
        mac.update(&(field.len() as u32).to_le_bytes());
        mac.update(field.as_bytes());
    }
    hex(&mac.finalize().into_bytes())
}

/// A fresh random token, hex-encoded.
pub fn new_token() -> io::Result<String> {
    random_hex(TOKEN_BYTES)
}

/// A fresh random handshake nonce, hex-encoded.
pub fn new_nonce() -> io::Result<String> {
    random_hex(NONCE_BYTES)
}

fn random_hex(len: usize) -> io::Result<String> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(format!("getrandom: {e}")))?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where the running daemon's token lives.
pub fn token_path() -> io::Result<PathBuf> {
    Ok(crate::paths::data_dir()?.join("daemon.token"))
}

/// Publish `token` for clients. Writes a temp file and renames it over the old
/// one, so a reader never sees a half-written token.
pub fn write_token(token: &str) -> io::Result<()> {
    let path = token_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("token.tmp");
    crate::platform::write_private(&tmp, token.as_bytes())?;
    fs::rename(&tmp, &path)
}

fn read_token() -> io::Result<String> {
    Ok(fs::read_to_string(token_path()?)?.trim().to_string())
}

/// Compare two proofs in time that depends only on their length.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A reader over a handshake that fails once `until` has passed, however
/// slowly the bytes come: each read waits only for the time left. A plain
/// read timeout starts again with every byte, so a client sending one byte
/// at a time could hold a daemon thread for an hour.
pub struct Deadline<'a> {
    stream: &'a TcpStream,
    until: Instant,
}

impl<'a> Deadline<'a> {
    pub fn new(stream: &'a TcpStream, until: Instant) -> Self {
        Deadline { stream, until }
    }
}

impl Read for Deadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the handshake took too long"));
        }
        self.stream.set_read_timeout(Some(left))?;
        (&mut &*self.stream).read(buf)
    }
}

/// Launch `udm-daemon` from the host's own directory as a detached
/// process, so it keeps running after the host and the browser exit. It
/// writes its own log (see `logging`), so its standard streams go nowhere.
fn spawn_daemon() -> io::Result<()> {
    keep_stdio_private();
    // Started from where Windows keeps a package's copy of AppData, the
    // daemon would keep its files in that copy; it runs outside it instead.
    #[cfg(windows)]
    if crate::package::data_copy().is_some() {
        let (program, args) = crate::platform::program("udm-daemon")?;
        return crate::package::spawn_outside(&program, &args);
    }
    let mut daemon = crate::platform::program_command("udm-daemon")?;

    let program = daemon.get_program().to_owned();
    crate::platform::spawn_reaped(
        crate::platform::detach(&mut daemon).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()),
    )
    .map_err(|e| io::Error::new(e.kind(), format!("starting {program:?}: {e}")))
}

/// Clear the inherit flag on this process's stdin, stdout and stderr.
///
/// Windows hands every inheritable handle to a child, and the pipes a browser
/// gives its native host are inheritable. Without this the daemon would keep
/// the browser's pipes open for its whole life, so the browser would never see
/// the host's stdout close. The handles still work in this process. (Unix
/// closes every other descriptor on exec already.)
#[cfg(not(windows))]
fn keep_stdio_private() {}

#[cfg(windows)]
fn keep_stdio_private() {
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle takes no pointers. SetHandleInformation only
        // changes a flag on a handle this process owns; a null or invalid
        // handle makes it fail, which is fine to ignore.
        unsafe {
            let handle = GetStdHandle(which);
            if !handle.is_null() && handle as isize != -1 {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_copy_gets_its_own_steady_port() {
        assert_eq!(daemon_port(None), DAEMON_PORT);
        let test = daemon_port(Some("test"));
        assert!((52000..53000).contains(&test), "{test}");
        assert_eq!(daemon_port(Some("test")), test, "the same name gives the same port");
        assert_ne!(daemon_port(Some("test")), daemon_port(Some("dev")));
    }

    #[test]
    fn a_port_nothing_listens_on_fails_fast() {
        // Windows takes about two seconds to refuse a loopback port with no
        // listener; a daemon that is not running must not cost a client that.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let start = Instant::now();
        assert!(dial_port(port).is_err());
        assert!(start.elapsed() < Duration::from_millis(1500), "took {:?}", start.elapsed());
    }

    #[test]
    fn a_listening_port_connects() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(dial_port(listener.local_addr().unwrap().port()).is_ok());
    }

    #[test]
    fn only_a_daemon_already_running_is_connected_to() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // A daemon that answers: its stream.
        let Existing::Answered(Ok(found)) = connect_existing(|| dial_port(port)) else { panic!("no stream") };
        assert_eq!(found.peer_addr().unwrap().port(), port);
        // Nothing answers: absent, with the reason, so connect_running fails
        // saying why and connect_or_spawn starts one.
        let refused = || Err(io::Error::new(io::ErrorKind::ConnectionRefused, "refused here"));
        let Existing::Absent(why) = connect_existing(refused) else { panic!("an answer") };
        assert_eq!(why.to_string(), "refused here");
        // A listener that never proves itself is tried for PROOF_WAIT, then
        // refused, and no daemon is started beside it.
        let start = Instant::now();
        let tries = std::cell::Cell::new(0);
        let impostor = || {
            tries.set(tries.get() + 1);
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        };
        let Existing::Answered(Err(failed)) = connect_existing(impostor) else { panic!("no refusal") };
        assert_eq!(failed.kind(), io::ErrorKind::PermissionDenied);
        assert!(tries.get() > 1 && start.elapsed() >= PROOF_WAIT, "{} tries in {:?}", tries.get(), start.elapsed());
    }

    #[test]
    fn tokens_are_hex_and_distinct() {
        let a = new_token().unwrap();
        let b = new_token().unwrap();
        assert_eq!(a.len(), TOKEN_BYTES * 2);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn constant_time_comparison() {
        assert!(ct_eq(b"abcd", b"abcd"));
        assert!(!ct_eq(b"abcd", b"abce"));
        assert!(!ct_eq(b"abcd", b"abc"));
        assert!(!ct_eq(b"", b"a"));
    }

    /// RFC 4231 test case 2 pins the HMAC-SHA256 primitive. `proof` adds
    /// length prefixes on top, so check it against a hand-built input too.
    #[test]
    fn proof_is_hmac_sha256_of_prefixed_fields() {
        let mut mac = Hmac::<Sha256>::new_from_slice(b"Jefe").unwrap();
        mac.update(b"what do ya want for nothing?");
        assert_eq!(
            hex(&mac.finalize().into_bytes()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );

        let mut mac = Hmac::<Sha256>::new_from_slice(b"k").unwrap();
        mac.update(b"\x03\0\0\0lab\x01\0\0\0c\x01\0\0\0s");
        assert_eq!(proof("k", "lab", "c", "s"), hex(&mac.finalize().into_bytes()));
    }

    /// A proof depends on every input, and the two roles never share one.
    #[test]
    fn proofs_bind_role_nonces_and_token() {
        let base = proof("tok", DAEMON_LABEL, "cn", "sn");
        assert_eq!(base.len(), 64);
        assert_ne!(base, proof("tok", HOST_LABEL, "cn", "sn"));
        assert_ne!(base, proof("tok", DAEMON_LABEL, "cx", "sn"));
        assert_ne!(base, proof("tok", DAEMON_LABEL, "cn", "sx"));
        assert_ne!(base, proof("other", DAEMON_LABEL, "cn", "sn"));
        // Length prefixes keep field boundaries from shifting.
        assert_ne!(proof("tok", DAEMON_LABEL, "ab", "c"), proof("tok", DAEMON_LABEL, "a", "bc"));
    }

    /// The handshake shapes are part of the host <-> daemon contract.
    #[test]
    fn handshake_wire_format() {
        let cases = [
            (Handshake::AuthHello { client_nonce: "c".into() }, r#"{"op":"auth_hello","client_nonce":"c"}"#),
            (
                Handshake::AuthChallenge { server_nonce: "s".into(), proof: "p".into() },
                r#"{"op":"auth_challenge","server_nonce":"s","proof":"p"}"#,
            ),
            (Handshake::Auth { proof: "p".into() }, r#"{"op":"auth","proof":"p"}"#),
            (Handshake::AuthOk, r#"{"op":"auth_ok"}"#),
        ];
        for (msg, json) in cases {
            assert_eq!(serde_json::to_string(&msg).unwrap(), json);
        }
    }
}
