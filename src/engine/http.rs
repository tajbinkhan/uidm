//! HTTP concerns: building a TLS-capable client (through a proxy when one is
//! set), probing a URL for size and range support, fetching one byte range,
//! and answering a server that asks for a login.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use super::EngineError;
use crate::net::{self, NetConfig};

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

/// A blocking HTTP client for one download: an agent that validates TLS
/// through the OS (SChannel on Windows via native-tls), with bounded timeouts,
/// going through the download's proxy, plus the site logins it may answer a
/// 401 with.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    logins: Arc<Vec<net::Login>>,
    /// `Proxy-Authorization` for plain http requests through an HTTP proxy
    /// with a login. ureq sends proxy credentials only when it tunnels
    /// (https), so for these the client adds them itself, and then follows
    /// redirects itself too (`agent` does not), so the header never rides a
    /// redirect to another server.
    proxy_basic: Option<String>,
    /// How to reach an `ftp://` address, which goes to the FTP module.
    ftp: super::ftp::FtpOptions,
}

/// Most redirects followed by hand.
const MAX_REDIRECTS: usize = 8;

impl Client {
    /// A client for downloading `url` under `net`. The proxy is chosen for
    /// this URL's scheme and host.
    pub fn new(net: &NetConfig, url: &str) -> Result<Self, EngineError> {
        let connector = native_tls::TlsConnector::new().map_err(|e| EngineError::Tls(e.to_string()))?;
        let mut builder = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(60))
            .tls_connector(Arc::new(connector));
        let mut proxy_basic = None;
        let ftp = super::ftp::is_ftp(url);
        if let Some(rules) = net.proxy.as_ref().filter(|_| !ftp) {
            if let Some(proxy) = rules.proxy_for(url) {
                if proxy.starts_with("http://") && !rules.username.is_empty() {
                    use base64::Engine as _;
                    let pair = format!("{}:{}", rules.username, rules.password);
                    proxy_basic = Some(format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair)));
                    builder = builder.redirects(0);
                }
                let proxy = ureq::Proxy::new(&proxy).map_err(|e| EngineError::Http(format!("bad proxy setting: {e}")))?;
                builder = builder.proxy(proxy);
            }
        }
        Ok(Client {
            agent: builder.build(),
            logins: Arc::new(net.logins.clone()),
            proxy_basic,
            ftp: super::ftp::FtpOptions {
                active: net.ftp_active,
                logins: net.logins.clone(),
                proxy: net.ftp_proxy.clone(),
            },
        })
    }

    /// Send a request, following redirects by hand when `proxy_basic` is set
    /// (see there). Each hop drops `Cookie` and `Authorization` as ureq does.
    fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        range: Option<&str>,
        body: Option<&[u8]>,
    ) -> Result<ureq::Response, EngineError> {
        let Some(proxy_auth) = &self.proxy_basic else {
            return self.send_once(method, url, headers, range, body, None);
        };
        let (mut method, mut url, mut headers, mut body) = (method.to_string(), url.to_string(), headers.to_vec(), body);
        for _ in 0..=MAX_REDIRECTS {
            let http = url.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("http://"));
            let resp = self.send_once(&method, &url, &headers, range, body, http.then_some(proxy_auth.as_str()))?;
            let status = resp.status();
            let location = resp.header("location").map(str::to_string);
            match location {
                Some(loc) if (300..400).contains(&status) && status != 304 => {
                    let next = url::Url::parse(&url)
                        .and_then(|base| base.join(&loc))
                        .map_err(|e| EngineError::Http(format!("bad redirect to {loc}: {e}")))?;
                    if matches!(status, 301..=303) && method == "POST" {
                        method = "GET".to_string();
                        body = None;
                    }
                    headers.retain(|(k, _)| !k.eq_ignore_ascii_case("cookie") && !k.eq_ignore_ascii_case("authorization"));
                    url = next.to_string();
                }
                _ => return Ok(resp),
            }
        }
        Err(EngineError::Http(format!("more than {MAX_REDIRECTS} redirects")))
    }

    /// Send one request. When the server answers 401, send it once more with
    /// the login for the host that asked, straight to the URL that asked.
    /// ureq drops `Authorization` on every redirect, so a login never follows
    /// one to another host.
    // ureq's error is large, but it lives only between the send and the
    // match below.
    #[allow(clippy::result_large_err)]
    fn send_once(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        range: Option<&str>,
        body: Option<&[u8]>,
        proxy_auth: Option<&str>,
    ) -> Result<ureq::Response, EngineError> {
        let build = |url: &str, auth: Option<&str>| {
            let mut r = apply_headers(self.agent.request(method, url), headers);
            if let Some(p) = proxy_auth {
                r = r.set("Proxy-Authorization", p);
            }
            if let Some(range) = range {
                r = r.set("Range", range);
            }
            if let Some(auth) = auth {
                r = r.set("Authorization", auth);
            }
            match body {
                Some(b) => r.send_bytes(b),
                None => r.call(),
            }
        };
        match build(url, None) {
            Err(ureq::Error::Status(401, resp)) => {
                let asked = resp.get_url().to_string();
                let host = net::host_of(&asked);
                let challenges: Vec<String> = resp.all("www-authenticate").iter().map(|h| h.to_string()).collect();
                let login = net::find_login(&self.logins, &host).ok_or_else(|| EngineError::NeedsLogin(host.clone()))?;
                let auth = net::authorization(&challenges, login, method, &asked, &cnonce())
                    .ok_or_else(|| EngineError::NeedsLogin(host.clone()))?;
                match build(&asked, Some(&auth)) {
                    Err(ureq::Error::Status(401, _)) => Err(EngineError::LoginRejected(host)),
                    other => other.map_err(map_ureq),
                }
            }
            other => other.map_err(map_ureq),
        }
    }
}

