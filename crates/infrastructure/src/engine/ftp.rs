//! FTP and FTPS downloads. Each fetch opens its own control connection, logs in
//! (with the address's user and password, else a matching site login, else
//! anonymously), switches to binary, and opens a data connection: passive by
//! default (EPSV, then PASV), or active (PORT or EPRT) when the settings say
//! so. The size comes from `SIZE`, the date from `MDTM`, and resuming and
//! parallel connections start each range with `REST`. Through an FTP proxy,
//! the control connection goes to the proxy, logs in to it first when it has
//! a login of its own, then logs in as `user@host`. Through a SOCKS proxy,
//! both the control and the data connections are tunnelled with SOCKS
//! CONNECT; data then always uses passive mode, since the server could not
//! reach UDM behind the proxy.
//!
//! FTPS: an `ftps://` address speaks TLS from the first byte (implicit TLS,
//! port 990 when it names none). An `ftp://` server that refuses a plain
//! login because it wants TLS (a 534, or a refusal that names TLS or SSL) is
//! connected to again with `AUTH TLS` (explicit TLS), and later connections
//! to it start that way; one that does not ask stays plain, as before. Over
//! TLS, `PBSZ 0` and `PROT P` ask for TLS data connections too, each one a
//! TLS client of its own once the server has answered `RETR`. The operating
//! system checks the server's certificate, as it does for https. A saved
//! login goes over TLS even when it is not allowed on plain connections. An
//! FTP proxy would be the TLS end instead of the server, so `ftps://` goes
//! around it (a SOCKS proxy still tunnels it), and an `ftp://` server behind
//! one is not asked again with TLS.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use udm_domain::sync::LockExt;

use super::http::Probe;
use super::EngineError;
use crate::net::{self, Login};

const TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// The longest FTP reply line read; real ones are well under a kilobyte.
const MAX_REPLY_LINE: u64 = 64 * 1024;

/// The port of an `ftps://` address that names none.
const IMPLICIT_TLS_PORT: u16 = 990;

/// Whether `url` is an FTP address: `ftp://` or `ftps://`.
pub fn is_ftp(url: &str) -> bool {
    udm_domain::batch::is_ftp_url(url)
}

/// Whether `url` is an `ftps://` address, TLS from the first byte.
pub fn is_ftps(url: &str) -> bool {
    url.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("ftps://"))
}

/// How a control connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Security {
    /// Plain FTP: `ftp://`, until the server asks for TLS.
    Plain,
    /// `AUTH TLS` on the plain connection, then TLS: an `ftp://` server
    /// that asked for it.
    Explicit,
    /// TLS from the first byte: `ftps://`.
    Implicit,
}

/// The `host:port` of each `ftp://` server that asked for TLS, so the next
/// connections to it start with `AUTH TLS` instead of being refused first.
static WANTS_TLS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// A control or data connection, plain or TLS.
enum Conn {
    Plain(TcpStream),
    Tls(Box<native_tls::TlsStream<TcpStream>>),
}

impl Conn {
    fn tcp(&self) -> &TcpStream {
        match self {
            Conn::Plain(s) => s,
            Conn::Tls(s) => s.get_ref(),
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(s) => s.read(buf),
            Conn::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(s) => s.write(buf),
            Conn::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Plain(s) => s.flush(),
            Conn::Tls(s) => s.flush(),
        }
    }
}

fn tls_error(e: impl std::fmt::Display) -> EngineError {
    EngineError::Tls(format!("the FTP server's TLS failed: {e}"))
}

/// A TLS client that checks the server's certificate and name against the
/// operating system's trusted roots, as https does.
fn connector() -> Result<native_tls::TlsConnector, EngineError> {
    native_tls::TlsConnector::new().map_err(tls_error)
}

/// Run the TLS handshake on `stream` as a client of `name`.
fn handshake(
    connector: &native_tls::TlsConnector,
    name: &str,
    stream: TcpStream,
) -> Result<native_tls::TlsStream<TcpStream>, EngineError> {
    connector.connect(name, stream).map_err(|e| match e {
        native_tls::HandshakeError::Failure(e) => tls_error(e),
        // The sockets block, so this does not happen.
        native_tls::HandshakeError::WouldBlock(_) => tls_error("the handshake stalled"),
    })
}

/// `stream` as a TLS connection to the server `name`.
fn secure(stream: TcpStream, name: &str) -> Result<Conn, EngineError> {
    #[cfg(not(test))]
    let tls = handshake(&connector()?, name, stream)?;
    // Tests trust the test server's own certificate and no other.
    #[cfg(test)]
    let tls = server::trusting_handshake(name, stream)?;
    Ok(Conn::Tls(Box::new(tls)))
}

/// Whether a refused login says the server takes logins only over TLS: a
/// 534 ("request denied for policy reasons"), or a refusal that names TLS,
/// SSL or encryption, as servers word it.
fn wants_tls(code: u16, text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    code == 534
        || (matches!(code, 421 | 500..=599) && ["tls", "ssl", "encrypt", "ftps"].iter().any(|w| text.contains(w)))
}

/// Why a connection failed: an error, or a server that wants TLS before it
/// takes a login.
enum Refusal {
    Failed(EngineError),
    WantsTls(EngineError),
}

impl From<EngineError> for Refusal {
    fn from(e: EngineError) -> Self {
        Refusal::Failed(e)
    }
}

impl Refusal {
    fn into_error(self) -> EngineError {
        match self {
            Refusal::Failed(e) | Refusal::WantsTls(e) => e,
        }
    }
}

