//! How downloads reach servers: through which proxy, and with which login.
//!
//! - Proxies come from the settings: none, the system proxy (the Windows
//!   Internet settings, with their bypass list and any configuration script
//!   or automatic detection; on macOS and Linux the `http_proxy`,
//!   `https_proxy`, `ftp_proxy`, `all_proxy` and `no_proxy` variables), a
//!   configuration script, or ones set by hand. A captured download can also
//!   use the proxy the browser reported.
//! - Site logins pair a site pattern, optionally with a path, with a
//!   username; the password lives in Windows Credential Manager, or on macOS
//!   and Linux in UDM's own encrypted `credentials.bin`. When a
//!   server answers 401, the engine sends the matching login with HTTP Basic
//!   or Digest, to the host that asked and to no other.
//!
//! Each proxy's password is kept apart from the settings, encrypted with
//! DPAPI.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use md5::Md5;
use sha2::Sha256;

use crate::dpapi;
#[cfg(windows)]
use crate::registry;
use udm_domain::site::glob;
use udm_domain::site::{host_of, site_matches, split_login_pattern};

/// Where the Windows Internet settings keep the system proxy.
#[cfg(windows)]
pub const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

/// The path and query of a URL, for Digest's `uri`.
fn request_uri(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest.find('/').map_or("/", |i| &rest[i..]);
    path.split('#').next().unwrap_or("/").to_string()
}

/// A proxy's user name and password; an empty user name means none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyLogin {
    pub username: String,
    pub password: String,
}

impl ProxyLogin {
    pub fn new(username: &str, password: &str) -> Self {
        ProxyLogin { username: username.to_string(), password: password.to_string() }
    }

    pub fn is_empty(&self) -> bool {
        self.username.is_empty()
    }

    /// `user:password@` for a proxy URL, or "" without a login.
    fn userinfo(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        format!("{}:{}@", encode_userinfo(&self.username), encode_userinfo(&self.password))
    }
}

/// Which proxies to use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyRules {
    /// `host:port` of the proxy for http URLs.
    pub http: Option<String>,
    /// `host:port` of the proxy for https URLs.
    pub https: Option<String>,
    /// `scheme://host:port` of a SOCKS proxy (scheme `socks4`, `socks4a` or
    /// `socks5`), for the URLs whose scheme has no proxy of its own.
    pub socks: Option<String>,
    /// `host:port` of the FTP proxy when Windows' setting names one with
    /// `ftp=`. Get System copies it to the FTP proxy field; `ftp://`
    /// downloads use `NetConfig::ftp_proxy`.
    pub ftp: Option<String>,
    /// Hosts reached directly: patterns with `*`, and `<local>` for names
    /// without a dot.
    pub bypass: Vec<String>,
    pub http_login: ProxyLogin,
    pub https_login: ProxyLogin,
    pub socks_login: ProxyLogin,
    /// https URLs go through the SOCKS proxy even when an HTTP or HTTPS
    /// proxy is set, so plain http can use a filtering web proxy while
    /// https, which it cannot read anyway, goes through SOCKS.
    pub socks_for_https: bool,
    /// ftp:// URLs go through the SOCKS proxy when no FTP proxy is set.
    pub socks_for_ftp: bool,
    /// A proxy auto-config script that picks the proxy per URL. When it
    /// cannot be found or run, the proxies above apply.
    pub pac: Option<crate::pac::PacSource>,
}

fn encode_userinfo(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// A bypass list as typed: entries separated by semicolons, commas or
/// spaces.
pub fn parse_bypass(text: &str) -> Vec<String> {
    text.split([';', ',', ' ', '\t', '\n', '\r'])
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The URL scheme for a SOCKS proxy: SOCKS5 always lets the proxy look up
/// names, which the scheme says (`socks5h`; to reqwest, yt-dlp and
/// gallery-dl, `socks5` means looking them up here); SOCKS4 does only as
/// SOCKS4a.
pub fn socks_scheme(v5: bool, remote_dns: bool) -> &'static str {
    match (v5, remote_dns) {
        (true, _) => "socks5h",
        (false, true) => "socks4a",
        (false, false) => "socks4",
    }
}

/// `proxy` with a SOCKS5 proxy's scheme saying that the proxy looks up host
/// names (`socks5h://`), as UDM always has it do: a browser's or the
/// environment's `socks5://` too. A name looked up here would go to the
/// system's resolver, outside the proxy.
pub fn remote_names(proxy: String) -> String {
    match proxy.get(..9) {
        Some(scheme) if scheme.eq_ignore_ascii_case("socks5://") => format!("socks5h://{}", &proxy[9..]),
        _ => proxy,
    }
}

impl ProxyRules {
    /// The proxy for `url`, as a URL the HTTP client understands, or `None` to connect
    /// directly.
    pub fn proxy_for(&self, url: &str) -> Option<String> {
        let host = host_of(url);
        if self.bypassed(&host) {
            return None;
        }
        if let Some(Ok(answer)) = self.pac.as_ref().map(|source| crate::pac::proxy_for(source, url)) {
            return answer.map(remote_names);
        }
        let (scheme, addr, login) = self.fixed_for(url)?;
        Some(remote_names(format!("{scheme}://{}{addr}", login.userinfo())))
    }

    /// The login of the fixed proxy `url` goes through, when it has one. A
    /// proxy a configuration script picks has none.
    pub fn login_for(&self, url: &str) -> Option<&ProxyLogin> {
        let scripted = self.pac.as_ref().is_some_and(|source| crate::pac::proxy_for(source, url).is_ok());
        if scripted || self.bypassed(&host_of(url)) {
            return None;
        }
        self.fixed_for(url).map(|(_, _, login)| login).filter(|l| !l.is_empty())
    }

    /// The fixed proxy for `url` as its scheme, `host:port` and login: the
    /// proxy for the URL's own scheme, else the SOCKS proxy. With
    /// `socks_for_https`, https URLs take the SOCKS proxy first.
    fn fixed_for(&self, url: &str) -> Option<(&str, &str, &ProxyLogin)> {
        let https = url.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://"));
        let own = if https && self.socks_for_https && self.socks.is_some() {
            None
        } else if https {
            self.https.as_deref().map(|a| (a, &self.https_login))
        } else {
            self.http.as_deref().map(|a| (a, &self.http_login))
        };
        if let Some((addr, login)) = own {
            return Some(("http", addr, login));
        }
        let socks = self.socks.as_deref()?;
        let (scheme, addr) = socks.split_once("://").unwrap_or(("socks5", socks));
        Some((scheme, addr, &self.socks_login))
    }

    /// The SOCKS proxy an ftp:// `url` goes through: with `socks_for_ftp`,
    /// when its host is not bypassed and no configuration script is set
    /// (scripts answer for web addresses).
    pub fn socks_for_ftp(&self, url: &str) -> Option<SocksProxy> {
        if !self.socks_for_ftp || self.pac.is_some() || self.bypassed(&host_of(url)) {
            return None;
        }
        SocksProxy::parse(self.socks.as_deref()?, &self.socks_login)
    }

    fn bypassed(&self, host: &str) -> bool {
        self.bypass.iter().any(|b| {
            let b = b.trim().to_ascii_lowercase();
            if b == "<local>" {
                !host.contains('.')
            } else if b.contains('*') {
                glob(&b, host)
            } else {
                !b.is_empty() && b == host
            }
        })
    }
}

/// A SOCKS proxy that UDM speaks to itself, for the FTP connections the HTTP client
/// does not make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocksProxy {
    /// `host:port` of the proxy.
    pub addr: String,
    pub v5: bool,
    /// The proxy looks up host names (SOCKS5 and SOCKS4a); otherwise they
    /// are looked up here and sent as an IPv4 address (SOCKS4).
    pub remote_dns: bool,
    pub login: ProxyLogin,
}