/// A fresh client nonce for Digest authentication.
fn cnonce() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
pub fn probe(client: &Client, url: &str, headers: &[(String, String)]) -> Result<Probe, EngineError> {
    if super::ftp::is_ftp(url) {
        return super::ftp::probe(url, &client.ftp);
    }
    let resp = client.send("GET", url, headers, Some("bytes=0-0"), None)?;

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
    client: &Client,
    url: &str,
    headers: &[(String, String)],
    start: u64,
    end: u64,
) -> Result<Box<dyn Read + Send>, EngineError> {
    if super::ftp::is_ftp(url) {
        return super::ftp::fetch(url, &client.ftp, start, Some(end));
    }
    let range = format!("bytes={start}-{end}");
    let resp = client.send("GET", url, headers, Some(&range), None)?;
    if resp.status() != 206 {
        return Err(EngineError::Protocol(format!(
            "expected 206 for range {range}, got {}",
            resp.status()
        )));
    }
    Ok(Box::new(resp.into_reader()))
}

/// Send a form as a POST and return what the response says about the file
/// with a reader over its body. The response cannot be resumed.
pub fn post(
    client: &Client,
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<(Probe, Box<dyn Read + Send>), EngineError> {
    let resp = client.send("POST", url, headers, None, Some(body))?;
    let probe = Probe {
        final_url: resp.get_url().to_string(),
        total: resp
            .header("content-length")
            .and_then(|s| s.trim().parse::<u64>().ok()),
        resumable: false,
        etag: None,
        last_modified: None,
        suggested_filename: resp
            .header("content-disposition")
            .and_then(parse_content_disposition_filename),
    };
    Ok((probe, Box::new(resp.into_reader())))
}

/// A GET's URL after redirects, content type, and a reader over the body.
pub type Got = (String, Option<String>, Box<dyn Read + Send>);

/// Send a GET.
pub fn get(client: &Client, url: &str, headers: &[(String, String)]) -> Result<Got, EngineError> {
    let resp = client.send("GET", url, headers, None, None)?;
    let final_url = resp.get_url().to_string();
    let content_type = resp.header("content-type").map(str::to_string);
    Ok((final_url, content_type, Box::new(resp.into_reader())))
}

/// Fetch the whole body in one stream, for servers without range support.
pub fn fetch_whole(client: &Client, url: &str, headers: &[(String, String)]) -> Result<Box<dyn Read + Send>, EngineError> {
    if super::ftp::is_ftp(url) {
        return super::ftp::fetch(url, &client.ftp, 0, None);
    }
    let resp = client.send("GET", url, headers, None, None)?;
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
