//! Finding, starting and authenticating to the daemon.
//!
//! The daemon listens on a fixed loopback port. Binding that port is also the
//! single-instance lock: a second daemon fails to bind and exits. The host
//! connects to it, starting it first when nothing is listening.
//!
//! Loopback keeps other machines out but not other local processes, so both
//! ends authenticate. At start the daemon makes a random token and writes it
//! to `%LOCALAPPDATA%\UIDM\daemon.token`, which only the current user (plus
//! SYSTEM and Administrators) can read under the profile's default ACL.
//!
//! The token never crosses the socket. Each side proves it holds it with an
//! HMAC-SHA256 over both sides' fresh nonces, keyed by the token:
//!
//! 1. host -> daemon: `auth_hello { client_nonce }`
//! 2. daemon -> host: `auth_challenge { server_nonce, proof }`, where `proof`
//!    is `HMAC(token, "uidm-daemon", client_nonce, server_nonce)`
//! 3. host checks that proof, then sends `auth { proof }` with the
//!    `"uidm-host"` label
//! 4. daemon checks it and replies `auth_ok`
//!
//! Either side closes the connection on a bad proof. A process squatting on
//! the port cannot answer step 2, so the host gives up before relaying any
//! browser frame, and the squatter learns only a random nonce.

use std::fs;
use std::io;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::protocol::{read_message, write_frame};

/// Loopback address the daemon serves on. Loopback only: nothing off this
/// machine can reach it, and local clients must still authenticate.
pub const DAEMON_ADDR: &str = "127.0.0.1:51515";

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
pub const DAEMON_LABEL: &str = "uidm-daemon";
pub const HOST_LABEL: &str = "uidm-host";


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
    match connect_authenticated() {
        Ok(stream) => return Ok(stream),
        // Something holds the port but cannot prove it is the daemon.
        // Starting another daemon cannot help, as it could not bind.
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            drop_package_token();
            return retry_proof(e);
        }
        Err(_) => {}
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
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("could not connect to daemon on {DAEMON_ADDR}: {detail}"),
    ))
}

/// Stop the running daemon, if any, then start a new one and connect to
/// it. For a daemon that is up but does not answer or prove itself.
pub fn restart_daemon() -> io::Result<TcpStream> {
    stop_daemons()?;
    // The old one's listener goes with its process.
    let deadline = Instant::now() + STARTUP_WAIT;
    while TcpStream::connect(DAEMON_ADDR).is_ok() && Instant::now() < deadline {
        thread::sleep(STARTUP_POLL);
    }
    connect_or_spawn()
}

/// End every `uidm-daemon` process this user may end.
fn stop_daemons() -> io::Result<()> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("taskkill");
        c.args(["/F", "/IM", "uidm-daemon.exe"]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = std::process::Command::new("pkill");
        c.args(["-x", "uidm-daemon"]);
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
fn retry_proof(first: io::Error) -> io::Result<TcpStream> {
    let deadline = Instant::now() + PROOF_WAIT;
    let mut last = first;
    while Instant::now() < deadline {
        thread::sleep(STARTUP_POLL);
        match connect_authenticated() {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Connect once and run the client side of the handshake. Returns the stream
/// ready for normal frames only after the daemon has proved itself.
fn connect_authenticated() -> io::Result<TcpStream> {
    let mut stream = TcpStream::connect(DAEMON_ADDR)?;
    let token = read_token()?;
    let client_nonce = new_nonce()?;
    stream.set_read_timeout(Some(AUTH_TIMEOUT))?;

    send_handshake(
        &mut stream,
        &Handshake::AuthHello {
            client_nonce: client_nonce.clone(),
        },
    )?;
    let server_nonce = match recv_handshake(&mut stream)? {
        Handshake::AuthChallenge {
            server_nonce,
            proof: given,
        } if ct_eq(
            given.as_bytes(),
            proof(&token, DAEMON_LABEL, &client_nonce, &server_nonce).as_bytes(),
        ) =>
        {
            server_nonce
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("the listener on {DAEMON_ADDR} could not prove it is the UIDM daemon"),
            ))
        }
    };

    let host_proof = proof(&token, HOST_LABEL, &client_nonce, &server_nonce);
    send_handshake(&mut stream, &Handshake::Auth { proof: host_proof })?;
    match recv_handshake(&mut stream)? {
        Handshake::AuthOk => {}
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "daemon rejected the host's proof",
            ))
        }
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
pub fn recv_handshake(stream: &mut TcpStream) -> io::Result<Handshake> {
    let raw = read_message(stream, AUTH_MAX_BYTES)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "connection closed during auth",
        )
    })?;
    serde_json::from_slice(&raw).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bad handshake frame: {e}"),
        )
    })
}

/// `HMAC-SHA256(token, label, client_nonce, server_nonce)`, hex-encoded. Each
/// field is length-prefixed so no two different inputs feed the MAC the same
/// bytes.
pub fn proof(token: &str, label: &str, client_nonce: &str, server_nonce: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("HMAC accepts keys of any length");
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
    Ok(crate::platform::data_dir()?.join("daemon.token"))
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

/// The daemon's log, in the temp folder.
pub fn daemon_log() -> PathBuf {
    std::env::temp_dir().join("uidm-daemon.log")
}

/// Launch `uidm-daemon` from the host's own directory as a detached
/// process, so it keeps running after the host and the browser exit. Its log
/// goes to `uidm-daemon.log` in the temp folder, added to the end: a daemon
/// that is already running keeps writing there too, and truncating the file
/// under it would leave a run of NULs where its log was.
fn spawn_daemon() -> io::Result<()> {
    keep_stdio_private();
    // Started from where Windows keeps a package's copy of AppData, the
    // daemon would keep its files in that copy; it runs outside it instead.
    #[cfg(windows)]
    if crate::package::data_copy().is_some() {
        let (program, mut args) = crate::platform::program("uidm-daemon")?;
        args.push(crate::package::LOG_ARG.to_string());
        return crate::package::spawn_outside(&program, &args);
    }
    let log = fs::OpenOptions::new().create(true).append(true).open(daemon_log())?;
    let mut daemon = crate::platform::program_command("uidm-daemon")?;

    crate::platform::detach(&mut daemon)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("starting {:?}: {e}", daemon.get_program())))?;
    Ok(())
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
        assert_eq!(
            proof("k", "lab", "c", "s"),
            hex(&mac.finalize().into_bytes())
        );
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
        assert_ne!(
            proof("tok", DAEMON_LABEL, "ab", "c"),
            proof("tok", DAEMON_LABEL, "a", "bc")
        );
    }

    /// The handshake shapes are part of the host <-> daemon contract.
    #[test]
    fn handshake_wire_format() {
        let cases = [
            (
                Handshake::AuthHello {
                    client_nonce: "c".into(),
                },
                r#"{"op":"auth_hello","client_nonce":"c"}"#,
            ),
            (
                Handshake::AuthChallenge {
                    server_nonce: "s".into(),
                    proof: "p".into(),
                },
                r#"{"op":"auth_challenge","server_nonce":"s","proof":"p"}"#,
            ),
            (
                Handshake::Auth { proof: "p".into() },
                r#"{"op":"auth","proof":"p"}"#,
            ),
            (Handshake::AuthOk, r#"{"op":"auth_ok"}"#),
        ];
        for (msg, json) in cases {
            assert_eq!(serde_json::to_string(&msg).unwrap(), json);
        }
    }
}
