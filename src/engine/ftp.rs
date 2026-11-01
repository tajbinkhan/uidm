//! FTP downloads. Each fetch opens its own control connection, logs in
//! (with the address's user and password, else a matching site login, else
//! anonymously), switches to binary, and opens a data connection: passive by
//! default (EPSV, then PASV), or active (PORT or EPRT) when the settings say
//! so. The size comes from `SIZE`, the date from `MDTM`, and resuming and
//! parallel connections start each range with `REST`. Through an FTP proxy,
//! the control connection goes to the proxy, logs in to it first when it has
//! a login of its own, then logs in as `user@host`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use super::http::Probe;
use super::EngineError;
use crate::net::{self, Login};

const TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// The longest FTP reply line read; real ones are well under a kilobyte.
const MAX_REPLY_LINE: u64 = 64 * 1024;

/// Whether `url` is an FTP address.
pub fn is_ftp(url: &str) -> bool {
    url.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("ftp://"))
}

/// What an FTP fetch needs besides the address.
#[derive(Clone, Default)]
pub struct FtpOptions {
    /// Open data connections from the server to UIDM (PORT) instead of the
    /// other way round.
    pub active: bool,
    pub logins: Vec<Login>,
    /// `host:port` of an FTP proxy to go through.
    pub proxy: Option<String>,
    /// The proxy's own login, if it wants one.
    pub proxy_login: net::ProxyLogin,
}

/// A logged-in control connection.
struct Control {
    reader: BufReader<TcpStream>,
    stream: TcpStream,
    host: String,
}

