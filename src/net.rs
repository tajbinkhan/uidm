//! How downloads reach servers: through which proxy, and with which login.
//!
//! - Proxies come from the settings: none, the system proxy (the Windows
//!   Internet settings, with their bypass list and any configuration script
//!   or automatic detection), a configuration script, or ones set by hand. A
//!   captured download can also use the proxy the browser reported.
//! - Site logins pair a site pattern, optionally with a path, with a
//!   username; the password lives in Windows Credential Manager. When a
//!   server answers 401, the engine sends the matching login with HTTP Basic
//!   or Digest, to the host that asked and to no other.
//!
//! Each proxy's password is kept apart from the settings, encrypted with
//! DPAPI.

use std::ffi::c_void;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr;

use base64::Engine as _;
use md5::Md5;
use sha2::Sha256;

use crate::dpapi;
use crate::registry;

/// Where the Windows Internet settings keep the system proxy.
pub const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

/// Whether `host` belongs to the site `pattern`. `example.com` and
/// `*.example.com` both cover example.com and every subdomain; any other `*`
/// matches within the host name. Case and a port in the pattern are ignored.
pub fn site_matches(pattern: &str, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let p = pattern.trim().to_ascii_lowercase();
    let p = match p.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h.to_string(),
        _ => p,
    };
    if p.is_empty() || host.is_empty() {
        return false;
    }
    if let Some(base) = p.strip_prefix("*.") {
        if !base.contains('*') {
            return host == base || host.ends_with(&format!(".{base}"));
        }
    }
    if p.contains('*') {
        return glob(&p, &host);
    }
    host == p || host.ends_with(&format!(".{p}"))
}

/// `*` matches any run of characters; everything else matches itself.
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut rest = text;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
    }
    rest.is_empty()
}

/// The host of an http(s) URL, lowercase, without port or credentials.
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(v6) = host_port.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    host.to_ascii_lowercase()
}

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
        ProxyLogin {
            username: username.to_string(),
            password: password.to_string(),
        }
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
    /// Hosts reached directly: patterns with `*`, and `<local>` for names
    /// without a dot.
    pub bypass: Vec<String>,
    pub http_login: ProxyLogin,
    pub https_login: ProxyLogin,
    pub socks_login: ProxyLogin,
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

/// The ureq scheme for a SOCKS proxy: SOCKS5 always lets the proxy look up
/// names; SOCKS4 does only as SOCKS4a.
pub fn socks_scheme(v5: bool, remote_dns: bool) -> &'static str {
    match (v5, remote_dns) {
        (true, _) => "socks5",
        (false, true) => "socks4a",
        (false, false) => "socks4",
    }
}

impl ProxyRules {
    /// The proxy for `url`, as a URL ureq understands, or `None` to connect
    /// directly.
    pub fn proxy_for(&self, url: &str) -> Option<String> {
        let host = host_of(url);
        if self.bypassed(&host) {
            return None;
        }
        if let Some(Ok(answer)) = self.pac.as_ref().map(|source| crate::pac::proxy_for(source, url)) {
            return answer;
        }
        let (scheme, addr, login) = self.fixed_for(url)?;
        Some(format!("{scheme}://{}{addr}", login.userinfo()))
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
    /// proxy for the URL's own scheme, else the SOCKS proxy.
    fn fixed_for(&self, url: &str) -> Option<(&str, &str, &ProxyLogin)> {
        let https = url.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://"));
        let own = if https {
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

/// Read Windows' proxy setting: `host:port` for everything, or
/// `http=host:port;https=host:port;socks=host:port` per scheme. Windows'
/// SOCKS proxies speak SOCKS4.
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
            "socks" => rules.socks = Some(format!("socks4://{addr}")),
            _ => {}
        }
    }
    rules
}

/// The system proxy from the Internet settings under `key`, if one is on.
/// Automatic configuration scripts are not read.
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

/// The Windows proxy setting: its fixed proxies, and its configuration
/// script or automatic detection, which comes first.
pub fn system_proxy() -> Option<ProxyRules> {
    let fixed = system_proxy_at(INTERNET_SETTINGS);
    match crate::pac::windows_source() {
        Some(source) => Some(ProxyRules {
            pac: Some(source),
            ..fixed.unwrap_or_default()
        }),
        None => fixed,
    }
}

/// A login for the sites matching `site`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub site: String,
    pub username: String,
    pub password: String,
}