impl SocksProxy {
    /// From `socks5://host:port` (or `socks4://`, `socks4a://`; no scheme is
    /// SOCKS5) and its login.
    pub fn parse(proxy: &str, login: &ProxyLogin) -> Option<SocksProxy> {
        let (scheme, addr) = proxy.split_once("://").unwrap_or(("socks5", proxy));
        let (v5, remote_dns) = match scheme.to_ascii_lowercase().as_str() {
            "socks5" | "socks5h" | "socks" => (true, true),
            "socks4a" => (false, true),
            "socks4" => (false, false),
            _ => return None,
        };
        // A login in the address itself (`user:pw@host:port`) is dropped;
        // the separate one counts.
        let addr = addr.rsplit_once('@').map_or(addr, |(_, a)| a).trim_end_matches('/');
        (!addr.is_empty()).then(|| SocksProxy { addr: addr.to_string(), v5, remote_dns, login: login.clone() })
    }

    /// Open a connection to `host:port` through the proxy (a SOCKS CONNECT).
    /// The connection to the proxy goes out by `route`, which also looks up
    /// a name SOCKS4 cannot send.
    pub fn connect(
        &self,
        route: &crate::route::Route,
        host: &str,
        port: u16,
        timeout: std::time::Duration,
    ) -> io::Result<std::net::TcpStream> {
        use std::io::{Read, Write};
        use std::net::IpAddr;
        let fail = |m: String| io::Error::other(format!("the SOCKS proxy {}: {m}", self.addr));
        let mut s = route.connect_to(&self.addr, 1080, timeout)?;
        s.set_read_timeout(Some(timeout))?;
        s.set_write_timeout(Some(timeout))?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if self.v5 {
            let login = !self.login.is_empty();
            s.write_all(if login { &[5, 2, 0, 2] } else { &[5, 1, 0] })?;
            let mut choice = [0u8; 2];
            s.read_exact(&mut choice)?;
            match choice {
                [5, 0] => {}
                [5, 2] if login => {
                    let (u, p) = (self.login.username.as_bytes(), self.login.password.as_bytes());
                    if u.len() > 255 || p.len() > 255 {
                        return Err(fail("the user name or password is too long".into()));
                    }
                    let mut auth = vec![1, u.len() as u8];
                    auth.extend_from_slice(u);
                    auth.push(p.len() as u8);
                    auth.extend_from_slice(p);
                    s.write_all(&auth)?;
                    let mut answer = [0u8; 2];
                    s.read_exact(&mut answer)?;
                    if answer[1] != 0 {
                        return Err(fail("refused the login".into()));
                    }
                }
                [5, 0xff] => return Err(fail("wants a login".into())),
                _ => return Err(fail("did not answer as SOCKS5".into())),
            }
            let mut req = vec![5, 1, 0];
            match host.parse::<IpAddr>() {
                Ok(IpAddr::V4(ip)) => {
                    req.push(1);
                    req.extend_from_slice(&ip.octets());
                }
                Ok(IpAddr::V6(ip)) => {
                    req.push(4);
                    req.extend_from_slice(&ip.octets());
                }
                Err(_) => {
                    if host.len() > 255 {
                        return Err(fail("the host name is too long".into()));
                    }
                    req.push(3);
                    req.push(host.len() as u8);
                    req.extend_from_slice(host.as_bytes());
                }
            }
            req.extend_from_slice(&port.to_be_bytes());
            s.write_all(&req)?;
            let mut head = [0u8; 4];
            s.read_exact(&mut head)?;
            if head[1] != 0 {
                return Err(fail(format!("could not reach {host}:{port} (error {})", head[1])));
            }
            // The address it bound, which is not needed.
            let rest = match head[3] {
                1 => 4,
                4 => 16,
                3 => {
                    let mut n = [0u8; 1];
                    s.read_exact(&mut n)?;
                    n[0] as usize
                }
                _ => return Err(fail("gave an unknown address type".into())),
            };
            let mut skip = vec![0u8; rest + 2];
            s.read_exact(&mut skip)?;
        } else {
            let user = self.login.username.as_bytes();
            let mut req = vec![4, 1];
            req.extend_from_slice(&port.to_be_bytes());
            let ip = host.parse::<std::net::Ipv4Addr>().ok();
            let name = match ip {
                Some(ip) => {
                    req.extend_from_slice(&ip.octets());
                    None
                }
                // SOCKS4a: an address 0.0.0.x asks the proxy to look the
                // name up.
                None if self.remote_dns => {
                    req.extend_from_slice(&[0, 0, 0, 1]);
                    Some(host)
                }
                None => {
                    let ip = route
                        .lookup(host, port)?
                        .into_iter()
                        .find_map(|a| match a.ip() {
                            IpAddr::V4(ip) => Some(ip),
                            IpAddr::V6(_) => None,
                        })
                        .ok_or_else(|| fail(format!("SOCKS4 needs an IPv4 address, and {host} has none")))?;
                    req.extend_from_slice(&ip.octets());
                    None
                }
            };
            req.extend_from_slice(user);
            req.push(0);
            if let Some(name) = name {
                req.extend_from_slice(name.as_bytes());
                req.push(0);
            }
            s.write_all(&req)?;
            let mut answer = [0u8; 8];
            s.read_exact(&mut answer)?;
            if answer[1] != 0x5a {
                return Err(fail(format!("could not reach {host}:{port} (error {})", answer[1])));
            }
        }
        s.set_read_timeout(None)?;
        s.set_write_timeout(None)?;
        Ok(s)
    }
}

/// Read Windows' proxy setting: `host:port` for everything, or
/// `http=host:port;https=host:port;ftp=host:port;socks=host:port` per
/// scheme. Windows' SOCKS proxies speak SOCKS4.
pub fn parse_proxy_server(s: &str) -> ProxyRules {
    let mut rules = ProxyRules::default();
    let s = s.trim();
    if !s.contains('=') {
        if !s.is_empty() {
            rules.http = Some(s.to_string());
            rules.https = Some(s.to_string());
        }
        return rules;
    }
    for part in s.split(';') {
        let Some((scheme, addr)) = part.split_once('=') else {
            continue;
        };
        let addr = addr.trim().trim_start_matches("http://").to_string();
        if addr.is_empty() {
            continue;
        }
        match scheme.trim().to_ascii_lowercase().as_str() {
            "http" => rules.http = Some(addr),
            "https" => rules.https = Some(addr),
            "ftp" => rules.ftp = Some(addr),
            "socks" => rules.socks = Some(format!("socks4://{addr}")),
            _ => {}
        }
    }
    rules
}