/// What an FTP fetch needs besides the address.
#[derive(Clone, Default)]
pub struct FtpOptions {
    /// Open data connections from the server to UDM (PORT) instead of the
    /// other way round.
    pub active: bool,
    pub logins: Vec<Login>,
    /// `host:port` of an FTP proxy to go through (`ftp://` only; TLS would
    /// end at the proxy).
    pub proxy: Option<String>,
    /// The proxy's own login, if it wants one.
    pub proxy_login: net::ProxyLogin,
    /// A SOCKS proxy to tunnel every connection through, when there is no
    /// FTP proxy.
    pub socks: Option<net::SocksProxy>,
    /// The network adapter every connection is bound to, the data
    /// connections and the active-mode listener included, and where names
    /// are looked up.
    pub route: crate::route::Route,
}

/// A logged-in control connection.
struct Control {
    conn: BufReader<Conn>,
    host: String,
    /// The name the server's certificate must carry: its host, without an
    /// IPv6 address's brackets.
    tls_name: String,
    /// The SOCKS proxy the connections go through, with the server's host
    /// as the URL names it, for the data connections.
    socks: Option<(net::SocksProxy, String)>,
    /// Data connections are TLS too: the server took `PROT P`.
    protected: bool,
    /// How the data connections go out.
    route: crate::route::Route,
    /// The server's address, when connected to it directly rather than
    /// through a proxy.
    server: Option<IpAddr>,
}

/// An I/O error as the engine's, keeping a refusal because the network
/// adapter is down.
fn io(e: std::io::Error) -> EngineError {
    EngineError::from(e)
}

/// Whether `s` holds a character that would break an FTP command line.
fn has_control(s: &str) -> bool {
    s.contains(['\r', '\n', '\0'])
}

fn decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned()
}

impl Control {
    /// Connect to the server of `url` and log in. Returns the file's path on
    /// the server too.
    fn open(url: &str, opts: &FtpOptions) -> Result<(Control, String), EngineError> {
        let parsed = url::Url::parse(url).map_err(|e| EngineError::Protocol(format!("bad FTP address {url}: {e}")))?;
        let host = parsed.host_str().ok_or_else(|| EngineError::Protocol(format!("no host in {url}")))?.to_string();
        let implicit = parsed.scheme() == "ftps";
        let port = parsed.port().unwrap_or(if implicit { IMPLICIT_TLS_PORT } else { 21 });
        // TLS through an FTP proxy would end at the proxy, so ftps:// goes
        // around it.
        let proxy = opts.proxy.as_deref().filter(|_| !implicit);
        let server = format!("{}:{port}", host.to_ascii_lowercase());
        let security = if implicit {
            Security::Implicit
        } else if proxy.is_none() && WANTS_TLS.lock_or_recover().contains(&server) {
            Security::Explicit
        } else {
            Security::Plain
        };
        let at = Target { url, parsed: &parsed, host: &host, port, proxy };
        match Control::connect(&at, opts, security) {
            Ok(opened) => Ok(opened),
            Err(Refusal::WantsTls(_)) if security == Security::Plain && proxy.is_none() => {
                tracing::info!(server = %server, "the FTP server wants TLS; connecting again with AUTH TLS");
                let opened = Control::connect(&at, opts, Security::Explicit).map_err(Refusal::into_error)?;
                WANTS_TLS.lock_or_recover().insert(server);
                Ok(opened)
            }
            Err(refusal) => Err(refusal.into_error()),
        }
    }