/// A login pattern split into its site and its path (`""` for none):
/// `example.com/private/*` is `example.com` and `/private/*`.
pub fn split_login_pattern(pattern: &str) -> (&str, &str) {
    let p = pattern.trim();
    match p.find('/') {
        Some(i) => (&p[..i], &p[i..]),
        None => (p, ""),
    }
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
    logins
        .iter()
        .filter(|l| login_matches(&l.site, url))
        .max_by_key(|l| l.site.trim().trim_start_matches("*.").len())
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

// --- Windows Credential Manager, for site login passwords --------------------

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

/// The Credential Manager entry for a site's login.
pub fn credential_target(site: &str) -> String {
    format!("UIDM/login/{}", site.trim().to_ascii_lowercase())
}

/// Store a site login's password in Credential Manager.
pub fn save_login_password(site: &str, username: &str, password: &str) -> io::Result<()> {
    let mut target = wide(&credential_target(site));
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

/// A site login's password from Credential Manager, if stored.
pub fn load_login_password(site: &str) -> Option<String> {
    let target = wide(&credential_target(site));
    let mut cred: *mut Credential = ptr::null_mut();
    // SAFETY: on success `cred` points at a block freed with CredFree below.
    unsafe {
        if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) == 0 {
            return None;
        }
        let c = &*cred;
        let bytes = std::slice::from_raw_parts(c.blob, c.blob_size as usize);
        let units: Vec<u16> = bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        let password = String::from_utf16_lossy(&units);
        CredFree(cred as *mut c_void);
        Some(password)
    }
}

/// Remove a site login's password; a missing one is not an error.
pub fn delete_login_password(site: &str) -> io::Result<()> {
    let target = wide(&credential_target(site));
    // SAFETY: a valid string for the call.
    if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(ERROR_NOT_FOUND) {
            return Err(e);
        }
    }
    Ok(())
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
/// `proxy-<kind>.bin` for the others, in `%LOCALAPPDATA%\UIDM`.
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
        assert_eq!(parse_bypass("<local>; *.corp.net 10.0.0.1,intranet"), ["<local>", "*.corp.net", "10.0.0.1", "intranet"]);
        assert!(parse_bypass("  ").is_empty());
        let rules = ProxyRules {
            socks: Some(format!("{}://s:1080", socks_scheme(false, true))),
            bypass: parse_bypass("*.corp.net"),
            ..ProxyRules::default()
        };
        assert_eq!(rules.proxy_for("https://a.com/").as_deref(), Some("socks4a://s:1080"));
        assert_eq!(rules.proxy_for("https://x.corp.net/"), None);
        assert_eq!(socks_scheme(false, false), "socks4");
        assert_eq!(socks_scheme(true, false), "socks5");
    }

    #[test]
    fn proxies_by_scheme_with_bypass_and_credentials() {
        let rules = ProxyRules {
            http: Some("proxy:3128".into()),
            https: Some("secure:3129".into()),
            socks: None,
            bypass: vec!["<local>".into(), "*.corp.net".into(), "10.0.0.1".into()],
            http_login: ProxyLogin::new("me@corp", "p:w/d"),
            https_login: ProxyLogin::new("tls", "pw"),
            socks_login: ProxyLogin::default(),
            pac: None,
        };
        assert_eq!(rules.proxy_for("http://a.com/f").as_deref(), Some("http://me%40corp:p%3Aw%2Fd@proxy:3128"));
        assert_eq!(rules.proxy_for("HTTPS://a.com/f").as_deref(), Some("http://tls:pw@secure:3129"));
        assert_eq!(rules.login_for("http://a.com/f").map(|l| l.username.as_str()), Some("me@corp"));
        assert_eq!(rules.login_for("http://intranet/f"), None);
        assert_eq!(rules.proxy_for("http://intranet/f"), None);
        assert_eq!(rules.proxy_for("https://files.corp.net/f"), None);
        assert_eq!(rules.proxy_for("http://10.0.0.1/f"), None);

        let socks = ProxyRules {
            socks: Some("socks5://s:1080".into()),
            ..ProxyRules::default()
        };
        assert_eq!(socks.proxy_for("https://a.com/").as_deref(), Some("socks5://s:1080"));
        let https_only = ProxyRules {
            https: Some("s:1".into()),
            ..ProxyRules::default()
        };
        assert_eq!(https_only.proxy_for("http://a.com/"), None, "no proxy for http");
        assert_eq!(ProxyRules::default().proxy_for("http://a.com/"), None);
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
        assert_eq!(rules.proxy_for("https://a.com/").as_deref(), Some("socks5://sock:2@s:1080"));
        assert_eq!(rules.login_for("https://a.com/").map(|l| l.username.as_str()), Some("sock"));
        let no_login = ProxyRules {
            socks_login: ProxyLogin::default(),
            ..rules
        };
        assert_eq!(no_login.login_for("https://a.com/"), None);
    }

    #[test]
    fn windows_proxy_setting() {
        let all = parse_proxy_server("proxy.corp:8080");
        assert_eq!((all.http.as_deref(), all.https.as_deref()), (Some("proxy.corp:8080"), Some("proxy.corp:8080")));
        let each = parse_proxy_server("http=h:1;https=s:2;socks=k:3;ftp=f:4");
        assert_eq!(each.http.as_deref(), Some("h:1"));
        assert_eq!(each.https.as_deref(), Some("s:2"));
        assert_eq!(each.socks.as_deref(), Some("socks4://k:3"));
        assert_eq!(parse_proxy_server(""), ProxyRules::default());
    }

    #[test]
    fn system_proxy_from_the_registry() {
        let key = r"Software\UIDM-test-proxy";
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
        assert_eq!(system_proxy_at(r"Software\UIDM-test-missing"), None);
    }

    fn login(site: &str) -> Login {
        Login {
            site: site.into(),
            username: "Mufasa".into(),
            password: "Circle Of Life".into(),
        }
    }

    #[test]
    fn digest_matches_rfc_2617() {
        // The worked example in RFC 2617, section 3.5.
        let challenge = r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#;
        let header = authorization(&[challenge.into()], &login("host.com"), "GET", "http://www.nowhere.org/dir/index.html", "0a4f113b").unwrap();
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
        let rfc7616 = Login {
            password: "Circle of Life".into(),
            ..login("example.org")
        };
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
        let both = authorization(&["Basic realm=\"x\"".into(), r#"Digest realm="x", nonce="n""#.into()], &login("x"), "GET", "http://x/", "c").unwrap();
        assert!(both.starts_with("Digest "));
        assert_eq!(authorization(&["Negotiate".into()], &login("x"), "GET", "http://x/", "c"), None);
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
        let site = "uidm-test.example";
        save_login_password(site, "me", "s3cret — ü").unwrap();
        assert_eq!(load_login_password(site).as_deref(), Some("s3cret — ü"));
        delete_login_password(site).unwrap();
        assert_eq!(load_login_password(site), None);
        delete_login_password(site).unwrap();

        let path = std::env::temp_dir().join(format!("uidm_proxy_{}.bin", std::process::id()));
        save_proxy_password(&path, "pr0xy").unwrap();
        assert!(!fs::read(&path).unwrap().windows(5).any(|w| w == b"pr0xy"));
        assert_eq!(load_proxy_password(&path), "pr0xy");
        save_proxy_password(&path, "").unwrap();
        assert!(!path.exists());
        assert_eq!(load_proxy_password(&path), "");
    }
}
