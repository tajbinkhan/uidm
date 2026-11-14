//! HTTP concerns: building a TLS-capable client (through a proxy when one is
//! set), probing a URL for size and range support, fetching one byte range,
//! and answering a server that asks for a login.
//!
//! The client is reqwest's async one, so a download speaks HTTP/2 when the
//! server offers it (the segments of one download then share one connection
//! as streams). The engine keeps its plain threads: each step blocks on one
//! small shared tokio runtime, and a body is read through `Body`, which gives
//! up when no data comes for `READ_TIMEOUT`.

use std::io::{self, Read};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use udm_domain::bytes::ByteCount;

use super::EngineError;
use crate::net::{self, NetConfig};

/// How long a connection may take to open.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a response may go without sending anything: its headers, or
/// the next piece of its body. The download's retry takes over after that.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// The runtime every download's requests run on. Two workers are plenty:
/// they only move bytes between sockets and the engine's threads. An error
/// if the system would not start it (no threads or no event queue left);
/// the next request tries again.
pub(crate) fn runtime() -> Result<&'static tokio::runtime::Runtime, EngineError> {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let built = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("udm-http")
        .enable_all()
        .build()
        .map_err(|e| EngineError::Io(format!("the HTTP runtime did not start: {e}")))?;
    // Another thread may have set one meanwhile; then this one is dropped
    // and theirs is used.
    let _ = RUNTIME.set(built);
    RUNTIME.get().ok_or_else(|| EngineError::Io("the HTTP runtime did not start".into()))
}

/// Drop `value` inside the runtime, which a response's connection belongs
/// to. Without a runtime there is no response, so it is just dropped.
fn drop_in_runtime<T>(value: T) {
    let _guard = runtime().ok().map(|r| r.enter());
    drop(value);
}

/// Run `future` to its end on the runtime, giving up after `READ_TIMEOUT`.
fn block_on<T>(future: impl std::future::Future<Output = T>) -> Result<T, EngineError> {
    runtime()?
        .block_on(async { tokio::time::timeout(READ_TIMEOUT, future).await })
        .map_err(|_| EngineError::Http("timed out".into()))
}

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
    /// The `Content-Type` the server sends, as it sends it.
    pub content_type: Option<String>,
    /// The address of the server that answered, when reached directly (a
    /// proxy's address says nothing about the server's country).
    pub server: Option<std::net::IpAddr>,
}

impl Probe {
    /// What identifies this version of the file, for telling later whether
    /// the server's copy changed.
    pub fn validator(&self) -> udm_domain::download::Validator {
        udm_domain::download::Validator {
            size: self.total.map(ByteCount::new),
            etag: self.etag.clone(),
            last_modified: self.last_modified.clone(),
        }
    }
}

/// A response whose headers have arrived; its body is read with
/// `into_reader`.
struct Response {
    status: u16,
    url: String,
    headers: HeaderMap,
    /// The address the connection went to.
    remote: Option<std::net::SocketAddr>,
    /// Taken by `into_reader`.
    inner: Option<reqwest::Response>,
}

impl Response {
    fn status(&self) -> u16 {
        self.status
    }