/// The system proxy from the Internet settings under `key`, if one is on.
/// Automatic configuration scripts are not read.
#[cfg(windows)]
pub fn system_proxy_at(key: &str) -> Option<ProxyRules> {
    if registry::get_dword(key, "ProxyEnable").ok().flatten() != Some(1) {
        return None;
    }
    let server = registry::get_string(key, "ProxyServer").ok().flatten()?;
    let mut rules = parse_proxy_server(&server);
    if let Some(over) = registry::get_string(key, "ProxyOverride").ok().flatten() {
        rules.bypass = over.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    (rules.http.is_some() || rules.https.is_some() || rules.socks.is_some()).then_some(rules)
}

/// The system's fixed proxies, without a configuration script: the
/// Windows Internet settings, or the proxy variables elsewhere.
pub fn system_fixed_proxy() -> Option<ProxyRules> {
    #[cfg(windows)]
    return system_proxy_at(INTERNET_SETTINGS);
    #[cfg(not(windows))]
    return proxy_from_env(|name| std::env::var(name).ok());
}

/// The system proxy: its fixed proxies, and on Windows its configuration
/// script or automatic detection, which comes first.
pub fn system_proxy() -> Option<ProxyRules> {
    let fixed = system_fixed_proxy();
    match crate::pac::windows_source() {
        Some(source) => Some(ProxyRules { pac: Some(source), ..fixed.unwrap_or_default() }),
        None => fixed,
    }
}

/// The proxies the `http_proxy`, `https_proxy`, `ftp_proxy`, `all_proxy`
/// and `no_proxy` variables name (lower case first, as curl reads them),
/// with `var` reading one. `all_proxy` is the SOCKS proxy when it says
/// `socks`, else the proxy for http and https alike.
pub fn proxy_from_env(var: impl Fn(&str) -> Option<String>) -> Option<ProxyRules> {
    let get = |name: &str| {
        var(name).or_else(|| var(&name.to_ascii_uppercase())).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
    };
    // `http://host:port/` to `host:port`; SOCKS keeps its scheme.
    let addr = |v: String| {
        let v = v.strip_prefix("http://").or_else(|| v.strip_prefix("https://")).map(str::to_string).unwrap_or(v);
        v.trim_end_matches('/').to_string()
    };
    let mut rules = ProxyRules::default();
    if let Some(all) = get("all_proxy") {
        if all.to_ascii_lowercase().starts_with("socks") {
            rules.socks = Some(all.trim_end_matches('/').to_string());
        } else {
            rules.http = Some(addr(all.clone()));
            rules.https = Some(addr(all));
        }
    }
    if let Some(http) = get("http_proxy") {
        rules.http = Some(addr(http));
    }
    if let Some(https) = get("https_proxy") {
        rules.https = Some(addr(https));
    }
    rules.ftp = get("ftp_proxy").map(addr);
    if let Some(none) = get("no_proxy") {
        rules.bypass = none
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            // `.corp.net` means the domain and its hosts.
            .map(|s| if s.starts_with('.') { format!("*{s}") } else { s.to_string() })
            .collect();
    }
    (rules.http.is_some() || rules.https.is_some() || rules.socks.is_some() || rules.ftp.is_some()).then_some(rules)
}

/// The proxies a download goes through when it uses the browser's proxy
/// setting `p`. `None` connects directly, as the browser does. The
/// browser's own fixed proxies, script, automatic detection and bypass list
/// carry over; under "system" the system proxy is read here, as UDM's own
/// System mode reads it. A script given as text runs as one at an address
/// does (see `pac::PacSource::Script`). HTTPS and QUIC proxies, which the
/// download engine cannot speak, are left out.
pub fn browser_proxy_rules(p: &udm_domain::download::BrowserProxy) -> Option<ProxyRules> {
    use crate::pac::PacSource;
    use udm_domain::download::BrowserProxyMode;
    let bypass = browser_bypass(&p.bypass);
    match p.mode {
        BrowserProxyMode::Direct => None,
        BrowserProxyMode::System => system_proxy(),
        BrowserProxyMode::AutoDetect => {
            Some(ProxyRules { pac: Some(PacSource::AutoDetect), bypass, ..ProxyRules::default() })
        }
        BrowserProxyMode::Pac => {
            let source = match (p.pac_url.as_deref().map(str::trim), p.pac_script.as_deref()) {
                (Some(url), _) if udm_domain::batch::is_web_url(url) => PacSource::Url(url.to_string()),
                (_, Some(script)) if !script.trim().is_empty() => PacSource::Script(script.to_string()),
                // A script nothing here can fetch (a file:// one): direct,
                // as the browser goes when its script cannot be read.
                _ => return None,
            };
            Some(ProxyRules { pac: Some(source), bypass, ..ProxyRules::default() })
        }
        BrowserProxyMode::Fixed => {
            // `host:port` of a plain HTTP proxy; a SOCKS one keeps its scheme.
            let http = |a: &Option<String>| {
                let a = a.as_deref()?.trim();
                match a.split_once("://") {
                    None => Some(a.to_string()),
                    Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => {
                        Some(rest.trim_end_matches('/').to_string())
                    }
                    _ => None,
                }
            };
            let socks_of = |a: &Option<String>| {
                let a = a.as_deref()?.trim();
                let scheme = a.split_once("://")?.0.to_ascii_lowercase();
                matches!(scheme.as_str(), "socks4" | "socks4a" | "socks5" | "socks5h")
                    .then(|| a.trim_end_matches('/').to_string())
            };
            let rules = ProxyRules {
                http: http(&p.http),
                https: http(&p.https),
                // A SOCKS proxy named for one scheme carries that scheme.
                socks: socks_of(&p.socks).or_else(|| socks_of(&p.https)).or_else(|| socks_of(&p.http)),
                socks_for_https: p.https.is_some() && socks_of(&p.https).is_some(),
                bypass,
                ..ProxyRules::default()
            };
            (rules.http.is_some() || rules.https.is_some() || rules.socks.is_some()).then_some(rules)
        }
    }
}

/// A browser's bypass list as `ProxyRules::bypass` patterns: Chrome's
/// `<local>`, `*.corp.net` and `host:port` entries, and Firefox's
/// `.corp.net` (a domain and its hosts). The scheme and port an entry may
/// name are dropped, since bypassing goes by host. Chrome's `<-loopback>`
/// and address ranges have no counterpart and are left out.
pub fn browser_bypass(list: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for entry in list.iter().flat_map(|e| e.split([',', ';', ' '])) {
        let e = entry.trim().to_ascii_lowercase();
        if e.is_empty() || e == "<-loopback>" || (e.contains('/') && !e.contains("://")) {
            continue;
        }
        if e == "<local>" {
            out.push(e);
            continue;
        }
        let host = e.split_once("://").map_or(e.as_str(), |(_, h)| h).trim_end_matches('/');
        // A port after the host, but not inside an IPv6 address.
        let host = match host.rsplit_once(':') {
            Some((h, port)) if !h.ends_with(':') && !h.contains('[') && port.bytes().all(|b| b.is_ascii_digit()) => h,
            _ => host,
        };
        let host = if host.starts_with('.') { format!("*{host}") } else { host.to_string() };
        if !host.is_empty() && !out.contains(&host) {
            out.push(host);
        }
    }
    out
}

/// A login for the sites matching `site`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub site: String,
    pub username: String,
    pub password: String,
    /// It may go where it can be read on the way: as Basic authentication
    /// over plain http, and to FTP servers, which take passwords as they
    /// are. A saved login needs the user's say-so for that; one typed for a
    /// download is meant for its address.
    pub plain: bool,
}

/// Whether `url` falls under the login pattern `pattern`: a site as for
/// [`site_matches`], optionally followed by a path. A path with `*` is
/// matched as a whole (`/private/*`); one without covers itself and what is
/// below it (`/private` covers `/private/a.zip`). Case is ignored.
pub fn login_matches(pattern: &str, url: &str) -> bool {
    let (site, path) = split_login_pattern(pattern);
    if !site_matches(site, &host_of(url)) {
        return false;
    }
    let path = path.to_ascii_lowercase();
    if path.is_empty() {
        return true;
    }
    let target = request_uri(url);
    let target = target.split('?').next().unwrap_or("/").to_ascii_lowercase();
    if path.contains('*') {
        return glob(&path, &target);
    }
    let folder = path.trim_end_matches('/');
    folder.is_empty() || target == folder || target.starts_with(&format!("{folder}/"))
}

/// The login for `url`, if any: of the matching patterns, the longest (the
/// most specific) wins, so a folder's login beats its server's.
pub fn find_login<'a>(logins: &'a [Login], url: &str) -> Option<&'a Login> {
    logins.iter().filter(|l| login_matches(&l.site, url)).max_by_key(|l| l.site.trim().trim_start_matches("*.").len())
}

/// Everything a download needs to reach servers.
#[derive(Debug, Clone, Default)]
pub struct NetConfig {
    pub proxy: Option<ProxyRules>,
    pub logins: Vec<Login>,
    /// FTP data connections come from the server (active mode) instead of
    /// going to it (passive mode, the default).
    pub ftp_active: bool,
    /// `host:port` of an FTP proxy for `ftp://` downloads.
    pub ftp_proxy: Option<String>,
    /// The FTP proxy's own login, sent before the server's.
    pub ftp_proxy_login: ProxyLogin,
    /// The network adapter connections are bound to, and where names are
    /// looked up.
    pub route: crate::route::Route,
}