fn io(e: std::io::Error) -> EngineError {
    EngineError::Io(e.to_string())
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
        let port = parsed.port().unwrap_or(21);
        // Through a proxy, the login names the server to reach.
        let (addr, target) = match &opts.proxy {
            Some(proxy) => {
                let (h, p) = proxy.rsplit_once(':').unwrap_or((proxy.as_str(), "21"));
                let addr = (h, p.parse().unwrap_or(21))
                    .to_socket_addrs_first()
                    .ok_or_else(|| EngineError::Io(format!("could not find the FTP proxy {proxy}")))?;
                (addr, Some(if port == 21 { host.clone() } else { format!("{host}:{port}") }))
            }
            None => {
                let addr = (host.as_str(), port)
                    .to_socket_addrs_first()
                    .ok_or_else(|| EngineError::Io(format!("could not find the FTP server {host}")))?;
                (addr, None)
            }
        };
        let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(io)?;
        stream.set_read_timeout(Some(TIMEOUT)).map_err(io)?;
        stream.set_write_timeout(Some(TIMEOUT)).map_err(io)?;
        let mut c = Control {
            reader: BufReader::new(stream.try_clone().map_err(io)?),
            stream,
            host: host.to_ascii_lowercase(),
        };
        let (code, text) = c.reply()?;
        if code != 220 {
            return Err(EngineError::Protocol(format!("the FTP server said: {code} {text}")));
        }
        let proxy_login = &opts.proxy_login;
        if target.is_some() && !proxy_login.is_empty() {
            if has_control(&proxy_login.username) || has_control(&proxy_login.password) {
                return Err(EngineError::Protocol("the FTP proxy's login has control characters in it".into()));
            }
            let (code, _) = c.command(&format!("USER {}", proxy_login.username))?;
            let (code, text) = if code == 331 { c.command(&format!("PASS {}", proxy_login.password))? } else { (code, String::new()) };
            if !matches!(code, 230 | 202) {
                return Err(EngineError::Protocol(format!("the FTP proxy refused its login: {code} {text}")));
            }
        }

        let from_url = (!parsed.username().is_empty()).then(|| (decode(parsed.username()), decode(parsed.password().unwrap_or(""))));
        // FTP sends the password as it is, so a saved login goes only if
        // allowed on plain connections.
        let saved = net::find_login(&opts.logins, url).filter(|l| l.plain).map(|l| (l.username.clone(), l.password.clone()));
        let given = from_url.is_some() || saved.is_some();
        let (user, pass) = from_url.or(saved).unwrap_or_else(|| ("anonymous".into(), "uidm@".into()));
        if has_control(&user) || has_control(&pass) {
            return Err(EngineError::Protocol("the FTP login has control characters in it".into()));
        }
        let user = match &target {
            Some(t) => format!("{user}@{t}"),
            None => user,
        };
        let (code, _) = c.command(&format!("USER {user}"))?;
        let code = if code == 331 { c.command(&format!("PASS {pass}"))?.0 } else { code };
        match code {
            230 | 202 => {}
            530 | 331 | 332 if given => return Err(EngineError::LoginRejected(c.host.clone(), None)),
            530 | 331 | 332 => return Err(EngineError::NeedsLogin(c.host.clone(), None)),
            other => return Err(EngineError::Protocol(format!("the FTP server refused the login ({other})"))),
        }
        let (code, text) = c.command("TYPE I")?;
        if code != 200 {
            return Err(EngineError::Protocol(format!("the FTP server would not send binary: {code} {text}")));
        }
        let path = decode(parsed.path());
        if has_control(&path) {
            return Err(EngineError::Protocol("the FTP address has control characters in its path".into()));
        }
        Ok((c, path))
    }

    /// One reply, joining the lines of a multi-line one. Each line is read up
    /// to `MAX_REPLY_LINE` bytes, so a server sending one endless line cannot
    /// fill the memory.
    fn reply(&mut self) -> Result<(u16, String), EngineError> {
        let mut line = String::new();
        if self.reader.by_ref().take(MAX_REPLY_LINE).read_line(&mut line).map_err(io)? == 0 {
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
                if self.reader.by_ref().take(MAX_REPLY_LINE).read_line(&mut next).map_err(io)? == 0 {
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
        self.stream.write_all(format!("{cmd}\r\n").as_bytes()).map_err(io)?;
        self.reply()
    }

    /// Ask for a data connection; returns how to get it once a transfer is
    /// asked for.
    fn data(&mut self, active: bool) -> Result<Data, EngineError> {
        let peer = self.stream.peer_addr().map_err(io)?;
        if active {
            let local = self.stream.local_addr().map_err(io)?;
            let listener = TcpListener::bind(SocketAddr::new(local.ip(), 0)).map_err(io)?;
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
        let stream = TcpStream::connect_timeout(&SocketAddr::new(peer.ip(), port), CONNECT_TIMEOUT).map_err(io)?;
        Ok(Data::Connected(stream))
    }
}

/// The port in a `227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)` reply.
fn pasv_port(text: &str) -> Option<u16> {
    let inner = text.split('(').nth(1)?.split(')').next()?;
    let n: Vec<u16> = inner.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    (n.len() == 6).then(|| n[4] * 256 + n[5])
}

trait FirstAddr {
    fn to_socket_addrs_first(&self) -> Option<SocketAddr>;
}

impl FirstAddr for (&str, u16) {
    fn to_socket_addrs_first(&self) -> Option<SocketAddr> {
        std::net::ToSocketAddrs::to_socket_addrs(self).ok()?.next()
    }
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
    data: TcpStream,
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
    let day = crate::queue::days_from_civil(y as i64, mo, d);
    let weekday = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][crate::queue::weekday(day) as usize];
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
    let transfer = Transfer {
        data: data.stream()?,
        _control: c,
    };
    Ok(match end {
        Some(end) => Box::new(transfer.take(end + 1 - start)),
        None => Box::new(transfer),
    })
}

/// A tiny FTP server for tests: one file, anonymous or one login, passive
/// and active mode, `SIZE`, `MDTM` and `REST`.
#[cfg(test)]
pub(crate) mod server {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread;

    pub struct FtpServer {
        pub url: String,
        /// Data connections opened, so tests can tell parallel fetches.
        pub transfers: Arc<AtomicU64>,
    }

    /// Serve `body` as `/pub/file.bin`; with `login`, only to that user.
    pub fn start(body: Vec<u8>, login: Option<(&'static str, &'static str)>) -> FtpServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let transfers = Arc::new(AtomicU64::new(0));
        let count = Arc::clone(&transfers);
        let body = Arc::new(body);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (body, count) = (Arc::clone(&body), Arc::clone(&count));
                thread::spawn(move || {
                    let _ = session(stream, &body, login, &count);
                });
            }
        });
        FtpServer {
            url: format!("ftp://{addr}/pub/file.bin"),
            transfers,
        }
    }

    fn session(stream: TcpStream, body: &[u8], login: Option<(&str, &str)>, count: &AtomicU64) -> std::io::Result<()> {
        let mut out = stream.try_clone()?;
        let mut reader = BufReader::new(stream);
        let mut say = |s: &str| out.write_all(format!("{s}\r\n").as_bytes());
        say("220-Welcome")?;
        say("220 test server")?;
        let (mut user, mut rest, mut passive, mut active) = (String::new(), 0u64, None::<TcpListener>, None::<SocketAddr>);
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let line = line.trim_end();
            let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
            match cmd.to_ascii_uppercase().as_str() {
                "USER" => {
                    user = arg.to_string();
                    say("331 password please")?;
                }
                "PASS" => match login {
                    Some((u, p)) if (u, p) != (user.as_str(), arg) => say("530 no")?,
                    _ => say("230 in")?,
                },
                "TYPE" => say("200 binary")?,
                "SIZE" if arg == "/pub/file.bin" => say(&format!("213 {}", body.len()))?,
                "MDTM" if arg == "/pub/file.bin" => say("213 20151021072800")?,
                "SIZE" | "MDTM" => say("550 no such file")?,
                "REST" => {
                    rest = arg.parse().unwrap_or(0);
                    say("350 restarting")?;
                }
                "EPSV" => {
                    let l = TcpListener::bind("127.0.0.1:0")?;
                    say(&format!("229 Entering Extended Passive Mode (|||{}|)", l.local_addr()?.port()))?;
                    passive = Some(l);
                }
                "PORT" => {
                    let n: Vec<u16> = arg.split(',').filter_map(|s| s.parse().ok()).collect();
                    active = Some(SocketAddr::from(([n[0] as u8, n[1] as u8, n[2] as u8, n[3] as u8], n[4] * 256 + n[5])));
                    say("200 ok")?;
                }
                "RETR" if arg != "/pub/file.bin" => say("550 no such file")?,
                "RETR" => {
                    let data = match (passive.take(), active.take()) {
                        (Some(l), _) => {
                            say("150 sending")?;
                            l.accept()?.0
                        }
                        (None, Some(a)) => {
                            say("150 sending")?;
                            TcpStream::connect(a)?
                        }
                        _ => {
                            say("425 no data connection")?;
                            continue;
                        }
                    };
                    count.fetch_add(1, Ordering::Relaxed);
                    let mut data = data;
                    let _ = data.write_all(&body[rest.min(body.len() as u64) as usize..]);
                    drop(data);
                    rest = 0;
                    say("226 done")?;
                }
                "QUIT" => {
                    say("221 bye")?;
                    return Ok(());
                }
                _ => say("502 not here")?,
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
        let active = FtpOptions {
            active: true,
            ..FtpOptions::default()
        };
        assert_eq!(read_all(fetch(&s.url, &active, 4000, None).unwrap()), &data[4000..]);
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
            logins: vec![Login {
                site: "127.0.0.1".into(),
                username: "ann".into(),
                password: "p@ss".into(),
                plain,
            }],
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
        let opts = FtpOptions {
            proxy: Some(addr),
            proxy_login: net::ProxyLogin::new("px", "wrong"),
            ..FtpOptions::default()
        };
        match probe(&strict.url, &opts) {
            Err(EngineError::Protocol(m)) => assert!(m.contains("proxy refused"), "{m}"),
            other => panic!("expected the proxy to refuse, got {:?}", other.map(|p| p.total)),
        }
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
    fn replies_and_dates_parse() {
        assert_eq!(pasv_port("Entering Passive Mode (127,0,0,1,195,80)."), Some(195 * 256 + 80));
        assert_eq!(pasv_port("nothing"), None);
        assert_eq!(mdtm_to_http_date("19941106084937"), Some("Sun, 06 Nov 1994 08:49:37 GMT".into()));
        assert_eq!(mdtm_to_http_date("2015"), None);
        assert!(is_ftp("FTP://host/a") && !is_ftp("https://host/a"));
    }
}