    fn get_url(&self) -> &str {
        &self.url
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    fn all(&self, name: &str) -> Vec<String> {
        self.headers.get_all(name).iter().filter_map(|v| v.to_str().ok()).map(str::to_string).collect()
    }

    fn into_reader(mut self) -> Body {
        Body { inner: self.inner.take(), chunk: bytes::Bytes::new() }
    }

    /// Read and drop the body, so an HTTP/1.1 connection can carry the
    /// next request.
    fn drain(self) {
        let _ = io::copy(&mut self.into_reader().take(1 << 20), &mut io::sink());
    }
}

/// A response body as a blocking reader. Each read waits at most
/// `READ_TIMEOUT` for the next piece, so a stalled connection fails the
/// segment instead of hanging it.
pub struct Body {
    inner: Option<reqwest::Response>,
    chunk: bytes::Bytes,
}

impl Read for Body {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        while self.chunk.is_empty() {
            let Some(resp) = self.inner.as_mut() else {
                return Ok(0);
            };
            let runtime = runtime().map_err(io::Error::other)?;
            match runtime.block_on(async { tokio::time::timeout(READ_TIMEOUT, resp.chunk()).await }) {
                Ok(Ok(Some(chunk))) => self.chunk = chunk,
                Ok(Ok(None)) => {
                    self.close();
                    return Ok(0);
                }
                Ok(Err(e)) => {
                    return Err(match crate::route::adapter_down_in(&e) {
                        Some(down) => io::Error::other(down.clone()),
                        None => io::Error::other(error_text(&e)),
                    })
                }
                Err(_) => return Err(io::Error::new(io::ErrorKind::TimedOut, "timed out")),
            }
        }
        let n = out.len().min(self.chunk.len());
        out[..n].copy_from_slice(&self.chunk[..n]);
        bytes::Buf::advance(&mut self.chunk, n);
        Ok(n)
    }
}

impl Body {
    /// Drop the response inside the runtime, which its connection belongs to.
    fn close(&mut self) {
        if let Some(resp) = self.inner.take() {
            drop_in_runtime(resp);
        }
    }
}

impl Drop for Body {
    fn drop(&mut self) {
        self.close();
    }
}

impl Drop for Response {
    fn drop(&mut self) {
        if let Some(resp) = self.inner.take() {
            drop_in_runtime(resp);
        }
    }
}

/// Why a request got no usable response.
enum Failed {
    /// The server answered with this error status (400 and up).
    Status(u16, Box<Response>),
    /// No answer: the connection, TLS, the proxy or a timeout.
    Transport(EngineError),
}

/// An HTTP client for one download: TLS through the OS (SChannel on Windows
/// via native-tls), HTTP/2 or HTTP/1.1, bounded timeouts, the download's
/// proxy, plus the site logins it may answer a 401 with.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    /// A new client like `http` with a connection pool of its own, on
    /// HTTP/1.1, for an NTLM handshake: its steps must share one connection,
    /// which another segment's request could take from a shared pool, and
    /// NTLM does not work over HTTP/2. `OneConnection` holds back any
    /// connection after its first.
    fresh_http1: Arc<dyn Fn(OneConnection) -> Result<reqwest::Client, EngineError> + Send + Sync>,
    logins: Arc<Vec<net::Login>>,
    /// How to reach an `ftp://` or `ftps://` address, which goes to the FTP
    /// module.
    ftp: super::ftp::FtpOptions,
    /// Requests go through a proxy, whose address is not the server's.
    proxied: bool,
}

/// Most redirects followed.
const MAX_REDIRECTS: usize = 8;

/// The proxy URL `net` gives, as reqwest reads it. SOCKS5 lets the proxy
/// look up host names, which reqwest calls `socks5h`.
fn reqwest_proxy(proxy: &str) -> Result<reqwest::Proxy, EngineError> {
    let proxy = match proxy.get(..9) {
        Some(p) if p.eq_ignore_ascii_case("socks5://") => format!("socks5h://{}", &proxy[9..]),
        _ => proxy.to_string(),
    };
    reqwest::Proxy::all(&proxy).map_err(|e| EngineError::Http(format!("bad proxy setting: {e}")))
}