impl NetConfig {
    /// The same, going out by `route`. Bound to an adapter, it uses no
    /// proxy configuration script, which the system would fetch unbound.
    pub fn with_route(&self, route: crate::route::Route) -> NetConfig {
        let mut net = self.clone();
        if route.binding.is_some() {
            if let Some(rules) = net.proxy.as_mut() {
                rules.pac = None;
            }
        }
        net.route = route;
        net
    }
}

/// Split a `WWW-Authenticate` header into its scheme and parameters.
fn parse_challenge(header: &str) -> (String, Vec<(String, String)>) {
    let header = header.trim();
    let (scheme, rest) = header.split_once(' ').unwrap_or((header, ""));
    let mut params = Vec::new();
    let mut chars = rest.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| *c == ',' || c.is_whitespace()) {
            chars.next();
        }
        let key: String = chars.by_ref().take_while(|c| *c != '=').collect();
        if key.is_empty() {
            break;
        }
        let mut value = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            let mut escaped = false;
            for c in chars.by_ref() {
                match (escaped, c) {
                    (true, c) => {
                        value.push(c);
                        escaped = false;
                    }
                    (false, '\\') => escaped = true,
                    (false, '"') => break,
                    (false, c) => value.push(c),
                }
            }
        } else {
            while let Some(c) = chars.peek() {
                if *c == ',' {
                    break;
                }
                value.push(*c);
                chars.next();
            }
        }
        params.push((key.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    (scheme.to_ascii_lowercase(), params)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn digest_hash(algorithm: &str, data: &str) -> String {
    use md5::Digest as _;
    if algorithm.to_ascii_uppercase().starts_with("SHA-256") {
        use sha2::Digest as _;
        hex(&Sha256::digest(data.as_bytes()))
    } else {
        hex(&Md5::digest(data.as_bytes()))
    }
}

/// The realm the server's `WWW-Authenticate` headers name, if any: the
/// name of the protected area, shown when asking for a login.
pub fn realm(challenges: &[String]) -> Option<String> {
    challenges.iter().find_map(|h| {
        let (_, params) = parse_challenge(h);
        params.into_iter().find(|(k, v)| k == "realm" && !v.is_empty()).map(|(_, v)| v)
    })
}

/// The scheme to answer `challenges` with by NTLM, when the server offers
/// it and offers no Digest (which is preferred): `NTLM`, or `Negotiate`,
/// which Windows servers also accept NTLM messages under.
pub fn ntlm_scheme(challenges: &[String]) -> Option<&'static str> {
    let schemes: Vec<String> = challenges.iter().map(|h| parse_challenge(h).0).collect();
    if schemes.iter().any(|s| s == "digest") {
        return None;
    }
    if schemes.iter().any(|s| s == "ntlm") {
        Some("NTLM")
    } else if schemes.iter().any(|s| s == "negotiate") {
        Some("Negotiate")
    } else {
        None
    }
}

/// The server's NTLM challenge message in `challenges`, the 401 answering
/// a negotiate message sent under `scheme`.
pub fn ntlm_challenge(challenges: &[String], scheme: &str) -> Option<crate::ntlm::Challenge> {
    challenges.iter().find_map(|h| {
        let (name, token) = h.trim().split_once(' ')?;
        if !name.eq_ignore_ascii_case(scheme) {
            return None;
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(token.trim()).ok()?;
        crate::ntlm::parse_challenge(&bytes)
    })
}

/// The `Authorization` header answering `challenges` (the server's
/// `WWW-Authenticate` headers) for a `method` request to `url`, preferring
/// Digest. `cnonce` is the client nonce Digest needs.
pub fn authorization(challenges: &[String], login: &Login, method: &str, url: &str, cnonce: &str) -> Option<String> {
    let parsed: Vec<_> = challenges.iter().map(|h| parse_challenge(h)).collect();
    if let Some((_, params)) = parsed.iter().find(|(s, _)| s == "digest") {
        let get = |k: &str| params.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        let realm = get("realm").unwrap_or("");
        let nonce = get("nonce")?;
        let algorithm = get("algorithm").unwrap_or("MD5");
        let qop = get("qop").filter(|q| q.split(',').any(|o| o.trim() == "auth"));
        let uri = request_uri(url);
        let nc = "00000001";
        let mut ha1 = digest_hash(algorithm, &format!("{}:{realm}:{}", login.username, login.password));
        if algorithm.to_ascii_lowercase().ends_with("-sess") {
            ha1 = digest_hash(algorithm, &format!("{ha1}:{nonce}:{cnonce}"));
        }
        let ha2 = digest_hash(algorithm, &format!("{method}:{uri}"));
        let response = match qop {
            Some(_) => digest_hash(algorithm, &format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}")),
            None => digest_hash(algorithm, &format!("{ha1}:{nonce}:{ha2}")),
        };
        let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let mut header = format!(
            "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", algorithm={algorithm}, response=\"{response}\"",
            quote(&login.username),
            quote(realm),
            quote(nonce),
            quote(&uri)
        );
        if qop.is_some() {
            let _ = write!(header, ", qop=auth, nc={nc}, cnonce=\"{cnonce}\"");
        }
        if let Some(opaque) = get("opaque") {
            let _ = write!(header, ", opaque=\"{}\"", quote(opaque));
        }
        return Some(header);
    }
    if parsed.iter().any(|(s, _)| s == "basic") {
        let pair = format!("{}:{}", login.username, login.password);
        return Some(format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair)));
    }
    None
}

// --- Saved passwords: Windows Credential Manager, or an encrypted file ------

/// The Credential Manager entry for a site's login.
pub fn credential_target(site: &str) -> String {
    format!("UDM/login/{}", site.trim().to_ascii_lowercase())
}

/// Store a site login's password in Credential Manager.
pub fn save_login_password(site: &str, username: &str, password: &str) -> io::Result<()> {
    save_credential(&credential_target(site), username, password)
}

/// A site login's password from Credential Manager, if stored.
pub fn load_login_password(site: &str) -> Option<String> {
    load_saved(&credential_target(site))
}

/// Remove a site login's password, and the one Pullbay saved for it; a
/// missing one is not an error.
pub fn delete_login_password(site: &str) -> io::Result<()> {
    let target = credential_target(site);
    if let Some(old) = pullbay_target(&target) {
        delete_credential(&old)?;
    }
    delete_credential(&target)
}

/// The entry `target` had while UDM was called Pullbay (0.4), whose saved
/// passwords are still read.
fn pullbay_target(target: &str) -> Option<String> {
    target.strip_prefix("UDM/").map(|rest| format!("Pullbay/{rest}"))
}

/// The password saved under `target`, or under the name Pullbay gave it.
pub fn load_saved(target: &str) -> Option<String> {
    load_credential(target).or_else(|| pullbay_target(target).and_then(|old| load_credential(&old)))
}

pub use credentials::{delete_credential, load_credential, save_credential};

#[cfg(windows)]
mod credentials {
    use std::ffi::c_void;
    use std::io;
    use std::ptr;

    const CRED_TYPE_GENERIC: u32 = 1;
    const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;
    const ERROR_NOT_FOUND: i32 = 1168;