    /// Connect to `at` secured as `security` asks, and log in.
    fn connect(at: &Target, opts: &FtpOptions, security: Security) -> Result<(Control, String), Refusal> {
        let Target { url, parsed, host, port, proxy } = *at;
        let host = host.to_string();
        // Through a proxy, the login names the server to reach.
        let socks = opts.socks.clone().filter(|_| proxy.is_none());
        let route = &opts.route;
        let target = proxy.map(|_| if port == 21 { host.clone() } else { format!("{host}:{port}") });
        let stream = match (proxy, &socks) {
            (Some(proxy), _) => route.connect_to(proxy, 21, CONNECT_TIMEOUT).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => EngineError::Io(format!("could not find the FTP proxy {proxy}")),
                _ => io(e),
            })?,
            // The SOCKS proxy finds the server.
            (None, Some(socks)) => socks.connect(route, &host, port, CONNECT_TIMEOUT).map_err(io)?,
            (None, None) => route.connect(&host, port, CONNECT_TIMEOUT).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => EngineError::Io(format!("could not find the FTP server {host}")),
                _ => io(e),
            })?,
        };
        let server = (proxy.is_none() && socks.is_none()).then(|| stream.peer_addr().ok().map(|a| a.ip())).flatten();
        stream.set_read_timeout(Some(TIMEOUT)).map_err(io)?;
        stream.set_write_timeout(Some(TIMEOUT)).map_err(io)?;
        let tls_name = match parsed.host() {
            Some(url::Host::Ipv6(ip)) => ip.to_string(),
            _ => host.clone(),
        };
        let mut c = Control {
            conn: BufReader::new(Conn::Plain(stream)),
            host: host.to_ascii_lowercase(),
            tls_name,
            socks: socks.map(|p| (p, host.clone())),
            protected: false,
            route: route.clone(),
            server,
        };
        if security == Security::Implicit {
            c = c.start_tls()?;
        }
        let (code, text) = c.reply()?;
        if code != 220 {
            return Err(EngineError::Protocol(format!("the FTP server said: {code} {text}")).into());
        }
        if security == Security::Explicit {
            let (code, text) = c.command("AUTH TLS")?;
            if code != 234 {
                return Err(EngineError::Protocol(format!("the FTP server would not start TLS: {code} {text}")).into());
            }
            c = c.start_tls()?;
        }
        let proxy_login = &opts.proxy_login;
        if target.is_some() && !proxy_login.is_empty() {
            if has_control(&proxy_login.username) || has_control(&proxy_login.password) {
                return Err(EngineError::Protocol("the FTP proxy's login has control characters in it".into()).into());
            }
            let (code, _) = c.command(&format!("USER {}", proxy_login.username))?;
            let (code, text) =
                if code == 331 { c.command(&format!("PASS {}", proxy_login.password))? } else { (code, String::new()) };
            if !matches!(code, 230 | 202) {
                return Err(EngineError::Protocol(format!("the FTP proxy refused its login: {code} {text}")).into());
            }
        }

        let tls = security != Security::Plain;
        let from_url = (!parsed.username().is_empty())
            .then(|| (decode(parsed.username()), decode(parsed.password().unwrap_or(""))));
        // FTP sends the password as it is, so a saved login goes over a
        // plain connection only if allowed there; TLS hides it.
        let saved = net::find_login(&opts.logins, url)
            .filter(|l| l.plain || tls)
            .map(|l| (l.username.clone(), l.password.clone()));
        let given = from_url.is_some() || saved.is_some();
        let (user, pass) = from_url.or(saved).unwrap_or_else(|| ("anonymous".into(), "udm@".into()));
        if has_control(&user) || has_control(&pass) {
            return Err(EngineError::Protocol("the FTP login has control characters in it".into()).into());
        }
        let user = match &target {
            Some(t) => format!("{user}@{t}"),
            None => user,
        };
        let wants = |code: u16, text: &str| {
            let refused = EngineError::Protocol(format!("the FTP server wants TLS: {code} {text}"));
            (!tls && wants_tls(code, text)).then_some(Refusal::WantsTls(refused))
        };
        let (code, text) = c.command(&format!("USER {user}"))?;
        if let Some(refusal) = wants(code, &text) {
            return Err(refusal);
        }
        let (code, text) = if code == 331 { c.command(&format!("PASS {pass}"))? } else { (code, text) };
        if let Some(refusal) = wants(code, &text) {
            return Err(refusal);
        }
        match code {
            230 | 202 => {}
            530 | 331 | 332 if given => return Err(EngineError::LoginRejected(c.host.clone(), None).into()),
            530 | 331 | 332 => return Err(EngineError::NeedsLogin(c.host.clone(), None).into()),
            other => return Err(EngineError::Protocol(format!("the FTP server refused the login ({other})")).into()),
        }
        if tls {
            // RFC 4217: PBSZ comes before PROT, and is 0 for TLS.
            c.command("PBSZ 0")?;
            let (code, text) = c.command("PROT P")?;
            c.protected = code == 200;
            if !c.protected {
                tracing::warn!(host = %c.host, code, text = %text, "the FTP server keeps its data connections plain");
            }
        }
        let (code, text) = c.command("TYPE I")?;
        if code != 200 {
            return Err(EngineError::Protocol(format!("the FTP server would not send binary: {code} {text}")).into());
        }
        let path = decode(parsed.path());
        if has_control(&path) {
            return Err(EngineError::Protocol("the FTP address has control characters in its path".into()).into());
        }
        Ok((c, path))
    }

    /// Switch the control connection to TLS.
    fn start_tls(self) -> Result<Control, EngineError> {
        // The server waits for the handshake, so nothing is buffered yet;
        // bytes that are came in plain and could have been injected on the
        // way, so they are not taken as part of the TLS session.
        if !self.conn.buffer().is_empty() {
            return Err(EngineError::Protocol("the FTP server sent more before TLS started".into()));
        }
        let Control { conn, host, tls_name, socks, protected, route, server } = self;
        let stream = match conn.into_inner() {
            Conn::Plain(stream) => stream,
            Conn::Tls(_) => return Err(EngineError::Protocol("TLS has already started".into())),
        };
        let conn = BufReader::new(secure(stream, &tls_name)?);
        Ok(Control { conn, host, tls_name, socks, protected, route, server })
    }

    /// One reply, joining the lines of a multi-line one. Each line is read up
    /// to `MAX_REPLY_LINE` bytes, so a server sending one endless line cannot
    /// fill the memory.
    fn reply(&mut self) -> Result<(u16, String), EngineError> {
        let mut line = String::new();
        if self.conn.by_ref().take(MAX_REPLY_LINE).read_line(&mut line).map_err(io)? == 0 {
            return Err(EngineError::Io("the FTP server closed the connection".into()));
        }
        let code: u16 = line
            .get(..3)
            .and_then(|c| c.parse().ok())
            .ok_or_else(|| EngineError::Protocol(format!("not an FTP reply: {}", line.trim())))?;
        let mut text = line.get(4..).unwrap_or("").trim().to_string();
        if line.as_bytes().get(3) == Some(&b'-') {
            let end = format!("{code} ");
            loop {
                let mut next = String::new();
                if self.conn.by_ref().take(MAX_REPLY_LINE).read_line(&mut next).map_err(io)? == 0 {
                    return Err(EngineError::Io("the FTP server closed the connection".into()));
                }
                if next.starts_with(&end) {
                    text = next[4..].trim().to_string();
                    break;
                }
            }
        }
        Ok((code, text))
    }

    fn command(&mut self, cmd: &str) -> Result<(u16, String), EngineError> {
        // A line break would end this command and start another.
        if has_control(cmd) {
            return Err(EngineError::Protocol("control characters in an FTP command".into()));
        }
        let conn = self.conn.get_mut();
        conn.write_all(format!("{cmd}\r\n").as_bytes()).map_err(io)?;
        conn.flush().map_err(io)?;
        self.reply()
    }

    /// Ask for a data connection; returns how to get it once a transfer is
    /// asked for.
    fn data(&mut self, active: bool) -> Result<Data, EngineError> {
        let peer = self.conn.get_ref().tcp().peer_addr().map_err(io)?;
        if active && self.socks.is_none() {
            let local = self.conn.get_ref().tcp().local_addr().map_err(io)?;
            let listener = self.route.listen(local.ip()).map_err(io)?;
            let port = listener.local_addr().map_err(io)?.port();
            let cmd = match local.ip() {
                IpAddr::V4(ip) => {
                    let o = ip.octets();
                    format!("PORT {},{},{},{},{},{}", o[0], o[1], o[2], o[3], port >> 8, port & 0xff)
                }
                IpAddr::V6(ip) => format!("EPRT |2|{ip}|{port}|"),
            };
            let (code, text) = self.command(&cmd)?;
            if code != 200 {
                return Err(EngineError::Protocol(format!("the FTP server refused active mode: {code} {text}")));
            }
            return Ok(Data::Listening(listener));
        }
        // The address the server gives is often wrong behind NAT; its port is
        // what matters, on the address the control connection reached.
        let (code, text) = self.command("EPSV")?;
        let port = if code == 229 {
            text.split('|').nth(3).and_then(|p| p.parse::<u16>().ok())
        } else {
            let (code, text) = self.command("PASV")?;
            (code == 227).then(|| pasv_port(&text)).flatten()
        };
        let port = port.ok_or_else(|| EngineError::Protocol(format!("the FTP server gave no passive port: {text}")))?;
        let stream = match &self.socks {
            // The control connection's peer is the proxy, so the data
            // connection names the server again.
            Some((proxy, host)) => proxy.connect(&self.route, host, port, CONNECT_TIMEOUT).map_err(io)?,
            None => self.route.connect_addr(SocketAddr::new(peer.ip(), port), CONNECT_TIMEOUT).map_err(io)?,
        };
        Ok(Data::Connected(stream))
    }
}