impl Client {
    /// A client for downloading `url` under `net`. The proxy is chosen for
    /// this URL's scheme and host; an HTTP proxy's login rides in its URL,
    /// and goes only to the proxy.
    pub fn new(net: &NetConfig, url: &str) -> Result<Self, EngineError> {
        let ftp = super::ftp::is_ftp(url);
        let proxy = match net.proxy.as_ref().filter(|_| !ftp).and_then(|rules| rules.proxy_for(url)) {
            Some(p) => Some(reqwest_proxy(&p)?),
            None => None,
        };
        let proxied = proxy.is_some();
        let route = net.route.clone();
        let make = move |ntlm: Option<OneConnection>| -> Result<reqwest::Client, EngineError> {
            let mut builder = reqwest::Client::builder()
                .use_native_tls()
                .connect_timeout(CONNECT_TIMEOUT)
                .read_timeout(READ_TIMEOUT)
                // Redirects are followed in `send`, which drops cookies and
                // logins on every hop.
                .redirect(reqwest::redirect::Policy::none());
            builder = match &proxy {
                Some(p) => builder.proxy(p.clone()),
                None => builder.no_proxy(),
            };
            if let Some(one) = ntlm {
                builder = builder.http1_only().connector_layer(one);
            }
            // Bound to the chosen adapter, each connection checking it first,
            // and with the chosen resolver.
            builder = route.client(builder)?;
            let _guard = runtime()?.enter();
            builder.build().map_err(|e| EngineError::Tls(error_text(&e)))
        };
        Ok(Client {
            http: make(None)?,
            fresh_http1: Arc::new(move |one| make(Some(one))),
            logins: Arc::new(net.logins.clone()),
            ftp: super::ftp::FtpOptions {
                active: net.ftp_active,
                logins: net.logins.clone(),
                proxy: net.ftp_proxy.clone(),
                proxy_login: net.ftp_proxy_login.clone(),
                // SOCKS for ftp:// only when no FTP proxy is set; ftps://
                // goes around the FTP proxy, so it can take SOCKS.
                socks: net
                    .proxy
                    .as_ref()
                    .filter(|_| ftp && (net.ftp_proxy.is_none() || super::ftp::is_ftps(url)))
                    .and_then(|rules| rules.socks_for_ftp(url)),
                route: net.route.clone(),
            },
            proxied,
        })
    }

    /// The address of the server that sent `resp`, unless it came through
    /// a proxy.
    fn server(&self, resp: &Response) -> Option<std::net::IpAddr> {
        resp.remote.filter(|_| !self.proxied).map(|a| a.ip())
    }

