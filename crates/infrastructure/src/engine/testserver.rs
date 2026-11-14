//! A tiny HTTP/1.1 server on loopback for engine tests. It serves one body at
//! every path, honors single `Range: bytes=a-b` requests with 206, sends an
//! ETag, and closes each connection after one response.
//!
//! It can also behave like a bad server, so tests can check that the engine
//! copes: it drops connections partway, answers only so many connections
//! from one client at once, waits before answering, answers a range request
//! with another range or the whole file, and sends at a set speed (see
//! [`Behavior`]). `tools/bench/faultserver.py` offers the same faults to
//! programs outside the tests.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// How the server misbehaves, for tests that need it.
#[derive(Clone, Default)]
pub struct Behavior {
    /// The response to a range request starting here is sent at
    /// `slow_bytes_per_sec`: one slow connection.
    pub slow_range_start: Option<u64>,
    pub slow_bytes_per_sec: u64,
    /// Serve the file only to a POST with exactly this body (at
    /// `bytes_per_sec`, when set); a GET gets an HTML error page with status
    /// 200, as many real sites send.
    pub post_only_body: Option<Vec<u8>>,
    /// Require HTTP Basic with this `user:password`: 401 without it.
    pub basic_auth: Option<String>,
    /// Require NTLM with this login (`DOMAIN\user` or `user`) and password,
    /// offered as `NTLM` and `Negotiate`. The challenge is kept with the
    /// connection, so the answer must come on the same one.
    pub ntlm_auth: Option<(String, String)>,
    /// Answer every request with a 302 to this URL.
    pub redirect_to: Option<String>,
    /// Serve these files by path (such as `/v/1.ts`) instead of one body
    /// everywhere; other paths get 404.
    pub files: Option<Arc<HashMap<String, Vec<u8>>>>,
    /// Send this `Last-Modified` header with the file.
    pub last_modified: Option<String>,

    // Faults.
    /// Close each response's connection after this many bytes of its body,
    /// as a connection dropped partway does.
    pub drop_after: Option<u64>,
    /// Answer at most this many connections from one client (one address)
    /// at once; the others get 503 Service Unavailable, as servers that
    /// limit connections per client send.
    pub max_per_client: Option<usize>,
    /// Wait this long before answering each request: a slow start.
    pub slow_start: Option<Duration>,
    /// Answer the first this many range requests other than the one-byte
    /// probe with a 206 for a range that starts 1 KiB away from the one
    /// asked for, as a broken proxy or cache can.
    pub shifted_ranges: u32,
    /// After those, answer the next this many range requests other than the
    /// probe with 200 and the whole file, as a server that ignores some
    /// range requests does.
    pub ignored_ranges: u32,
    /// Send every response at this many bytes a second: a server that
    /// limits each connection's speed. `slow_range_start` overrides it for
    /// its one range.
    pub bytes_per_sec: Option<u64>,
}

pub struct TestServer {
    pub url: String,
    /// Body bytes sent so far, across every response.
    pub served: Arc<AtomicU64>,
    /// Every `Authorization` header received.
    pub auth_seen: Arc<Mutex<Vec<String>>>,
    /// The path and `Cookie` header of every request that sent one.
    pub cookies_seen: Arc<Mutex<Vec<(String, String)>>>,
    /// Connections refused with 503 for going over `max_per_client`.
    pub refused: Arc<AtomicU64>,
    /// The most connections from one client answered at once.
    pub peak_per_client: Arc<AtomicUsize>,
    /// Connections accepted so far.
    pub connections: Arc<AtomicU64>,
}

/// What every connection's thread shares.
#[derive(Clone)]
struct Seen {
    served: Arc<AtomicU64>,
    auth_seen: Arc<Mutex<Vec<String>>>,
    cookies_seen: Arc<Mutex<Vec<(String, String)>>>,
    refused: Arc<AtomicU64>,
    peak_per_client: Arc<AtomicUsize>,
    /// Connections being answered, by client address.
    open: Arc<Mutex<HashMap<IpAddr, usize>>>,
    /// Range requests answered so far, other than the probe.
    ranges: Arc<AtomicU32>,
}

/// One client's place among its connections being answered; the count goes
/// down when it is dropped.
struct Slot {
    open: Arc<Mutex<HashMap<IpAddr, usize>>>,
    client: IpAddr,
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(n) = self.open.lock().unwrap().get_mut(&self.client) {
            *n = n.saturating_sub(1);
        }
    }
}

impl Seen {
    /// A place for one more connection from `client`, or `None` when it
    /// already has `limit` being answered.
    fn take_slot(&self, client: IpAddr, limit: Option<usize>) -> Option<Slot> {
        let mut open = self.open.lock().unwrap();
        let n = open.entry(client).or_insert(0);
        if limit.is_some_and(|limit| *n >= limit) {
            return None;
        }
        *n += 1;
        self.peak_per_client.fetch_max(*n, Ordering::Relaxed);
        Some(Slot { open: Arc::clone(&self.open), client })
    }
}