    #[repr(C)]
    struct Credential {
        flags: u32,
        kind: u32,
        target_name: *mut u16,
        comment: *mut u16,
        last_written: [u32; 2],
        blob_size: u32,
        blob: *mut u8,
        persist: u32,
        attribute_count: u32,
        attributes: *mut c_void,
        target_alias: *mut u16,
        user_name: *mut u16,
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        fn CredReadW(target: *const u16, kind: u32, flags: u32, credential: *mut *mut Credential) -> i32;
        fn CredDeleteW(target: *const u16, kind: u32, flags: u32) -> i32;
        fn CredFree(buffer: *mut c_void);
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Store `password` for `username` in Credential Manager under `target`.
    pub fn save_credential(target: &str, username: &str, password: &str) -> io::Result<()> {
        let mut target = wide(target);
        let mut user = wide(username);
        let mut blob: Vec<u8> = password.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let cred = Credential {
            flags: 0,
            kind: CRED_TYPE_GENERIC,
            target_name: target.as_mut_ptr(),
            comment: ptr::null_mut(),
            last_written: [0, 0],
            blob_size: blob.len() as u32,
            blob: blob.as_mut_ptr(),
            persist: CRED_PERSIST_LOCAL_MACHINE,
            attribute_count: 0,
            attributes: ptr::null_mut(),
            target_alias: ptr::null_mut(),
            user_name: user.as_mut_ptr(),
        };
        // SAFETY: every pointer refers to a live buffer for the call.
        if unsafe { CredWriteW(&cred, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The password Credential Manager keeps under `target`, if any.
    pub fn load_credential(target: &str) -> Option<String> {
        let target = wide(target);
        let mut cred: *mut Credential = ptr::null_mut();
        // SAFETY: on success `cred` points at a block freed with CredFree below.
        unsafe {
            if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) == 0 {
                return None;
            }
            let c = &*cred;
            let bytes = std::slice::from_raw_parts(c.blob, c.blob_size as usize);
            let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&b| u16::from_le_bytes(b)).collect();
            let password = String::from_utf16_lossy(&units);
            CredFree(cred as *mut c_void);
            Some(password)
        }
    }

    /// Remove what Credential Manager keeps under `target`; a missing entry is
    /// not an error.
    pub fn delete_credential(target: &str) -> io::Result<()> {
        let target = wide(target);
        // SAFETY: a valid string for the call.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(ERROR_NOT_FOUND) {
                return Err(e);
            }
        }
        Ok(())
    }
}

/// On macOS and Linux, the passwords live in `credentials.bin` in UDM's
/// data folder, a JSON map of entry to user name and password, encrypted
/// like the saved requests (`dpapi`).
#[cfg(unix)]
mod credentials {
    use std::collections::BTreeMap;
    use std::io;
    use std::sync::Mutex;

    use serde::{Deserialize, Serialize};
    use udm_domain::sync::LockExt;

    #[derive(Serialize, Deserialize)]
    struct Entry {
        username: String,
        password: String,
    }

    /// One change at a time, so two saves cannot lose each other's entry.
    static LOCK: Mutex<()> = Mutex::new(());

    fn path() -> io::Result<std::path::PathBuf> {
        Ok(crate::paths::data_dir()?.join("credentials.bin"))
    }

    fn read() -> io::Result<BTreeMap<String, Entry>> {
        match std::fs::read(path()?) {
            Ok(sealed) => {
                let plain = crate::dpapi::unprotect(&sealed)?;
                // The file holds saved logins, and serde's message could quote one.
                serde_json::from_slice(&plain).map_err(|e| crate::logging::json_error(&e))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e),
        }
    }

    fn write(all: &BTreeMap<String, Entry>) -> io::Result<()> {
        let plain = serde_json::to_vec(all).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let path = path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::platform::write_private(&path, &crate::dpapi::protect(&plain)?)
    }

    /// Store `password` for `username` under `target`.
    pub fn save_credential(target: &str, username: &str, password: &str) -> io::Result<()> {
        let _guard = LOCK.lock_or_recover();
        let mut all = read()?;
        all.insert(target.to_string(), Entry { username: username.to_string(), password: password.to_string() });
        write(&all)
    }

    /// The password kept under `target`, if any.
    pub fn load_credential(target: &str) -> Option<String> {
        let _guard = LOCK.lock_or_recover();
        read().ok()?.remove(target).map(|e| e.password)
    }

    /// Remove what is kept under `target`; a missing entry is not an error.
    pub fn delete_credential(target: &str) -> io::Result<()> {
        let _guard = LOCK.lock_or_recover();
        let mut all = read()?;
        if all.remove(target).is_some() {
            write(&all)?;
        }
        Ok(())
    }
}

// --- The proxies' passwords, encrypted with DPAPI -----------------------------

/// The proxies set by hand, each with its own login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    Http,
    Https,
    Ftp,
    Socks,
}

impl ProxyKind {
    pub const ALL: [ProxyKind; 4] = [ProxyKind::Http, ProxyKind::Https, ProxyKind::Ftp, ProxyKind::Socks];

    fn file_name(self) -> &'static str {
        match self {
            // The name is from when one login served every proxy.
            ProxyKind::Http => "proxy.bin",
            ProxyKind::Https => "proxy-https.bin",
            ProxyKind::Ftp => "proxy-ftp.bin",
            ProxyKind::Socks => "proxy-socks.bin",
        }
    }
}

/// Where the `kind` proxy's password is kept: `proxy.bin` for HTTP and
/// `proxy-<kind>.bin` for the others, in `%LOCALAPPDATA%\UDM`.
pub fn proxy_password_path(kind: ProxyKind) -> io::Result<PathBuf> {
    Ok(crate::history::history_path()?.with_file_name(kind.file_name()))
}

/// Give the HTTPS and SOCKS proxies the HTTP proxy's stored password, for
/// settings from before each proxy had its own login. A proxy that already
/// has a password keeps it.
pub fn split_proxy_password() -> io::Result<()> {
    let http = proxy_password_path(ProxyKind::Http)?;
    let password = load_proxy_password(&http);
    if password.is_empty() {
        return Ok(());
    }
    for kind in [ProxyKind::Https, ProxyKind::Socks] {
        let path = proxy_password_path(kind)?;
        if !path.exists() {
            save_proxy_password(&path, &password)?;
        }
    }
    Ok(())
}