    /// Send a request and follow its redirects. Each hop drops `Cookie` and
    /// `Authorization`, so neither reaches a server it was not meant for;
    /// a 303 (or a 301 or 302 after a POST) turns the request into a GET.
    fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        range: Option<&str>,
        body: Option<&[u8]>,
    ) -> Result<Response, EngineError> {
        let (mut method, mut url, mut headers, mut body) =
            (method.to_string(), url.to_string(), headers.to_vec(), body);
        for _ in 0..=MAX_REDIRECTS {
            let resp = self.send_once(&method, &url, &headers, range, body)?;
            let status = resp.status();
            let location = resp.header("location").map(str::to_string);
            match location {
                Some(loc) if (300..400).contains(&status) && status != 304 => {
                    let next = url::Url::parse(&url)
                        .and_then(|base| base.join(&loc))
                        .map_err(|e| EngineError::Http(format!("bad redirect to {loc}: {e}")))?;
                    if !matches!(next.scheme(), "http" | "https") {
                        return Err(EngineError::Http(format!("redirect to an unsupported address: {next}")));
                    }
                    if status == 303 || (matches!(status, 301 | 302) && method == "POST") {
                        method = "GET".to_string();
                        body = None;
                    }
                    headers
                        .retain(|(k, _)| !k.eq_ignore_ascii_case("cookie") && !k.eq_ignore_ascii_case("authorization"));
                    url = next.to_string();
                    resp.drain();
                }
                _ => return Ok(resp),
            }
        }
        Err(EngineError::Http(format!("more than {MAX_REDIRECTS} redirects")))
    }

    /// Send one request. When the server answers 401, send it once more with
    /// the login for that URL's host.
    fn send_once(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        range: Option<&str>,
        body: Option<&[u8]>,
    ) -> Result<Response, EngineError> {
        let build_on = |http: &reqwest::Client, auth: Option<&str>| -> Result<Response, Failed> {
            let method = reqwest::Method::from_bytes(method.as_bytes())
                .map_err(|_| Failed::Transport(EngineError::Http(format!("bad method {method}"))))?;
            let mut map = header_map(headers);
            if let Some(range) = range.and_then(|r| HeaderValue::from_str(r).ok()) {
                map.insert(reqwest::header::RANGE, range);
            }
            if let Some(auth) = auth.and_then(|a| HeaderValue::from_str(a).ok()) {
                map.insert(reqwest::header::AUTHORIZATION, auth);
            }
            let mut req = http.request(method, url).headers(map);
            if let Some(b) = body {
                req = req.body(b.to_vec());
            }
            execute(req)
        };
        match build_on(&self.http, None) {
            Err(Failed::Status(401, resp)) => {
                let asked = resp.get_url().to_string();
                let host = udm_domain::site::host_of(&asked);
                let challenges = resp.all("www-authenticate");
                let realm = net::realm(&challenges);
                (*resp).drain();
                let needs = || EngineError::NeedsLogin(host.clone(), realm.clone());
                let login = net::find_login(&self.logins, &asked).ok_or_else(needs)?;
                if let Some(scheme) = net::ntlm_scheme(&challenges) {
                    let one = OneConnection::new(LATER_CONNECT_WAIT);
                    let http = (self.fresh_http1)(one.clone())?;
                    let build = |auth: Option<&str>| build_on(&http, auth);
                    let result = ntlm(scheme, login, &build);
                    one.finish();
                    return result.map_err(|e| match e {
                        NtlmError::Rejected => EngineError::LoginRejected(host, realm),
                        NtlmError::Other(e) => e,
                    });
                }
                let auth = net::authorization(&challenges, login, method, &asked, &cnonce()).ok_or_else(needs)?;
                // Basic sends the password as it is: over plain http it goes
                // only with a login allowed there, else the user is asked.
                // A server, or someone on the way, cannot downgrade a login
                // saved for https by offering only Basic.
                if auth.starts_with("Basic ") && !asked.starts_with("https:") && !login.plain {
                    return Err(needs());
                }
                match build_on(&self.http, Some(&auth)) {
                    Err(Failed::Status(401, _)) => Err(EngineError::LoginRejected(host, realm)),
                    other => other.map_err(failed),
                }
            }
            other => other.map_err(failed),
        }
    }
}

/// Send a request and wait for its headers.
fn execute(req: reqwest::RequestBuilder) -> Result<Response, Failed> {
    // The future is made inside the runtime: reqwest starts its timers
    // as soon as it is made.
    let inner = match block_on(async move { req.send().await }) {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => return Err(Failed::Transport(transport(&e))),
        Err(e) => return Err(Failed::Transport(e)),
    };
    let resp = Response {
        status: inner.status().as_u16(),
        url: inner.url().to_string(),
        headers: inner.headers().clone(),
        remote: inner.remote_addr(),
        inner: Some(inner),
    };
    if resp.status >= 400 {
        return Err(Failed::Status(resp.status, Box::new(resp)));
    }
    Ok(resp)
}

/// The request's headers as reqwest takes them. A name or value that is not
/// valid HTTP is left out rather than failing the download.
fn header_map(headers: &[(String, String)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(value)) {
            map.insert(name, value);
        }
    }
    map
}

/// A request that got no answer, as the engine's error: the network
/// adapter being down when that is why, so the download stops for good.
fn transport(e: &reqwest::Error) -> EngineError {
    match crate::route::adapter_down_in(e) {
        Some(down) => EngineError::AdapterDown(down.clone()),
        None => EngineError::Http(error_text(e)),
    }
}