/// The port in a `227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)` reply.
fn pasv_port(text: &str) -> Option<u16> {
    let inner = text.split('(').nth(1)?.split(')').next()?;
    let n: Vec<u16> = inner.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    (n.len() == 6).then(|| n[4] * 256 + n[5])
}

/// Where a control connection goes: the address's server, and the FTP
/// proxy in between, if any.
#[derive(Clone, Copy)]
struct Target<'a> {
    url: &'a str,
    parsed: &'a url::Url,
    host: &'a str,
    port: u16,
    proxy: Option<&'a str>,
}

enum Data {
    Connected(TcpStream),
    Listening(TcpListener),
}

impl Data {
    fn stream(self) -> Result<TcpStream, EngineError> {
        let stream = match self {
            Data::Connected(s) => s,
            Data::Listening(l) => {
                l.set_nonblocking(true).map_err(io)?;
                let until = Instant::now() + CONNECT_TIMEOUT;
                loop {
                    match l.accept() {
                        Ok((s, _)) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        Err(e) => return Err(io(e)),
                    }
                }
            }
        };
        stream.set_nonblocking(false).map_err(io)?;
        stream.set_read_timeout(Some(TIMEOUT)).map_err(io)?;
        Ok(stream)
    }
}

/// A file being received; it keeps its control connection open while read.
struct Transfer {
    data: Conn,
    _control: Control,
}

impl Read for Transfer {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.data.read(buf)
    }
}

/// `MDTM`'s `YYYYMMDDHHMMSS` as an HTTP date, for the resume check and the
/// file's date.
fn mdtm_to_http_date(text: &str) -> Option<String> {
    let t = text.trim();
    let digits = t.get(..14)?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let num = |a: usize, b: usize| digits[a..b].parse::<u32>().ok();
    let (y, mo, d, h, mi, s) = (num(0, 4)?, num(4, 6)?, num(6, 8)?, num(8, 10)?, num(10, 12)?, num(12, 14)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let day = udm_domain::queue::days_from_civil(y as i64, mo, d);
    let weekday = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][udm_domain::queue::weekday(day) as usize];
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][mo as usize - 1];
    Some(format!("{weekday}, {d:02} {month} {y:04} {h:02}:{mi:02}:{s:02} GMT"))
}

/// What the server says about the file: its size, whether it resumes
/// (`REST`), and its date.
pub fn probe(url: &str, opts: &FtpOptions) -> Result<Probe, EngineError> {
    let (mut c, path) = Control::open(url, opts)?;
    let (code, text) = c.command(&format!("SIZE {path}"))?;
    let total = (code == 213).then(|| text.trim().parse::<u64>().ok()).flatten();
    if code == 550 {
        return Err(EngineError::Status(404));
    }
    let resumable = total.is_some() && c.command("REST 0")?.0 == 350;
    let (code, text) = c.command(&format!("MDTM {path}"))?;
    let last_modified = (code == 213).then(|| mdtm_to_http_date(&text)).flatten();
    let _ = c.command("QUIT");
    Ok(Probe {
        final_url: url.to_string(),
        total,
        resumable,
        etag: None,
        last_modified,
        suggested_filename: None,
        content_type: None,
        server: c.server,
    })
}

/// Receive the file from byte `start`, through `end` (inclusive) when given.
pub fn fetch(url: &str, opts: &FtpOptions, start: u64, end: Option<u64>) -> Result<Box<dyn Read + Send>, EngineError> {
    let (mut c, path) = Control::open(url, opts)?;
    let data = c.data(opts.active)?;
    if start > 0 {
        let (code, text) = c.command(&format!("REST {start}"))?;
        if code != 350 {
            return Err(EngineError::Protocol(format!("the FTP server cannot resume: {code} {text}")));
        }
    }
    let (code, text) = c.command(&format!("RETR {path}"))?;
    match code {
        125 | 150 => {}
        550 => return Err(EngineError::Status(404)),
        _ => return Err(EngineError::Protocol(format!("the FTP server would not send the file: {code} {text}"))),
    }
    let data = data.stream()?;
    // The server starts its TLS end once it has answered RETR.
    let data = if c.protected { secure(data, &c.tls_name)? } else { Conn::Plain(data) };
    let transfer = Transfer { data, _control: c };
    Ok(match end {
        Some(end) => Box::new(transfer.take(end + 1 - start)),
        None => Box::new(transfer),
    })
}