/// Save the proxy password, or delete it when empty.
pub fn save_proxy_password(path: &Path, password: &str) -> io::Result<()> {
    if password.is_empty() {
        return match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    crate::settings::write_atomic(path, &dpapi::protect(password.as_bytes())?)
}

pub fn load_proxy_password(path: &Path) -> String {
    fs::read(path)
        .ok()
        .and_then(|sealed| dpapi::unprotect(&sealed).ok())
        .and_then(|plain| String::from_utf8(plain).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites_and_hosts() {
        assert!(site_matches("example.com", "example.com"));
        assert!(site_matches("example.com", "dl.example.com"));
        assert!(site_matches("*.example.com", "example.com"));
        assert!(site_matches("Example.COM:8080", "example.com"));
        assert!(!site_matches("example.com", "example.com.evil.net"));
        assert!(!site_matches("example.com", "notexample.com"));
        assert!(site_matches("dl*.mirror.net", "dl3.mirror.net"));
        assert!(!site_matches("dl*.mirror.net", "www.mirror.net"));
        assert_eq!(host_of("https://user:pw@Files.Example.com:8443/a?b#c"), "files.example.com");
        assert_eq!(host_of("http://[::1]:8080/"), "::1");
        assert_eq!(request_uri("https://x.com/dir/f.zip?a=1#frag"), "/dir/f.zip?a=1");
        assert_eq!(request_uri("https://x.com"), "/");
    }

    #[test]
    fn a_script_that_cannot_be_run_falls_back_to_the_fixed_proxy() {
        let rules = ProxyRules {
            http: Some("fixed:8080".into()),
            pac: Some(crate::pac::PacSource::Url("http://127.0.0.1:1/missing.pac".into())),
            ..ProxyRules::default()
        };
        assert_eq!(rules.proxy_for("http://a.com/f").as_deref(), Some("http://fixed:8080"));
    }

    #[test]
    fn bypass_lists_and_socks_versions() {
        assert_eq!(
            parse_bypass("<local>; *.corp.net 10.0.0.1,intranet"),
            ["<local>", "*.corp.net", "10.0.0.1", "intranet"]
        );
        assert!(parse_bypass("  ").is_empty());
        let rules = ProxyRules {
            socks: Some(format!("{}://s:1080", socks_scheme(false, true))),
            bypass: parse_bypass("*.corp.net"),
            ..ProxyRules::default()
        };
        assert_eq!(rules.proxy_for("https://a.com/").as_deref(), Some("socks4a://s:1080"));
        assert_eq!(rules.proxy_for("https://x.corp.net/"), None);
        assert_eq!(socks_scheme(false, false), "socks4");
        assert_eq!(socks_scheme(true, false), "socks5h");
        assert_eq!(socks_scheme(true, true), "socks5h");
        assert_eq!(remote_names("SOCKS5://u:p@s:1080".into()), "socks5h://u:p@s:1080");
        assert_eq!(remote_names("socks5h://s:1".into()), "socks5h://s:1");
        assert_eq!(remote_names("socks4://s:1".into()), "socks4://s:1");
        assert!(SocksProxy::parse("socks5h://s:1080", &ProxyLogin::default()).is_some_and(|p| p.v5 && p.remote_dns));
    }

    #[test]
    fn proxies_by_scheme_with_bypass_and_credentials() {
        let rules = ProxyRules {
            http: Some("proxy:3128".into()),
            https: Some("secure:3129".into()),
            socks: None,
            ftp: None,
            bypass: vec!["<local>".into(), "*.corp.net".into(), "10.0.0.1".into()],
            http_login: ProxyLogin::new("me@corp", "p:w/d"),
            https_login: ProxyLogin::new("tls", "pw"),
            socks_login: ProxyLogin::default(),
            socks_for_https: false,
            socks_for_ftp: false,
            pac: None,
        };
        assert_eq!(rules.proxy_for("http://a.com/f").as_deref(), Some("http://me%40corp:p%3Aw%2Fd@proxy:3128"));
        assert_eq!(rules.proxy_for("HTTPS://a.com/f").as_deref(), Some("http://tls:pw@secure:3129"));
        assert_eq!(rules.login_for("http://a.com/f").map(|l| l.username.as_str()), Some("me@corp"));
        assert_eq!(rules.login_for("http://intranet/f"), None);
        assert_eq!(rules.proxy_for("http://intranet/f"), None);
        assert_eq!(rules.proxy_for("https://files.corp.net/f"), None);
        assert_eq!(rules.proxy_for("http://10.0.0.1/f"), None);

        let socks = ProxyRules { socks: Some("socks5://s:1080".into()), ..ProxyRules::default() };
        assert_eq!(socks.proxy_for("https://a.com/").as_deref(), Some("socks5h://s:1080"));
        let https_only = ProxyRules { https: Some("s:1".into()), ..ProxyRules::default() };
        assert_eq!(https_only.proxy_for("http://a.com/"), None, "no proxy for http");
        assert_eq!(ProxyRules::default().proxy_for("http://a.com/"), None);
    }

    #[test]
    fn the_browsers_proxy_setting_maps_to_proxy_rules() {
        use crate::pac::PacSource;
        use udm_domain::download::{BrowserProxy, BrowserProxyMode};
        let p = |mode, f: &dyn Fn(&mut BrowserProxy)| {
            let mut p = BrowserProxy { mode, ..BrowserProxy::default() };
            f(&mut p);
            browser_proxy_rules(&p)
        };
        assert_eq!(p(BrowserProxyMode::Direct, &|_| {}), None);

        let fixed = p(BrowserProxyMode::Fixed, &|p| {
            p.http = Some("http://web.corp:3128".into());
            p.https = Some("http://web.corp:3129".into());
            p.bypass = vec!["<local>".into(), ".corp.net".into(), "http://intra:8080".into(), "10.0.0.0/8".into()];
        })
        .unwrap();
        assert_eq!(
            (fixed.http.as_deref(), fixed.https.as_deref(), fixed.socks.as_deref()),
            (Some("web.corp:3128"), Some("web.corp:3129"), None)
        );
        assert_eq!(fixed.bypass, ["<local>", "*.corp.net", "intra"]);
        assert_eq!(fixed.proxy_for("https://files.x/a"), Some("http://web.corp:3129".into()));
        assert_eq!(fixed.proxy_for("http://a.corp.net/a"), None, "bypassed");
        assert_eq!(fixed.proxy_for("http://intra/a"), None, "bypassed");

        let socks = p(BrowserProxyMode::Fixed, &|p| p.socks = Some("socks5://s:1080".into())).unwrap();
        assert_eq!(socks.proxy_for("https://x/a"), Some("socks5h://s:1080".into()));
        // Chrome's one SOCKS proxy for https only.
        let https_socks = p(BrowserProxyMode::Fixed, &|p| {
            p.http = Some("http://web:80".into());
            p.https = Some("socks4://s:1080".into());
        })
        .unwrap();
        assert_eq!(https_socks.proxy_for("https://x/a"), Some("socks4://s:1080".into()));
        assert_eq!(https_socks.proxy_for("http://x/a"), Some("http://web:80".into()));
        // An HTTPS proxy the engine cannot reach is no proxy.
        assert_eq!(p(BrowserProxyMode::Fixed, &|p| p.http = Some("https://p:443".into())), None);

        let script = p(BrowserProxyMode::Pac, &|p| p.pac_url = Some("http://wpad/proxy.pac".into())).unwrap();
        assert_eq!(script.pac, Some(PacSource::Url("http://wpad/proxy.pac".into())));
        let text =
            p(BrowserProxyMode::Pac, &|p| p.pac_script = Some("function FindProxyForURL(){return 'DIRECT'}".into()))
                .unwrap();
        assert!(matches!(text.pac, Some(PacSource::Script(_))));
        assert_eq!(p(BrowserProxyMode::Pac, &|p| p.pac_url = Some("file:///etc/p.pac".into())), None);
        let auto = p(BrowserProxyMode::AutoDetect, &|p| p.bypass = vec!["localhost".into()]).unwrap();
        assert_eq!((auto.pac, auto.bypass), (Some(PacSource::AutoDetect), vec!["localhost".to_string()]));
        assert_eq!(p(BrowserProxyMode::System, &|_| {}), system_proxy());
    }

    #[test]
    fn socks_carries_only_the_schemes_without_a_proxy() {
        let rules = ProxyRules {
            http: Some("web:8080".into()),
            socks: Some("socks5://s:1080".into()),
            http_login: ProxyLogin::new("web", "1"),
            socks_login: ProxyLogin::new("sock", "2"),
            ..ProxyRules::default()
        };
        assert_eq!(rules.proxy_for("http://a.com/").as_deref(), Some("http://web:1@web:8080"));
        assert_eq!(rules.proxy_for("https://a.com/").as_deref(), Some("socks5h://sock:2@s:1080"));
        assert_eq!(rules.login_for("https://a.com/").map(|l| l.username.as_str()), Some("sock"));
        let no_login = ProxyRules { socks_login: ProxyLogin::default(), ..rules };
        assert_eq!(no_login.login_for("https://a.com/"), None);
    }

    #[test]
    fn https_can_take_socks_while_http_takes_the_web_proxy() {
        let rules = ProxyRules {
            http: Some("web:8080".into()),
            https: Some("web:8080".into()),
            socks: Some("socks5://s:1080".into()),
            socks_for_https: true,
            ..ProxyRules::default()
        };
        assert_eq!(rules.proxy_for("http://a.com/").as_deref(), Some("http://web:8080"));
        assert_eq!(rules.proxy_for("https://a.com/").as_deref(), Some("socks5h://s:1080"));
        let off = ProxyRules { socks_for_https: false, ..rules.clone() };
        assert_eq!(off.proxy_for("https://a.com/").as_deref(), Some("http://web:8080"));
        // Without a SOCKS proxy, the setting changes nothing.
        let no_socks = ProxyRules { socks: None, ..rules };
        assert_eq!(no_socks.proxy_for("https://a.com/").as_deref(), Some("http://web:8080"));
    }

    #[test]
    fn ftp_takes_socks_only_when_asked() {
        let rules = ProxyRules {
            socks: Some("socks4a://s:1080".into()),
            socks_login: ProxyLogin::new("me", "pw"),
            bypass: parse_bypass("<local>"),
            ..ProxyRules::default()
        };
        assert_eq!(rules.socks_for_ftp("ftp://f.net/a"), None);
        let rules = ProxyRules { socks_for_ftp: true, ..rules };
        let socks = rules.socks_for_ftp("ftp://f.net/a").unwrap();
        assert_eq!((socks.addr.as_str(), socks.v5, socks.remote_dns), ("s:1080", false, true));
        assert_eq!(socks.login.username, "me");
        assert_eq!(rules.socks_for_ftp("ftp://intranet/a"), None, "bypassed");
        assert_eq!(SocksProxy::parse("http://x:1", &ProxyLogin::default()), None);
        assert!(SocksProxy::parse("x:1", &ProxyLogin::default()).unwrap().v5);
    }

    #[test]
    fn socks_connects_through_versions_4_4a_and_5() {
        use std::io::{Read, Write};
        let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = target.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut s in target.incoming().flatten() {
                let _ = s.write_all(b"hello");
            }
        });
        let timeout = std::time::Duration::from_secs(5);
        let read = |mut s: std::net::TcpStream| {
            let mut text = String::new();
            s.read_to_string(&mut text).unwrap();
            text
        };
        let cases = [
            ("socks5", ProxyLogin::new("ann", "pw")),
            ("socks5", ProxyLogin::default()),
            ("socks4a", ProxyLogin::new("ann", "")),
            ("socks4", ProxyLogin::default()),
        ];
        for (scheme, login) in cases {
            let wants_login = scheme == "socks5" && !login.is_empty();
            let server = socks_server::start(wants_login.then_some("ann:pw"));
            let proxy = SocksProxy::parse(&format!("{scheme}://{}", server.addr), &login).unwrap();
            let s = proxy
                .connect(&crate::route::Route::default(), "localhost", port, timeout)
                .unwrap_or_else(|e| panic!("{scheme}: {e}"));
            assert_eq!(read(s), "hello", "{scheme}");
            let seen = server.targets.lock().unwrap().clone();
            let want = if scheme == "socks4" { format!("127.0.0.1:{port}") } else { format!("localhost:{port}") };
            assert_eq!(seen, [want], "{scheme}");
        }
        // A wrong password is refused.
        let server = socks_server::start(Some("ann:pw"));
        let proxy = SocksProxy::parse(&format!("socks5://{}", server.addr), &ProxyLogin::new("ann", "no")).unwrap();
        let e = proxy.connect(&crate::route::Route::default(), "localhost", port, timeout).unwrap_err();
        assert!(e.to_string().contains("refused the login"), "{e}");
    }

    #[test]
    fn windows_proxy_setting() {
        let all = parse_proxy_server("proxy.corp:8080");
        assert_eq!((all.http.as_deref(), all.https.as_deref()), (Some("proxy.corp:8080"), Some("proxy.corp:8080")));
        let each = parse_proxy_server("http=h:1;https=s:2;socks=k:3;ftp=f:4");
        assert_eq!(each.http.as_deref(), Some("h:1"));
        assert_eq!(each.https.as_deref(), Some("s:2"));
        assert_eq!(each.socks.as_deref(), Some("socks4://k:3"));
        assert_eq!(each.ftp.as_deref(), Some("f:4"));
        assert_eq!(all.ftp, None, "one proxy for all is an HTTP proxy, not an FTP gateway");
        assert_eq!(parse_proxy_server(""), ProxyRules::default());
    }

    #[test]
    fn system_proxy_from_the_variables() {
        let vars = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
        };
        assert_eq!(proxy_from_env(vars(&[])), None);
        let rules = proxy_from_env(vars(&[
            ("http_proxy", "http://h:1/"),
            ("HTTPS_PROXY", "s:2"),
            ("no_proxy", "localhost, .corp.net"),
        ]))
        .unwrap();
        assert_eq!((rules.http.as_deref(), rules.https.as_deref()), (Some("h:1"), Some("s:2")));
        assert_eq!(rules.bypass, ["localhost", "*.corp.net"]);
        let socks = proxy_from_env(vars(&[("all_proxy", "socks5://k:3")])).unwrap();
        assert_eq!((socks.socks.as_deref(), socks.http), (Some("socks5://k:3"), None));
        let all = proxy_from_env(vars(&[("ALL_PROXY", "p:8"), ("http_proxy", "h:1")])).unwrap();
        assert_eq!((all.http.as_deref(), all.https.as_deref()), (Some("h:1"), Some("p:8")));
    }

    #[cfg(windows)]
    #[test]
    fn system_proxy_from_the_registry() {
        let key = r"Software\UDM-test-proxy";
        registry::set_dword(key, "ProxyEnable", 0).unwrap();
        registry::set_string(key, "ProxyServer", "http=h:1;https=s:2").unwrap();
        registry::set_string(key, "ProxyOverride", "<local>;*.corp.net").unwrap();
        assert_eq!(system_proxy_at(key), None, "off");
        registry::set_dword(key, "ProxyEnable", 1).unwrap();
        let rules = system_proxy_at(key).unwrap();
        assert_eq!(rules.https.as_deref(), Some("s:2"));
        assert_eq!(rules.bypass, ["<local>", "*.corp.net"]);
        for v in ["ProxyEnable", "ProxyServer", "ProxyOverride"] {
            registry::delete_value(key, v).unwrap();
        }
        assert_eq!(system_proxy_at(r"Software\UDM-test-missing"), None);
    }

    fn login(site: &str) -> Login {
        Login { site: site.into(), username: "Mufasa".into(), password: "Circle Of Life".into(), plain: false }
    }

    #[test]
    fn digest_matches_rfc_2617() {
        // The worked example in RFC 2617, section 3.5.
        let challenge = r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#;
        let header = authorization(
            &[challenge.into()],
            &login("host.com"),
            "GET",
            "http://www.nowhere.org/dir/index.html",
            "0a4f113b",
        )
        .unwrap();
        assert!(header.contains(r#"response="6629fae49393a05397450978507c4ef1""#), "{header}");
        assert!(header.contains("qop=auth, nc=00000001, cnonce=\"0a4f113b\""));
        assert!(header.contains(r#"opaque="5ccc069c403ebaf9f0171e9517f40e41""#));
        assert!(header.contains(r#"uri="/dir/index.html""#));
    }

    #[test]
    fn digest_sha256_and_basic() {
        // RFC 7616, section 3.9.1: SHA-256 for the same user.
        let challenge = r#"Digest realm="http-auth@example.org", qop="auth, auth-int", algorithm=SHA-256, nonce="7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v", opaque="FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS""#;
        // RFC 7616's example spells the password "Circle of Life".
        let rfc7616 = Login { password: "Circle of Life".into(), ..login("example.org") };
        let header = authorization(
            &[challenge.into()],
            &rfc7616,
            "GET",
            "http://www.example.org/dir/index.html",
            "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ",
        )
        .unwrap();
        assert!(
            header.contains(r#"response="753927fa0e85d155564e2e272a28d1802ca10daf4496794697cf8db5856cb6c1""#),
            "{header}"
        );

        assert_eq!(realm(&["Basic realm=\"Members, only\"".into()]).as_deref(), Some("Members, only"));
        assert_eq!(realm(&["Basic".into()]), None);
        let basic = authorization(&["Basic realm=\"x\"".into()], &login("x"), "GET", "http://x/", "c").unwrap();
        assert_eq!(basic, "Basic TXVmYXNhOkNpcmNsZSBPZiBMaWZl");
        // Digest wins when both are offered; nothing for an unknown scheme.
        let both = authorization(
            &["Basic realm=\"x\"".into(), r#"Digest realm="x", nonce="n""#.into()],
            &login("x"),
            "GET",
            "http://x/",
            "c",
        )
        .unwrap();
        assert!(both.starts_with("Digest "));
        assert_eq!(authorization(&["Negotiate".into()], &login("x"), "GET", "http://x/", "c"), None);
        // NTLM comes after Digest, before Basic, and Negotiate carries it.
        assert_eq!(ntlm_scheme(&["NTLM".into(), "Basic realm=\"x\"".into()]), Some("NTLM"));
        assert_eq!(ntlm_scheme(&["Negotiate".into()]), Some("Negotiate"));
        assert_eq!(ntlm_scheme(&["Digest realm=\"x\", nonce=\"n\"".into(), "NTLM".into()]), None);
        assert_eq!(ntlm_scheme(&["Basic realm=\"x\"".into()]), None);
    }

    #[test]
    fn the_most_specific_login_wins() {
        let logins = vec![login("example.com"), Login { username: "cdn".into(), ..login("cdn.example.com") }];
        assert_eq!(find_login(&logins, "https://cdn.example.com/a").unwrap().username, "cdn");
        assert_eq!(find_login(&logins, "https://www.example.com/a").unwrap().username, "Mufasa");
        assert!(find_login(&logins, "https://other.net/a").is_none());
    }

    #[test]
    fn a_login_can_be_for_a_folder_on_the_server() {
        let user = |site: &str, name: &str| Login { username: name.into(), ..login(site) };
        let logins = vec![
            user("example.com", "site"),
            user("example.com/private/*", "private"),
            user("example.com/private/staff", "staff"),
            user("files.example.com/members", "members"),
        ];
        let who = |url: &str| find_login(&logins, url).map(|l| l.username.as_str());
        assert_eq!(who("https://example.com/public/a.zip"), Some("site"));
        assert_eq!(who("https://example.com/private/a.zip?x=1"), Some("private"));
        assert_eq!(who("https://Example.com/PRIVATE/a.zip"), Some("private"), "case is ignored");
        assert_eq!(who("https://example.com/private/staff/b.zip"), Some("staff"), "the longest pattern wins");
        assert_eq!(who("https://example.com/private/staffroom.zip"), Some("private"), "a folder, not a prefix");
        assert_eq!(who("https://files.example.com/members"), Some("members"));
        assert_eq!(who("https://files.example.com/other"), Some("site"));
        assert_eq!(who("https://other.net/private/a.zip"), None);
        assert!(login_matches("example.com/", "https://example.com/x"));
        assert!(!login_matches("example.com/private/*", "https://example.com/private"));
        assert_eq!(split_login_pattern(" dl*.example.com/a/* "), ("dl*.example.com", "/a/*"));
    }

    #[test]
    fn passwords_are_kept_by_windows() {
        let site = "udm-test.example";
        save_login_password(site, "me", "s3cret — ü").unwrap();
        assert_eq!(load_login_password(site).as_deref(), Some("s3cret — ü"));
        delete_login_password(site).unwrap();
        assert_eq!(load_login_password(site), None);
        delete_login_password(site).unwrap();

        // A password Pullbay saved still loads, and removing it removes that one.
        save_credential(&format!("Pullbay/login/{site}"), "me", "old").unwrap();
        assert_eq!(load_login_password(site).as_deref(), Some("old"));
        delete_login_password(site).unwrap();
        assert_eq!(load_login_password(site), None);

        let path = std::env::temp_dir().join(format!("udm_proxy_{}.bin", std::process::id()));
        save_proxy_password(&path, "pr0xy").unwrap();
        assert!(!fs::read(&path).unwrap().windows(5).any(|w| w == b"pr0xy"));
        assert_eq!(load_proxy_password(&path), "pr0xy");
        save_proxy_password(&path, "").unwrap();
        assert!(!path.exists());
        assert_eq!(load_proxy_password(&path), "");
    }
}

/// A tiny SOCKS4/4a/5 proxy for tests: it relays each CONNECT to its target
/// and keeps the targets asked for, as `host:port`.
#[cfg(test)]
pub(crate) mod socks_server {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    pub struct SocksServer {
        pub addr: String,
        pub targets: Arc<Mutex<Vec<String>>>,
    }

    /// With `login` (`user:password`), SOCKS5 clients must log in with it.
    pub fn start(login: Option<&'static str>) -> SocksServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let targets = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&targets);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let seen = Arc::clone(&seen);
                thread::spawn(move || {
                    let _ = session(stream, login, &seen);
                });
            }
        });
        SocksServer { addr, targets }
    }

    fn byte(s: &mut TcpStream) -> std::io::Result<u8> {
        let mut b = [0u8; 1];
        s.read_exact(&mut b)?;
        Ok(b[0])
    }

    fn until_nul(s: &mut TcpStream) -> std::io::Result<String> {
        let mut out = Vec::new();
        loop {
            match byte(s)? {
                0 => return Ok(String::from_utf8_lossy(&out).into_owned()),
                b => out.push(b),
            }
        }
    }

    fn session(mut c: TcpStream, login: Option<&str>, seen: &Mutex<Vec<String>>) -> std::io::Result<()> {
        let target = match byte(&mut c)? {
            5 => {
                let n = byte(&mut c)? as usize;
                let mut methods = vec![0u8; n];
                c.read_exact(&mut methods)?;
                if let Some(login) = login {
                    if !methods.contains(&2) {
                        return c.write_all(&[5, 0xff]);
                    }
                    c.write_all(&[5, 2])?;
                    byte(&mut c)?;
                    let mut u = vec![0u8; byte(&mut c)? as usize];
                    c.read_exact(&mut u)?;
                    let mut p = vec![0u8; byte(&mut c)? as usize];
                    c.read_exact(&mut p)?;
                    let given = format!("{}:{}", String::from_utf8_lossy(&u), String::from_utf8_lossy(&p));
                    if given != login {
                        return c.write_all(&[1, 1]);
                    }
                    c.write_all(&[1, 0])?;
                } else {
                    c.write_all(&[5, 0])?;
                }
                let mut head = [0u8; 4];
                c.read_exact(&mut head)?;
                let host = match head[3] {
                    1 => {
                        let mut ip = [0u8; 4];
                        c.read_exact(&mut ip)?;
                        std::net::Ipv4Addr::from(ip).to_string()
                    }
                    3 => {
                        let mut name = vec![0u8; byte(&mut c)? as usize];
                        c.read_exact(&mut name)?;
                        String::from_utf8_lossy(&name).into_owned()
                    }
                    _ => return Ok(()),
                };
                let port = u16::from_be_bytes([byte(&mut c)?, byte(&mut c)?]);
                c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])?;
                format!("{host}:{port}")
            }
            4 => {
                byte(&mut c)?;
                let port = u16::from_be_bytes([byte(&mut c)?, byte(&mut c)?]);
                let mut ip = [0u8; 4];
                c.read_exact(&mut ip)?;
                until_nul(&mut c)?;
                let host = if ip[..3] == [0, 0, 0] && ip[3] != 0 {
                    until_nul(&mut c)?
                } else {
                    std::net::Ipv4Addr::from(ip).to_string()
                };
                c.write_all(&[0, 0x5a, 0, 0, 0, 0, 0, 0])?;
                format!("{host}:{port}")
            }
            _ => return Ok(()),
        };
        seen.lock().unwrap().push(target.clone());
        let upstream = TcpStream::connect(target.as_str())?;
        let (mut up_r, mut c_w) = (upstream.try_clone()?, c.try_clone()?);
        let down = thread::spawn(move || {
            let _ = std::io::copy(&mut up_r, &mut c_w);
            let _ = c_w.shutdown(Shutdown::Write);
        });
        let (mut c_r, mut up_w) = (c, upstream);
        let _ = std::io::copy(&mut c_r, &mut up_w);
        let _ = up_w.shutdown(Shutdown::Write);
        let _ = down.join();
        Ok(())
    }
}