/// A reqwest error in words, with its causes: "error sending request"
/// alone says nothing about the refused connection under it.
pub(crate) fn error_text(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        let more = cause.to_string();
        if !text.contains(&more) {
            text.push_str(": ");
            text.push_str(&more);
        }
        source = cause.source();
    }
    text
}

fn failed(f: Failed) -> EngineError {
    match f {
        Failed::Status(code, resp) => {
            (*resp).drain();
            EngineError::Status(code)
        }
        Failed::Transport(e) => e,
    }
}

/// Sends the request with an `Authorization` header, for the NTLM
/// handshake.
type SendWith<'a> = &'a dyn Fn(Option<&str>) -> Result<Response, Failed>;

/// Why an NTLM handshake did not get through.
enum NtlmError {
    /// The server refused the login.
    Rejected,
    Other(EngineError),
}

/// The NTLM handshake under `scheme` (`NTLM` or `Negotiate`): a negotiate
/// message, the server's challenge in its 401, then the answer. The
/// challenge's body is read to the end so the connection goes back to the
/// pool and the answer goes on it, as NTLM requires. `build` sends the
/// request with an `Authorization` header, on a client of its own.
fn ntlm(scheme: &str, login: &net::Login, build: SendWith) -> Result<Response, NtlmError> {
    use base64::Engine as _;
    let b64 = |m: Vec<u8>| base64::engine::general_purpose::STANDARD.encode(m);
    let first = format!("{scheme} {}", b64(crate::ntlm::negotiate()));
    let challenge = match build(Some(&first)) {
        Err(Failed::Status(401, resp)) => {
            let challenges = resp.all("www-authenticate");
            (*resp).drain();
            net::ntlm_challenge(&challenges, scheme).ok_or(NtlmError::Rejected)?
        }
        // Let in without the rest of the handshake.
        other => return other.map_err(|e| NtlmError::Other(failed(e))),
    };
    let answer = crate::ntlm::authenticate(&challenge, &login.username, &login.password, &crate::ntlm::workstation());
    match build(Some(&format!("{scheme} {}", b64(answer)))) {
        Err(Failed::Status(401, _)) => Err(NtlmError::Rejected),
        other => other.map_err(|e| NtlmError::Other(failed(e))),
    }
}

/// How long a connection after the first waits on an NTLM client. The
/// handshake's first connection goes back to the pool microseconds after its
/// challenge is read, so this only matters when the server closed it.
const LATER_CONNECT_WAIT: Duration = Duration::from_secs(3);

/// A connector layer for an NTLM client that keeps the handshake on one
/// connection. The pool picks whichever comes first, a connection put back
/// in it or a new one, so on a busy machine a new connection could win, and
/// the answer would reach the server on a connection that never got the
/// challenge, which the server refuses. Here only the first connection opens
/// at once. Any later one waits `wait` and opens only if the handshake is
/// still going by then, so the pooled connection wins.
#[derive(Clone)]
struct OneConnection {
    state: Arc<OneConnectionState>,
    wait: Duration,
}

#[derive(Default)]
struct OneConnectionState {
    started: std::sync::atomic::AtomicBool,
    finished: std::sync::atomic::AtomicBool,
}

impl OneConnection {
    fn new(wait: Duration) -> Self {
        OneConnection { state: Arc::default(), wait }
    }

    /// The handshake is over: a connection still waiting fails instead of
    /// opening.
    fn finish(&self) {
        self.state.finished.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl<S> tower_layer::Layer<S> for OneConnection {
    type Service = OneConnectionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        OneConnectionService { inner, one: self.clone() }
    }
}

#[derive(Clone)]
struct OneConnectionService<S> {
    inner: S,
    one: OneConnection,
}

type BoxFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

impl<S, R> tower_service::Service<R> for OneConnectionService<S>
where
    S: tower_service::Service<R> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: From<&'static str>,
    R: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<Result<S::Response, S::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), S::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: R) -> Self::Future {
        use std::sync::atomic::Ordering::SeqCst;
        // The service that was made ready is the one called, as tower asks.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let state = self.one.state.clone();
        let wait = self.one.wait;
        Box::pin(async move {
            if state.started.swap(true, SeqCst) {
                tokio::time::sleep(wait).await;
                if state.finished.load(SeqCst) {
                    return Err(S::Error::from("the NTLM handshake already has its connection"));
                }
            }
            inner.call(req).await
        })
    }
}