/// How a range request is answered.
enum RangeAnswer {
    /// As asked.
    Right,
    /// With a 206 for another range.
    Shifted,
    /// With 200 and the whole file.
    Whole,
}

/// A body of `len` bytes that differs at every offset modulo 251, so a
/// misplaced range shows up in a comparison.
pub fn body(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

impl TestServer {
    pub fn start(body: Vec<u8>) -> TestServer {
        Self::start_with(body, Behavior::default())
    }

    pub fn start_with(body: Vec<u8>, behavior: Behavior) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.bin", listener.local_addr().unwrap());
        Self::serve_on(listener, url, body, behavior)
    }

    /// Serve `files` by path; `url` is the server's root, without the
    /// trailing slash.
    /// Like `start_files`, with the files made knowing the server's
    /// address (`http://127.0.0.1:PORT`), for answers that link to it.
    pub fn start_files_linking(files: impl FnOnce(&str) -> HashMap<String, Vec<u8>>) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let behavior = Behavior { files: Some(Arc::new(files(&url))), ..Behavior::default() };
        Self::serve_on(listener, url, Vec::new(), behavior)
    }

    pub fn start_files(files: HashMap<String, Vec<u8>>) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let behavior = Behavior { files: Some(Arc::new(files)), ..Behavior::default() };
        Self::serve_on(listener, url, Vec::new(), behavior)
    }

    fn serve_on(listener: TcpListener, url: String, body: Vec<u8>, behavior: Behavior) -> TestServer {
        let body = Arc::new(body);
        let seen = Seen {
            served: Arc::new(AtomicU64::new(0)),
            auth_seen: Arc::new(Mutex::new(Vec::new())),
            cookies_seen: Arc::new(Mutex::new(Vec::new())),
            refused: Arc::new(AtomicU64::new(0)),
            peak_per_client: Arc::new(AtomicUsize::new(0)),
            open: Arc::new(Mutex::new(HashMap::new())),
            ranges: Arc::new(AtomicU32::new(0)),
        };
        let server = TestServer {
            url,
            served: Arc::clone(&seen.served),
            auth_seen: Arc::clone(&seen.auth_seen),
            cookies_seen: Arc::clone(&seen.cookies_seen),
            refused: Arc::clone(&seen.refused),
            peak_per_client: Arc::clone(&seen.peak_per_client),
            connections: Arc::new(AtomicU64::new(0)),
        };
        let connections = Arc::clone(&server.connections);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                connections.fetch_add(1, Ordering::SeqCst);
                let body = Arc::clone(&body);
                let seen = seen.clone();
                let behavior = behavior.clone();
                thread::spawn(move || {
                    let _ = serve(stream, &body, &behavior, &seen, None);
                });
            }
        });
        server
    }
}

