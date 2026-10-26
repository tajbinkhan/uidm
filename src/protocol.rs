//! Chrome native-messaging framing and message types.
//!
//! Framing: a 4-byte little-endian length, then that many bytes of UTF-8 JSON.
//! This is the browser's standard and is transport-agnostic, so your own
//! extension and host are free to use whatever JSON shapes you like. The types
//! below are UIDM's own contract, not a copy of any other product's protocol.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// A message sent by the browser extension to the host.
///
/// `op` selects the variant, e.g. `{"op":"add","url":"https://..."}`.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Incoming {
    /// Handshake the extension sends on connect.
    Hello {
        #[serde(default)]
        version: Option<String>,
    },
    /// Liveness check.
    Ping,
    /// A captured download to hand off to the engine.
    Add(Download),
    /// Cancel a running or queued download by the id from its `ack`.
    Cancel { id: u64 },
}

/// The payload a browser hand-off needs to reissue a request outside the page.
///
/// Some fields are not read yet: they are part of the wire contract and will be
/// consumed by the download engine, so we keep them parsed and silence the
/// dead-code warning until then.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Download {
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
    /// Request headers as name -> value. A map keeps lookup simple; switch to a
    /// `Vec<(String, String)>` if you need to preserve duplicate header names.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub cookies: Option<String>,
    #[serde(default)]
    pub referrer: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    /// Suggested filename, when the extension could derive one.
    #[serde(default)]
    pub filename: Option<String>,
}

/// A message the host sends back to the extension.
#[derive(Debug, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Outgoing {
    /// Reply to `hello`, announcing the host's identity.
    Hello { name: String, version: String },
    /// Reply to `ping`.
    Pong,
    /// Reply to `add`, confirming the download was accepted. `id` identifies the
    /// download in later events and in a `cancel`.
    Ack { id: u64, url: String, status: String },
    /// A download has begun; totals may be unknown for a non-ranged server.
    Started {
        id: u64,
        url: String,
        total: Option<u64>,
        resumable: bool,
        connections: usize,
    },
    /// Periodic progress for a running download.
    Progress {
        id: u64,
        url: String,
        done: u64,
        total: Option<u64>,
    },
    /// A download finished; `path` is the saved file.
    Done { id: u64, url: String, path: String },
    /// A download failed partway.
    Failed { id: u64, url: String, error: String },
    /// A download was canceled; its partial file and resume sidecar are kept.
    Canceled { id: u64, url: String },
    /// Any failure the extension should hear about.
    Error { message: String },
}

/// Read one framed message from `reader`.
///
/// Returns `Ok(None)` on a clean EOF at a frame boundary (the browser closed
/// the port), `Ok(Some(bytes))` for a complete frame, and an error for a
/// partial frame or an oversized length.
pub fn read_message<R: Read>(reader: &mut R, max_bytes: u32) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match read_exact_or_eof(reader, &mut len_buf)? {
        false => return Ok(None),
        true => {}
    }

    let len = u32::from_le_bytes(len_buf);
    if len > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message length {len} exceeds limit {max_bytes}"),
        ));
    }

    let mut body = vec![0u8; len as usize];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Serialize `message` to JSON and write it with the 4-byte length prefix.
pub fn write_message<W: Write>(writer: &mut W, message: &Outgoing) -> io::Result<()> {
    let json = serde_json::to_vec(message)?;
    let len: u32 = json
        .len()
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "reply exceeds u32 length"))?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&json)?;
    Ok(())
}

/// Fill `buf` fully. Returns `Ok(false)` if EOF happens before any byte is
/// read (a clean close), `Ok(true)` on success, and an error if EOF happens
/// part-way through (a truncated frame).
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(false);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream ended mid-frame",
                ));
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A frame written by `write_message` reads back with the same length.
    #[test]
    fn roundtrip_frame() {
        let mut out = Vec::new();
        write_message(&mut out, &Outgoing::Pong).unwrap();
        // 4-byte prefix must equal the JSON byte count.
        let len = u32::from_le_bytes(out[0..4].try_into().unwrap()) as usize;
        assert_eq!(len, out.len() - 4);

        let mut cur = Cursor::new(out);
        let body = read_message(&mut cur, 1024).unwrap().unwrap();
        assert_eq!(body, br#"{"op":"pong"}"#);
    }

    /// EOF at a frame boundary is a clean close, not an error.
    #[test]
    fn clean_eof() {
        let mut cur = Cursor::new(Vec::new());
        assert!(read_message(&mut cur, 1024).unwrap().is_none());
    }

    /// An oversized length prefix is rejected before allocating.
    #[test]
    fn rejects_oversized() {
        let mut data = 999_999u32.to_le_bytes().to_vec();
        data.push(0);
        let mut cur = Cursor::new(data);
        assert!(read_message(&mut cur, 1024).is_err());
    }

    /// An `add` message parses into the typed download.
    #[test]
    fn parse_add() {
        let raw = br#"{"op":"add","url":"https://example.com/f.zip","method":"GET"}"#;
        let msg: Incoming = serde_json::from_slice(raw).unwrap();
        match msg {
            Incoming::Add(d) => {
                assert_eq!(d.url, "https://example.com/f.zip");
                assert_eq!(d.method.as_deref(), Some("GET"));
            }
            _ => panic!("expected add"),
        }
    }
}