/// A fresh client nonce for Digest authentication.
fn cnonce() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
    let suggested_filename = resp.header("content-disposition").and_then(parse_content_disposition_filename);
    let content_type = resp.header("content-type").map(str::to_string);
    let status = resp.status();
    let server = client.server(&resp);

    if status == 206 {
        let total = resp.header("content-range").and_then(parse_content_range_total);
        Ok(Probe { final_url, total, resumable: true, etag, last_modified, suggested_filename, content_type, server })
    } else {
        // 200 (or anything else 2xx): no partial content. Read the declared
        // length if present, then let `resp` drop without consuming the body.
        let total = resp.header("content-length").and_then(|s| s.trim().parse::<u64>().ok());
        Ok(Probe { final_url, total, resumable: false, etag, last_modified, suggested_filename, content_type, server })
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
        return Err(EngineError::Protocol(format!("expected 206 for range {range}, got {}", resp.status())));
    }
    // A proxy or server that answers with another range would put its bytes
    // at the wrong place in the file.
    let answered = resp.header("content-range").unwrap_or_default().to_string();
    if parse_content_range_start(&answered) != Some(start) {
        return Err(EngineError::Protocol(format!("asked for range {range}, but the server sent \"{answered}\"")));
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
        total: resp.header("content-length").and_then(|s| s.trim().parse::<u64>().ok()),
        resumable: false,
        etag: None,
        last_modified: None,
        suggested_filename: resp.header("content-disposition").and_then(parse_content_disposition_filename),
        content_type: resp.header("content-type").map(str::to_string),
        server: client.server(&resp),
    };
    Ok((probe, Box::new(resp.into_reader())))
}

/// Send one request to a web service's API and read its answer, an error
/// status's too (APIs explain an error in its body): the status and at most
/// `limit` bytes of the body. Redirects are not followed and no site login
/// answers a 401; `headers` carry the service's own credentials.
pub fn call(
    client: &Client,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    limit: u64,
) -> Result<(u16, Vec<u8>), EngineError> {
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| EngineError::Http(format!("bad method {method}")))?;
    let mut req = client.http.request(method, url).headers(header_map(headers));
    if let Some(b) = body {
        req = req.body(b.to_vec());
    }
    let resp = match execute(req) {
        Ok(resp) => resp,
        Err(Failed::Status(_, resp)) => *resp,
        Err(Failed::Transport(e)) => return Err(e),
    };
    let status = resp.status();
    let mut answer = Vec::new();
    resp.into_reader().take(limit).read_to_end(&mut answer)?;
    Ok((status, answer))
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

/// What `get_sized` returns: the final URL, the body's length when the
/// server gives it, and the body.
pub type GotSized = (String, Option<u64>, Box<dyn Read + Send>);

/// Send a GET, with the body's length when the server gives it.
pub fn get_sized(client: &Client, url: &str, headers: &[(String, String)]) -> Result<GotSized, EngineError> {
    let resp = client.send("GET", url, headers, None, None)?;
    let final_url = resp.get_url().to_string();
    let length = resp.header("content-length").and_then(|v| v.trim().parse::<u64>().ok());
    Ok((final_url, length, Box::new(resp.into_reader())))
}

