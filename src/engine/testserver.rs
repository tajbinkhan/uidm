//! A tiny HTTP/1.1 server on loopback for engine tests. It serves one body at
//! every path, honors single `Range: bytes=a-b` requests with 206, sends an
//! ETag, and closes each connection after one response.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Serve the file only to a POST with exactly this body; a GET gets an
    /// HTML error page with status 200, as many real sites send.
    pub post_only_body: Option<Vec<u8>>,
    /// Require HTTP Basic with this `user:password`: 401 without it.
    pub basic_auth: Option<String>,
    /// Answer every request with a 302 to this URL.
    pub redirect_to: Option<String>,
    /// Serve these files by path (such as `/v/1.ts`) instead of one body
    /// everywhere; other paths get 404.
    pub files: Option<Arc<HashMap<String, Vec<u8>>>>,
}

pub struct TestServer {
    pub url: String,
    /// Body bytes sent so far, across every response.
    pub served: Arc<AtomicU64>,
    /// Every `Authorization` header received.
    pub auth_seen: Arc<Mutex<Vec<String>>>,
    /// The path and `Cookie` header of every request that sent one.
    pub cookies_seen: Arc<Mutex<Vec<(String, String)>>>,
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
    pub fn start_files(files: HashMap<String, Vec<u8>>) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let behavior = Behavior {
            files: Some(Arc::new(files)),
            ..Behavior::default()
        };
        Self::serve_on(listener, url, Vec::new(), behavior)
    }

    fn serve_on(listener: TcpListener, url: String, body: Vec<u8>, behavior: Behavior) -> TestServer {
        let body = Arc::new(body);
        let served = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&served);
        let auth_seen = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&auth_seen);
        let cookies_seen = Arc::new(Mutex::new(Vec::new()));
        let cookies = Arc::clone(&cookies_seen);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let body = Arc::clone(&body);
                let counter = Arc::clone(&counter);
                let seen = Arc::clone(&seen);
                let cookies = Arc::clone(&cookies);
                let behavior = behavior.clone();
                thread::spawn(move || {
                    let _ = serve(stream, &body, &counter, &behavior, &seen, &cookies);
                });
            }
        });
        TestServer {
            url,
            served,
            auth_seen,
            cookies_seen,
        }
    }
}

fn serve(
    mut stream: TcpStream,
    body: &[u8],
    served: &AtomicU64,
    behavior: &Behavior,
    auth_seen: &Mutex<Vec<String>>,
    cookies_seen: &Mutex<Vec<(String, String)>>,
) -> std::io::Result<()> {
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
            auth_seen.lock().unwrap().push(value.clone());
            auth = Some(value);
        }
        if lower.starts_with("cookie:") {
            let path = request_line.split(' ').nth(1).unwrap_or("/").to_string();
            cookies_seen.lock().unwrap().push((path, line["cookie:".len()..].trim().to_string()));
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
    let path = request_line.split(' ').nth(1).unwrap_or("/");
    let body: &[u8] = match &behavior.files {
        None => body,
        Some(files) => match files.get(path) {
            Some(b) => b,
            None => {
                return write!(
                    stream,
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
            }
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
        served.fetch_add(body.len() as u64, Ordering::Relaxed);
        return stream.write_all(body);
    }

    let total = body.len() as u64;
    let slow = range.is_some_and(|(start, _)| Some(start) == behavior.slow_range_start);
    let (status, slice, extra) = match range {
        Some((start, end)) => (
            "206 Partial Content",
            &body[start as usize..=end as usize],
            format!("Content-Range: bytes {start}-{end}/{total}\r\n"),
        ),
        None => ("200 OK", body, String::new()),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: \"v1\"\r\nAccept-Ranges: bytes\r\n{extra}Connection: close\r\n\r\n",
        slice.len()
    )?;
    for chunk in slice.chunks(16 * 1024) {
        // Count before sending: once the client has the bytes, a test may
        // read the count right away.
        served.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        stream.write_all(chunk)?;
        if slow {
            let rate = behavior.slow_bytes_per_sec.max(1);
            thread::sleep(Duration::from_secs_f64(chunk.len() as f64 / rate as f64));
        }
    }
    stream.flush()
}