fn serve(
    mut stream: TcpStream,
    body: &[u8],
    behavior: &Behavior,
    seen: &Seen,
    challenged: Option<crate::ntlm::Challenge>,
) -> std::io::Result<()> {
    let served = &*seen.served;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut range: Option<(u64, u64)> = None;
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut content_length = 0usize;
    let mut auth: Option<String> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(len) = lower.strip_prefix("content-length:") {
            content_length = len.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("authorization:") {
            let value = line["authorization:".len()..].trim().to_string();
            seen.auth_seen.lock().unwrap().push(value.clone());
            auth = Some(value);
        }
        if lower.starts_with("cookie:") {
            let path = request_line.split(' ').nth(1).unwrap_or("/").to_string();
            seen.cookies_seen.lock().unwrap().push((path, line["cookie:".len()..].trim().to_string()));
        }
        if let Some(spec) = lower.strip_prefix("range: bytes=") {
            let mut parts = spec.trim().splitn(2, '-');
            let start: u64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
            let end: u64 = parts.next().and_then(|e| e.parse().ok()).unwrap_or(u64::MAX);
            range = Some((start, end));
        }
    }

    let mut sent_body = vec![0u8; content_length];
    reader.read_exact(&mut sent_body)?;
    // The request is read first, so the client gets the refusal rather than
    // a reset connection.
    let client = stream.peer_addr()?.ip();
    let Some(slot) = seen.take_slot(client, behavior.max_per_client) else {
        seen.refused.fetch_add(1, Ordering::Relaxed);
        return write!(stream, "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    };
    if let Some(wait) = behavior.slow_start {
        thread::sleep(wait);
    }
    let path = request_line.split(' ').nth(1).unwrap_or("/");
    let body: &[u8] = match &behavior.files {
        None => body,
        Some(files) => match files.get(path) {
            Some(b) => b,
            None => return write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
        },
    };
    let range = range.map(|(a, b)| (a, b.min((body.len() as u64).saturating_sub(1))));
    if let Some(pair) = &behavior.basic_auth {
        use base64::Engine as _;
        let expected = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair));
        if auth.as_deref() != Some(expected.as_str()) {
            return write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"files\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    }
    if let Some((login, password)) = &behavior.ntlm_auth {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let token = auth
            .as_deref()
            .and_then(|a| a.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("NTLM") || scheme.eq_ignore_ascii_case("Negotiate"))
            .and_then(|(scheme, t)| Some((scheme.to_string(), b64.decode(t).ok()?)));
        match (token, challenged) {
            (Some((scheme, t)), _) if t.get(8) == Some(&1) => {
                let c = crate::ntlm::test_challenge();
                let message = b64.encode(crate::ntlm::challenge_message(&c));
                write!(
                    stream,
                    "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: {scheme} {message}\r\nContent-Length: 0\r\n\r\n"
                )?;
                drop(reader);
                // The same connection goes on, so it keeps one place.
                drop(slot);
                return serve(stream, body, behavior, seen, Some(c));
            }
            (Some((_, t)), Some(c)) if crate::ntlm::check_authenticate(&c, &t, login, password) => {}
            _ => {
                return write!(
                    stream,
                    "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Negotiate\r\nWWW-Authenticate: NTLM\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
        }
    }
    if let Some(to) = &behavior.redirect_to {
        return write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
    }
    if let Some(expected) = &behavior.post_only_body {
        let ok = request_line.starts_with("POST ") && sent_body == *expected;
        if !ok {
            let page = b"<html><body>Your session has expired.</body></html>";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                page.len()
            )?;
            return stream.write_all(page);
        }
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        // At `bytes_per_sec` too, so a test can act while the answer comes.
        for chunk in body.chunks(16 * 1024) {
            served.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            stream.write_all(chunk)?;
            if let Some(rate) = behavior.bytes_per_sec {
                thread::sleep(Duration::from_secs_f64(chunk.len() as f64 / rate.max(1) as f64));
            }
        }
        return stream.flush();
    }

    let total = body.len() as u64;
    let probe = range == Some((0, 0));
    let answer = match range {
        Some(_) if !probe => {
            let n = seen.ranges.fetch_add(1, Ordering::Relaxed);
            if n < behavior.shifted_ranges {
                RangeAnswer::Shifted
            } else if n - behavior.shifted_ranges < behavior.ignored_ranges {
                RangeAnswer::Whole
            } else {
                RangeAnswer::Right
            }
        }
        _ => RangeAnswer::Right,
    };
    let slow = range.is_some_and(|(start, _)| Some(start) == behavior.slow_range_start);
    let (status, slice, mut extra) = match (range, answer) {
        (Some(_), RangeAnswer::Whole) | (None, _) => ("200 OK", body, String::new()),
        (Some((start, end)), answer) => {
            let start = match answer {
                RangeAnswer::Shifted if start >= 1024 => start - 1024,
                RangeAnswer::Shifted => (start + 1024).min(end),
                _ => start,
            };
            (
                "206 Partial Content",
                &body[start as usize..=end as usize],
                format!("Content-Range: bytes {start}-{end}/{total}\r\n"),
            )
        }
    };
    if let Some(date) = &behavior.last_modified {
        extra.push_str(&format!("Last-Modified: {date}\r\n"));
    }
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: \"v1\"\r\nAccept-Ranges: bytes\r\n{extra}Connection: close\r\n\r\n",
        slice.len()
    )?;
    let rate = if slow { Some(behavior.slow_bytes_per_sec) } else { behavior.bytes_per_sec };
    // Past `drop_after`, the rest of the body is never sent.
    let cut = behavior.drop_after.map_or(slice.len(), |n| slice.len().min(n as usize));
    let mut slot = Some(slot);
    let mut chunks = slice[..cut].chunks(16 * 1024).peekable();
    while let Some(chunk) = chunks.next() {
        // The client's place is given back before the last bytes go out: once
        // the client has them it may connect again at once, and that
        // connection must not be refused for this one.
        if chunks.peek().is_none() {
            slot.take();
        }
        // Count before sending: once the client has the bytes, a test may
        // read the count right away.
        served.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        stream.write_all(chunk)?;
        if let Some(rate) = rate {
            thread::sleep(Duration::from_secs_f64(chunk.len() as f64 / rate.max(1) as f64));
        }
    }
    stream.flush()?;
    if cut < slice.len() {
        // A dropped connection: the client sees the body end early.
        let _ = stream.shutdown(Shutdown::Both);
    }
    Ok(())
}