/// Fetch the whole body in one stream, for servers without range support.
pub fn fetch_whole(
    client: &Client,
    url: &str,
    headers: &[(String, String)],
) -> Result<Box<dyn Read + Send>, EngineError> {
    if super::ftp::is_ftp(url) {
        return super::ftp::fetch(url, &client.ftp, 0, None);
    }
    let resp = client.send("GET", url, headers, None, None)?;
    Ok(Box::new(resp.into_reader()))
}

/// Parse the total length out of a `Content-Range: bytes 0-0/12345` header.
/// Returns `None` for an unknown total (`.../*`).
/// The first byte of a `Content-Range: bytes 100-199/1000` value.
fn parse_content_range_start(value: &str) -> Option<u64> {
    let range = value.trim().strip_prefix("bytes")?.trim_start();
    range.split('-').next()?.trim().parse::<u64>().ok()
}

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

#[cfg(test)]
mod tests {
    use super::{parse_content_disposition_filename, parse_content_range_start, parse_content_range_total};

    /// Against a real HTTPS server, which needs the network: run with
    /// `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn a_real_server_is_reached_over_http2() {
        let client = super::Client::new(&Default::default(), "https://www.cloudflare.com/").unwrap();
        let resp = client.send("GET", "https://www.cloudflare.com/", &[], Some("bytes=0-0"), None).unwrap();
        assert_eq!(resp.inner.as_ref().unwrap().version(), reqwest::Version::HTTP_2);
        // Two requests on one client share the connection as streams.
        let again = client.send("GET", "https://www.cloudflare.com/", &[], Some("bytes=0-0"), None).unwrap();
        assert_eq!(again.inner.as_ref().unwrap().version(), reqwest::Version::HTTP_2);
    }

    /// A connector that counts how many connections it opens.
    #[derive(Clone, Default)]
    struct CountingConnector(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl tower_service::Service<()> for CountingConnector {
        type Response = ();
        type Error = Box<dyn std::error::Error + Send + Sync>;
        type Future = std::future::Ready<Result<(), Self::Error>>;

        fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: ()) -> Self::Future {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::ready(Ok(()))
        }
    }

    #[test]
    fn an_ntlm_client_opens_a_second_connection_only_after_a_wait() {
        use std::sync::atomic::Ordering::SeqCst;
        use std::time::{Duration, Instant};
        use tower_layer::Layer;
        use tower_service::Service;

        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let wait = Duration::from_millis(300);

        // The first connection opens at once, a later one after the wait.
        let opened = CountingConnector::default();
        let mut svc = super::OneConnection::new(wait).layer(opened.clone());
        let start = Instant::now();
        rt.block_on(svc.call(())).unwrap();
        assert!(start.elapsed() < wait);
        assert_eq!(opened.0.load(SeqCst), 1);
        rt.block_on(svc.call(())).unwrap();
        assert!(start.elapsed() >= wait);
        assert_eq!(opened.0.load(SeqCst), 2);

        // Once the handshake is over, a waiting connection never opens.
        let opened = CountingConnector::default();
        let one = super::OneConnection::new(wait);
        let mut svc = one.clone().layer(opened.clone());
        rt.block_on(svc.call(())).unwrap();
        let later = svc.call(());
        one.finish();
        assert!(rt.block_on(later).is_err());
        assert_eq!(opened.0.load(SeqCst), 1);
    }

    #[test]
    fn content_range_start() {
        assert_eq!(parse_content_range_start("bytes 100-199/1000"), Some(100));
        assert_eq!(parse_content_range_start("bytes 0-0/*"), Some(0));
        assert_eq!(parse_content_range_start("  bytes 5-9/10 "), Some(5));
        // Unsatisfiable or missing ranges have no start.
        assert_eq!(parse_content_range_start("bytes */1000"), None);
        assert_eq!(parse_content_range_start(""), None);
        assert_eq!(parse_content_range_start("items 0-1/2"), None);
    }

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
        assert_eq!(parse_content_disposition_filename("attachment; filename=plain.txt"), Some("plain.txt".to_string()));
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
