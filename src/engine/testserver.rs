//! A tiny HTTP/1.1 server on loopback for engine tests. It serves one body at
//! every path, honors single `Range: bytes=a-b` requests with 206, sends an
//! ETag, and closes each connection after one response.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

pub struct TestServer {
    pub url: String,
    /// Body bytes sent so far, across every response.
    pub served: Arc<AtomicU64>,
}

/// A body of `len` bytes that differs at every offset modulo 251, so a
/// misplaced range shows up in a comparison.
pub fn body(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

impl TestServer {
    pub fn start(body: Vec<u8>) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.bin", listener.local_addr().unwrap());
        let body = Arc::new(body);
        let served = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&served);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let body = Arc::clone(&body);
                let counter = Arc::clone(&counter);
                thread::spawn(move || {
                    let _ = serve(stream, &body, &counter);
                });
            }
        });
        TestServer { url, served }
    }
}

fn serve(mut stream: TcpStream, body: &[u8], served: &AtomicU64) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut range: Option<(u64, u64)> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(spec) = lower.strip_prefix("range: bytes=") {
            let mut parts = spec.trim().splitn(2, '-');
            let start: u64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
            let end: u64 = parts
                .next()
                .and_then(|e| e.parse().ok())
                .unwrap_or(body.len() as u64 - 1);
            range = Some((start, end.min(body.len() as u64 - 1)));
        }
    }

    let total = body.len() as u64;
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
        stream.write_all(chunk)?;
        served.fetch_add(chunk.len() as u64, Ordering::Relaxed);
    }
    stream.flush()
}