/// A tiny FTP server for tests: one file, anonymous or one login, passive
/// and active mode, `SIZE`, `MDTM` and `REST`, and TLS as `Tls` says.
#[cfg(test)]
pub(crate) mod server {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread;

    /// The test servers' self-signed certificate for `127.0.0.1` and
    /// `localhost` and its PKCS#8 key, both PEM, made once per test run so
    /// no key is kept in the repository. Nothing else trusts it.
    pub fn identity() -> &'static (String, String) {
        static IDENTITY: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
        IDENTITY.get_or_init(make_identity)
    }

    /// An RSA key, which SChannel's `Identity::from_pkcs8` needs, signing
    /// its own certificate in pure Rust.
    fn make_identity() -> (String, String) {
        use rsa::pkcs1::EncodeRsaPublicKey;
        use rsa::pkcs8::EncodePrivateKey;
        use rsa::signature::{SignatureEncoding, Signer};

        struct Key {
            signer: rsa::pkcs1v15::SigningKey<rsa::sha2::Sha256>,
            public: Vec<u8>,
        }
        impl rcgen::PublicKeyData for Key {
            fn der_bytes(&self) -> &[u8] {
                &self.public
            }
            fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
                &rcgen::PKCS_RSA_SHA256
            }
        }
        impl rcgen::SigningKey for Key {
            fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
                Ok(self.signer.sign(msg).to_vec())
            }
        }

        let private = rsa::RsaPrivateKey::new(&mut rand_core::OsRng, 2048).unwrap();
        let key_pem = private.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap().to_string();
        let public = private.to_public_key().to_pkcs1_der().unwrap().as_bytes().to_vec();
        let key = Key { signer: rsa::pkcs1v15::SigningKey::new(private), public };
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()]).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, "UDM FTPS test server");
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        params.serial_number = Some(rcgen::SerialNumber::from(rand_core::RngCore::next_u64(&mut rand_core::OsRng)));
        (params.self_signed(&key).unwrap().pem(), key_pem)
    }

    /// How the server speaks TLS.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum Tls {
        /// Not at all: `AUTH TLS` is refused.
        #[default]
        Off,
        /// `AUTH TLS` when the client asks, plain logins too.
        Offered,
        /// Logins and data only over TLS (`AUTH TLS` and `PROT P`).
        Required,
        /// TLS from the first byte, as for `ftps://`.
        Implicit,
    }

    pub struct FtpServer {
        pub url: String,
        /// Data connections opened, so tests can tell parallel fetches.
        pub transfers: Arc<AtomicU64>,
        /// Data connections that were TLS.
        pub tls_transfers: Arc<AtomicU64>,
        /// `USER` commands that came over a plain control connection.
        pub plain_users: Arc<AtomicU64>,
        /// The `REST` offset of each `RETR`.
        pub rests: Arc<Mutex<Vec<u64>>>,
    }

    #[derive(Clone)]
    struct Shared {
        body: Arc<Vec<u8>>,
        login: Option<(&'static str, &'static str)>,
        tls: Tls,
        acceptor: Option<native_tls::TlsAcceptor>,
        transfers: Arc<AtomicU64>,
        tls_transfers: Arc<AtomicU64>,
        plain_users: Arc<AtomicU64>,
        rests: Arc<Mutex<Vec<u64>>>,
    }

    /// Serve `body` as `/pub/file.bin`; with `login`, only to that user.
    pub fn start(body: Vec<u8>, login: Option<(&'static str, &'static str)>) -> FtpServer {
        start_with(body, login, Tls::Off)
    }

    /// `start`, speaking TLS as `tls` says. An implicit-TLS server's address
    /// is `ftps://`.
    pub fn start_with(body: Vec<u8>, login: Option<(&'static str, &'static str)>, tls: Tls) -> FtpServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = (tls != Tls::Off).then(|| {
            let (cert, key) = identity();
            let identity = native_tls::Identity::from_pkcs8(cert.as_bytes(), key.as_bytes()).unwrap();
            native_tls::TlsAcceptor::new(identity).unwrap()
        });
        let shared = Shared {
            body: Arc::new(body),
            login,
            tls,
            acceptor,
            transfers: Arc::default(),
            tls_transfers: Arc::default(),
            plain_users: Arc::default(),
            rests: Arc::default(),
        };
        let server = FtpServer {
            url: format!("{}://{addr}/pub/file.bin", if tls == Tls::Implicit { "ftps" } else { "ftp" }),
            transfers: Arc::clone(&shared.transfers),
            tls_transfers: Arc::clone(&shared.tls_transfers),
            plain_users: Arc::clone(&shared.plain_users),
            rests: Arc::clone(&shared.rests),
        };
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let shared = shared.clone();
                thread::spawn(move || {
                    let _ = session(stream, &shared);
                });
            }
        });
        server
    }

    /// Tests' TLS client: it takes the test server's certificate, and only
    /// that one, in place of the operating system's check.
    pub(crate) fn trusting_handshake(
        name: &str,
        stream: TcpStream,
    ) -> Result<native_tls::TlsStream<TcpStream>, EngineError> {
        let connector = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()
            .unwrap();
        let tls = handshake(&connector, name, stream)?;
        let theirs = tls.peer_certificate().ok().flatten().and_then(|c| c.to_der().ok());
        let ours = native_tls::Certificate::from_pem(identity().0.as_bytes()).unwrap().to_der().unwrap();
        if theirs != Some(ours) {
            return Err(EngineError::Tls("not the test server's certificate".into()));
        }
        Ok(tls)
    }

    fn accept(acceptor: Option<&native_tls::TlsAcceptor>, stream: TcpStream) -> std::io::Result<Conn> {
        let acceptor = acceptor.ok_or_else(|| std::io::Error::other("no TLS here"))?;
        let tls = acceptor.accept(stream).map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(Conn::Tls(Box::new(tls)))
    }

    fn say(conn: &mut BufReader<Conn>, s: &str) -> std::io::Result<()> {
        conn.get_mut().write_all(format!("{s}\r\n").as_bytes())?;
        conn.get_mut().flush()
    }

    fn session(stream: TcpStream, sh: &Shared) -> std::io::Result<()> {
        let body = sh.body.as_slice();
        let first = if sh.tls == Tls::Implicit { accept(sh.acceptor.as_ref(), stream)? } else { Conn::Plain(stream) };
        let mut conn = BufReader::new(first);
        say(&mut conn, "220-Welcome")?;
        say(&mut conn, "220 test server")?;
        let (mut user, mut rest, mut passive, mut active) =
            (String::new(), 0u64, None::<TcpListener>, None::<SocketAddr>);
        let mut protect = false;
        loop {
            let mut line = String::new();
            if conn.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let line = line.trim_end();
            let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
            let secured = matches!(conn.get_ref(), Conn::Tls(_));
            match cmd.to_ascii_uppercase().as_str() {
                "AUTH" if matches!(sh.tls, Tls::Offered | Tls::Required) && !secured && arg == "TLS" => {
                    say(&mut conn, "234 starting TLS")?;
                    let Conn::Plain(tcp) = conn.into_inner() else { return Ok(()) };
                    conn = BufReader::new(accept(sh.acceptor.as_ref(), tcp)?);
                }
                "AUTH" => say(&mut conn, "502 no TLS here")?,
                "PBSZ" if secured => say(&mut conn, "200 PBSZ=0")?,
                "PROT" if secured => {
                    protect = arg == "P";
                    say(&mut conn, "200 protection set")?;
                }
                "PBSZ" | "PROT" => say(&mut conn, "503 TLS first")?,
                "USER" if !secured && sh.tls == Tls::Required => {
                    sh.plain_users.fetch_add(1, Ordering::Relaxed);
                    say(&mut conn, "530 Logins need TLS here: AUTH TLS first.")?;
                }
                "USER" => {
                    if !secured {
                        sh.plain_users.fetch_add(1, Ordering::Relaxed);
                    }
                    user = arg.to_string();
                    say(&mut conn, "331 password please")?;
                }
                "PASS" => match sh.login {
                    Some((u, p)) if (u, p) != (user.as_str(), arg) => say(&mut conn, "530 no")?,
                    _ => say(&mut conn, "230 in")?,
                },
                "TYPE" => say(&mut conn, "200 binary")?,
                "SIZE" if arg == "/pub/file.bin" => say(&mut conn, &format!("213 {}", body.len()))?,
                "MDTM" if arg == "/pub/file.bin" => say(&mut conn, "213 20151021072800")?,
                "SIZE" | "MDTM" => say(&mut conn, "550 no such file")?,
                "REST" => {
                    rest = arg.parse().unwrap_or(0);
                    say(&mut conn, "350 restarting")?;
                }
                "EPSV" => {
                    let l = TcpListener::bind("127.0.0.1:0")?;
                    say(&mut conn, &format!("229 Entering Extended Passive Mode (|||{}|)", l.local_addr()?.port()))?;
                    passive = Some(l);
                }
                "PORT" => {
                    let n: Vec<u16> = arg.split(',').filter_map(|s| s.parse().ok()).collect();
                    active =
                        Some(SocketAddr::from(([n[0] as u8, n[1] as u8, n[2] as u8, n[3] as u8], n[4] * 256 + n[5])));
                    say(&mut conn, "200 ok")?;
                }
                "RETR" if arg != "/pub/file.bin" => say(&mut conn, "550 no such file")?,
                "RETR" if sh.tls == Tls::Required && !protect => say(&mut conn, "521 data connections need TLS")?,
                "RETR" => {
                    let data = match (passive.take(), active.take()) {
                        (Some(l), _) => {
                            say(&mut conn, "150 sending")?;
                            l.accept()?.0
                        }
                        (None, Some(a)) => {
                            say(&mut conn, "150 sending")?;
                            TcpStream::connect(a)?
                        }
                        _ => {
                            say(&mut conn, "425 no data connection")?;
                            continue;
                        }
                    };
                    sh.transfers.fetch_add(1, Ordering::Relaxed);
                    sh.rests.lock().unwrap().push(rest);
                    let mut data = if protect {
                        sh.tls_transfers.fetch_add(1, Ordering::Relaxed);
                        accept(sh.acceptor.as_ref(), data)?
                    } else {
                        Conn::Plain(data)
                    };
                    let _ = data.write_all(&body[rest.min(body.len() as u64) as usize..]);
                    if let Conn::Tls(tls) = &mut data {
                        let _ = tls.shutdown();
                    }
                    drop(data);
                    rest = 0;
                    say(&mut conn, "226 done")?;
                }
                "QUIT" => {
                    say(&mut conn, "221 bye")?;
                    return Ok(());
                }
                _ => say(&mut conn, "502 not here")?,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn read_all(mut r: Box<dyn Read + Send>) -> Vec<u8> {
        let mut v = Vec::new();
        r.read_to_end(&mut v).unwrap();
        v
    }

    #[test]
    fn probes_size_resume_and_date() {
        let s = server::start(body(1000), None);
        let p = probe(&s.url, &FtpOptions::default()).unwrap();
        assert_eq!(p.total, Some(1000));
        assert!(p.resumable);
        assert_eq!(p.last_modified.as_deref(), Some("Wed, 21 Oct 2015 07:28:00 GMT"));
        let missing = s.url.replace("file.bin", "gone.bin");
        assert!(matches!(probe(&missing, &FtpOptions::default()), Err(EngineError::Status(404))));
    }

    #[test]
    fn fetches_whole_files_and_ranges_in_both_modes() {
        let data = body(5000);
        let s = server::start(data.clone(), None);
        assert_eq!(read_all(fetch(&s.url, &FtpOptions::default(), 0, None).unwrap()), data);
        assert_eq!(read_all(fetch(&s.url, &FtpOptions::default(), 1000, Some(1999)).unwrap()), &data[1000..2000]);
        let active = FtpOptions { active: true, ..FtpOptions::default() };
        assert_eq!(read_all(fetch(&s.url, &active, 4000, None).unwrap()), &data[4000..]);
    }

    #[test]
    fn binds_control_and_data_connections_to_the_adapter_in_both_modes() {
        let data = body(4000);
        let s = server::start(data.clone(), None);
        let (route, stand_in) = crate::route::tests::bound_to_loopback();
        let passive = FtpOptions { route: route.clone(), ..FtpOptions::default() };
        let active = FtpOptions { active: true, ..passive.clone() };
        let found = probe(&s.url, &passive).unwrap();
        assert_eq!(found.total, Some(4000));
        assert_eq!(found.server, Some(IpAddr::from([127, 0, 0, 1])), "the server it reached");
        assert_eq!(read_all(fetch(&s.url, &passive, 0, None).unwrap()), data);
        assert_eq!(read_all(fetch(&s.url, &active, 1000, None).unwrap()), &data[1000..]);
        // Once the adapter is gone, nothing connects, and the error says why.
        stand_in.gone.store(true, std::sync::atomic::Ordering::SeqCst);
        for opts in [&passive, &active] {
            match fetch(&s.url, opts, 0, None) {
                Err(EngineError::AdapterDown(down)) => assert_eq!(down.adapter, "Test adapter"),
                other => panic!("expected the adapter to be down, got {:?}", other.map(|_| ())),
            }
        }
    }

    #[test]
    fn logs_in_from_the_address_or_a_site_login() {
        let s = server::start(body(10), Some(("ann", "p@ss")));
        assert!(matches!(probe(&s.url, &FtpOptions::default()), Err(EngineError::NeedsLogin(..))));
        let with_user = s.url.replace("ftp://", "ftp://ann:p%40ss@");
        assert_eq!(probe(&with_user, &FtpOptions::default()).unwrap().total, Some(10));
        let wrong = s.url.replace("ftp://", "ftp://ann:nope@");
        assert!(matches!(probe(&wrong, &FtpOptions::default()), Err(EngineError::LoginRejected(..))));
        let saved = |plain| FtpOptions {
            logins: vec![Login { site: "127.0.0.1".into(), username: "ann".into(), password: "p@ss".into(), plain }],
            ..FtpOptions::default()
        };
        // A saved login goes to FTP only when allowed on plain connections.
        assert!(matches!(probe(&s.url, &saved(false)), Err(EngineError::NeedsLogin(..))));
        assert_eq!(probe(&s.url, &saved(true)).unwrap().total, Some(10));
    }

    #[test]
    fn logs_in_to_the_proxy_first_when_it_has_a_login() {
        // The test server stands in for the proxy: it takes any USER, and
        // with a login set, refuses any other PASS.
        let open = server::start(body(10), None);
        let addr = open.url.trim_start_matches("ftp://").split('/').next().unwrap().to_string();
        let through = |login: net::ProxyLogin| FtpOptions {
            proxy: Some(addr.clone()),
            proxy_login: login,
            ..FtpOptions::default()
        };
        assert_eq!(probe(&open.url, &through(net::ProxyLogin::new("px", "pp"))).unwrap().total, Some(10));

        let strict = server::start(body(10), Some(("px", "pp")));
        let addr = strict.url.trim_start_matches("ftp://").split('/').next().unwrap().to_string();
        let opts =
            FtpOptions { proxy: Some(addr), proxy_login: net::ProxyLogin::new("px", "wrong"), ..FtpOptions::default() };
        match probe(&strict.url, &opts) {
            Err(EngineError::Protocol(m)) => assert!(m.contains("proxy refused"), "{m}"),
            other => panic!("expected the proxy to refuse, got {:?}", other.map(|p| p.total)),
        }
    }

    #[test]
    fn goes_through_a_socks_proxy_for_control_and_data() {
        let data = body(3000);
        let s = server::start(data.clone(), None);
        let proxy = net::socks_server::start(None);
        let server_addr = s.url.trim_start_matches("ftp://").split('/').next().unwrap().to_string();
        let opts = FtpOptions {
            socks: Some(
                net::SocksProxy::parse(&format!("socks5://{}", proxy.addr), &net::ProxyLogin::default()).unwrap(),
            ),
            // Active mode cannot work behind the proxy; passive is used.
            active: true,
            ..FtpOptions::default()
        };
        assert_eq!(read_all(fetch(&s.url, &opts, 1000, None).unwrap()), &data[1000..]);
        let targets = proxy.targets.lock().unwrap().clone();
        assert_eq!(targets.len(), 2, "control and data: {targets:?}");
        assert_eq!(targets[0], server_addr);
        assert!(targets[1].starts_with("127.0.0.1:") && targets[1] != server_addr, "{targets:?}");
    }

    #[test]
    fn encoded_line_breaks_cannot_add_commands() {
        let s = server::start(body(10), None);
        let path = s.url.replace("file.bin", "file.bin%0D%0ADELE%20file.bin");
        assert!(matches!(probe(&path, &FtpOptions::default()), Err(EngineError::Protocol(_))));
        let user = s.url.replace("ftp://", "ftp://ann%0D%0AQUIT:x@");
        assert!(matches!(probe(&user, &FtpOptions::default()), Err(EngineError::Protocol(_))));
        assert!(has_control("a\nb") && !has_control("a b"));
    }

    #[test]
    fn ftps_is_tls_on_the_control_and_every_data_connection() {
        use std::sync::atomic::Ordering;
        let data = body(5000);
        let s = server::start_with(data.clone(), None, server::Tls::Implicit);
        assert!(s.url.starts_with("ftps://") && is_ftps(&s.url) && is_ftp(&s.url));
        let p = probe(&s.url, &FtpOptions::default()).unwrap();
        assert_eq!((p.total, p.resumable), (Some(5000), true));
        assert_eq!(p.last_modified.as_deref(), Some("Wed, 21 Oct 2015 07:28:00 GMT"));
        assert_eq!(read_all(fetch(&s.url, &FtpOptions::default(), 0, None).unwrap()), data);
        assert_eq!(read_all(fetch(&s.url, &FtpOptions::default(), 1000, Some(1999)).unwrap()), &data[1000..2000]);
        let active = FtpOptions { active: true, ..FtpOptions::default() };
        assert_eq!(read_all(fetch(&s.url, &active, 4000, None).unwrap()), &data[4000..]);
        assert_eq!(s.transfers.load(Ordering::Relaxed), 3);
        assert_eq!(s.tls_transfers.load(Ordering::Relaxed), 3, "every data connection is TLS");
        assert_eq!(s.plain_users.load(Ordering::Relaxed), 0, "the login never goes plain");
        assert_eq!(*s.rests.lock().unwrap(), [0, 1000, 4000]);
    }

    #[test]
    fn an_ftp_server_that_asks_for_tls_gets_auth_tls() {
        use std::sync::atomic::Ordering;
        let data = body(3000);
        let s = server::start_with(data.clone(), Some(("ann", "pw")), server::Tls::Required);
        // A saved login not allowed on plain connections still goes over TLS.
        let opts = FtpOptions {
            logins: vec![Login {
                site: "127.0.0.1".into(),
                username: "ann".into(),
                password: "pw".into(),
                plain: false,
            }],
            ..FtpOptions::default()
        };
        assert_eq!(probe(&s.url, &opts).unwrap().total, Some(3000));
        assert_eq!(s.plain_users.load(Ordering::Relaxed), 1, "refused once, then TLS");
        // Later connections start with AUTH TLS.
        assert_eq!(read_all(fetch(&s.url, &opts, 500, None).unwrap()), &data[500..]);
        assert_eq!(s.plain_users.load(Ordering::Relaxed), 1);
        assert_eq!(s.tls_transfers.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn an_ftp_server_that_does_not_ask_stays_plain() {
        use std::sync::atomic::Ordering;
        let s = server::start_with(body(100), None, server::Tls::Offered);
        assert_eq!(read_all(fetch(&s.url, &FtpOptions::default(), 0, None).unwrap()), body(100));
        assert_eq!(s.plain_users.load(Ordering::Relaxed), 1);
        assert_eq!(s.tls_transfers.load(Ordering::Relaxed), 0);
        assert!(wants_tls(534, "Policy requires SSL."));
        assert!(wants_tls(530, "Non-anonymous sessions must use encryption."));
        assert!(wants_tls(550, "SSL/TLS required on the control channel"));
        assert!(!wants_tls(530, "Login incorrect."));
        assert!(!wants_tls(230, "TLS is nice"));
    }

    #[test]
    fn the_servers_certificate_is_checked_by_the_system() {
        // Outside tests, nothing trusts the test server's self-signed
        // certificate.
        let s = server::start_with(body(10), None, server::Tls::Implicit);
        let addr = s.url.trim_start_matches("ftps://").split('/').next().unwrap().to_string();
        let stream = TcpStream::connect(&addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        assert!(matches!(handshake(&connector().unwrap(), "127.0.0.1", stream), Err(EngineError::Tls(_))));
    }

    #[test]
    fn plain_bytes_sent_with_the_tls_go_ahead_are_refused() {
        // A server (or someone on the way) that sends a reply along with its
        // 234 could have it read as if it came over TLS.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                stream.write_all(b"220 hi\r\n").unwrap();
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.starts_with("USER") {
                    stream.write_all(b"530 TLS required\r\n").unwrap();
                } else {
                    stream.write_all(b"234 go ahead\r\n230 logged in\r\n").unwrap();
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        });
        match probe(&format!("ftp://{addr}/a"), &FtpOptions::default()) {
            Err(EngineError::Protocol(m)) => assert!(m.contains("before TLS"), "{m}"),
            other => panic!("expected a refusal, got {:?}", other.map(|p| p.total)),
        }
    }

    #[test]
    fn ftps_goes_around_an_ftp_proxy_and_through_socks() {
        let data = body(2000);
        let s = server::start_with(data.clone(), None, server::Tls::Implicit);
        // Nothing listens at the FTP proxy; ftps:// does not go there.
        let around = FtpOptions { proxy: Some("127.0.0.1:1".into()), ..FtpOptions::default() };
        assert_eq!(probe(&s.url, &around).unwrap().total, Some(2000));
        let proxy = net::socks_server::start(None);
        let opts = FtpOptions {
            socks: Some(
                net::SocksProxy::parse(&format!("socks5://{}", proxy.addr), &net::ProxyLogin::default()).unwrap(),
            ),
            ..FtpOptions::default()
        };
        assert_eq!(read_all(fetch(&s.url, &opts, 100, None).unwrap()), &data[100..]);
        assert_eq!(proxy.targets.lock().unwrap().len(), 2, "control and data");
    }

    #[test]
    fn replies_and_dates_parse() {
        assert_eq!(pasv_port("Entering Passive Mode (127,0,0,1,195,80)."), Some(195 * 256 + 80));
        assert_eq!(pasv_port("nothing"), None);
        assert_eq!(mdtm_to_http_date("19941106084937"), Some("Sun, 06 Nov 1994 08:49:37 GMT".into()));
        assert_eq!(mdtm_to_http_date("2015"), None);
        assert!(is_ftp("FTP://host/a") && is_ftp("FTPS://host/a") && !is_ftp("https://host/a"));
        assert!(is_ftps("ftps://host/a") && !is_ftps("ftp://host/a"));
    }
}
