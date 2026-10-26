//! HTTP concerns: building a TLS-capable agent, probing a URL for size and
//! range support, and fetching one byte range.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use super::EngineError;

/// What a probe learned about a URL before downloading.
pub struct Probe {
    /// URL after any redirects; later range requests use this to skip the
    /// redirect round-trips.
    pub final_url: String,
    /// Total length in bytes, when the server reports it.
    pub total: Option<u64>,
    /// Whether the server honored a byte range (returned 206).
    pub resumable: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Filename from a `Content-Disposition` header, when the server sends one.
    pub suggested_filename: Option<String>,
}

/// Build a blocking agent that validates TLS through the OS (SChannel on
/// Windows via native-tls), with bounded connect and read timeouts.
pub fn build_agent() -> Result<ureq::Agent, EngineError> {
    let connector = native_tls::TlsConnector::new()
        .map_err(|e| EngineError::Tls(e.to_string()))?;
    Ok(ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .tls_connector(Arc::new(connector))
        .build())
}

fn apply_headers(mut req: ureq::Request, headers: &[(String, String)]) -> ureq::Request {
    for (name, value) in headers {
        req = req.set(name, value);
    }
    req
}

/// Probe with a one-byte range request. A 206 reply confirms range support and
/// carries the total size in `Content-Range`. A 200 reply means no range
/// support; we drop the body unread rather than download the whole file here.
pub fn probe(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(String, String)],
) -> Result<Probe, EngineError> {
    let req = apply_headers(agent.get(url), headers).set("Range", "bytes=0-0");
    let resp = req.call().map_err(map_ureq)?;

    let final_url = resp.get_url().to_string();
    let etag = resp.header("etag").map(str::to_string);
    let last_modified = resp.header("last-modified").map(str::to_string);
    let suggested_filename = resp
        .header("content-disposition")
        .and_then(parse_content_disposition_filename);
    let status = resp.status();

    if status == 206 {
        let total = resp
            .header("content-range")
            .and_then(parse_content_range_total);
        Ok(Probe { final_url, total, resumable: true, etag, last_modified, suggested_filename })
    } else {
        // 200 (or anything else 2xx): no partial content. Read the declared
        // length if present, then let `resp` drop without consuming the body.
        let total = resp
            .header("content-length")
            .and_then(|s| s.trim().parse::<u64>().ok());
        Ok(Probe { final_url, total, resumable: false, etag, last_modified, suggested_filename })
    }
}

/// Fetch one inclusive byte range, returning a reader over the body. Errors if
/// the server does not answer with 206 Partial Content.
pub fn fetch_range(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(String, String)],
    start: u64,
    end: u64,
) -> Result<Box<dyn Read + Send>, EngineError> {
    let range = format!("bytes={start}-{end}");
    let req = apply_headers(agent.get(url), headers).set("Range", &range);
    let resp = req.call().map_err(map_ureq)?;
    if resp.status() != 206 {
        return Err(EngineError::Protocol(format!(
            "expected 206 for range {range}, got {}",
            resp.status()
        )));
    }
    Ok(Box::new(resp.into_reader()))
}

/// Fetch the whole body in one stream, for servers without range support.
pub fn fetch_whole(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(String, String)],
) -> Result<Box<dyn Read + Send>, EngineError> {
    let resp = apply_headers(agent.get(url), headers).call().map_err(map_ureq)?;
    Ok(Box::new(resp.into_reader()))
}

/// Parse the total length out of a `Content-Range: bytes 0-0/12345` header.
/// Returns `None` for an unknown total (`.../*`).
fn parse_content_range_total(value: &str) -> Option<u64> {
    let total = value.rsplit('/').next()?.trim();
    if total == "*" {
        return None;
    }
    total.parse::<u64>().ok()
}

/// Extract a filename from a `Content-Disposition` header (RFC 6266).
///
/// Prefers the extended `filename*=charset'lang'pct-encoded` form over plain
/// `filename=`, decodes percent-escapes, and returns only the final path
/// component. Returns `None` when no filename parameter is present.
fn parse_content_disposition_filename(header: &str) -> Option<String> {
    let mut plain: Option<String> = None;
    let mut extended: Option<String> = None;

    for part in header.split(';') {
        let mut kv = part.splitn(2, '=');
        let key = kv.next().unwrap_or("").trim().to_ascii_lowercase();
        let value = kv.next().unwrap_or("").trim();
        match key.as_str() {
            "filename*" => extended = Some(value.to_string()),
            "filename" => plain = Some(value.to_string()),
            _ => {}
        }
    }

    // Extended form wins: UTF-8''%E2%82%ac -> the bytes after the second quote.
    if let Some(ext) = extended {
        let ext = ext.trim_matches('"');
        let mut segs = ext.splitn(3, '\'');
        let _charset = segs.next();
        let _lang = segs.next();
        if let Some(encoded) = segs.next() {
            let decoded = String::from_utf8_lossy(&percent_decode(encoded)).into_owned();
            return basename(&decoded);
        }
    }

    if let Some(plain) = plain {
        let unquoted = unquote(plain.trim());
        return basename(&unquoted);
    }

    None
}

/// Decode `%XX` escapes to bytes; leaves other bytes untouched.
fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Strip surrounding quotes and unescape `\"`/`\\` from a quoted-string value.
fn unquote(value: &str) -> String {
    let inner = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value);
    let mut out = String::with_capacity(inner.len());
    let mut escaped = false;
    for c in inner.chars() {
        if escaped {
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else {
            out.push(c);
        }
    }
    out
}

/// Keep only the final path component, guarding against a server that sneaks a
/// directory into the filename.
fn basename(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    if base.is_empty() {
        None
    } else {
        Some(base.to_string())
    }
}

/// Flatten a ureq error (transport or HTTP status) into our error type.
fn map_ureq(e: ureq::Error) -> EngineError {
    match e {
        ureq::Error::Status(code, _) => EngineError::Status(code),
        ureq::Error::Transport(t) => EngineError::Http(t.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_content_disposition_filename, parse_content_range_total};

    #[test]
    fn content_range_total() {
        assert_eq!(parse_content_range_total("bytes 0-0/12345"), Some(12345));
        assert_eq!(parse_content_range_total("bytes 0-499/1000"), Some(1000));
        assert_eq!(parse_content_range_total("bytes 0-0/*"), None);
        assert_eq!(parse_content_range_total("garbage"), None);
    }

    #[test]
    fn content_disposition_plain() {
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=\"file name.zip\""),
            Some("file name.zip".to_string())
        );
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=plain.txt"),
            Some("plain.txt".to_string())
        );
    }

    #[test]
    fn content_disposition_extended_wins_and_decodes() {
        // %E2%82%AC is the euro sign; %20 is a space.
        assert_eq!(
            parse_content_disposition_filename(
                "attachment; filename=\"fallback.pdf\"; filename*=UTF-8''%E2%82%AC%20rates.pdf"
            ),
            Some("€ rates.pdf".to_string())
        );
    }

    #[test]
    fn content_disposition_strips_path_and_missing() {
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=\"../../etc/passwd\""),
            Some("passwd".to_string())
        );
        assert_eq!(parse_content_disposition_filename("attachment"), None);
        assert_eq!(parse_content_disposition_filename("inline"), None);
    }
}
