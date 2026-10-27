//! Proxy auto-config: which proxy a PAC script picks for a URL. WinHTTP
//! fetches and runs the script (`WinHttpGetProxyForUrl`), from a URL or,
//! for WPAD, from wherever the network says. Answers are cached per host
//! for a while, so each request does not run the script again.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Where the script comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PacSource {
    /// A script at this address.
    Url(String),
    /// Found by WPAD (DHCP, then DNS).
    AutoDetect,
}

/// No script could be found, fetched or run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoScript;

/// How long one host's answer is kept.
const CACHE_FOR: Duration = Duration::from_secs(10 * 60);

const WINHTTP_ACCESS_TYPE_NO_PROXY: u32 = 1;
const WINHTTP_ACCESS_TYPE_NAMED_PROXY: u32 = 3;
const WINHTTP_AUTOPROXY_AUTO_DETECT: u32 = 0x1;
const WINHTTP_AUTOPROXY_CONFIG_URL: u32 = 0x2;
const WINHTTP_AUTO_DETECT_TYPE_DHCP: u32 = 0x1;
const WINHTTP_AUTO_DETECT_TYPE_DNS_A: u32 = 0x2;

#[repr(C)]
struct AutoProxyOptions {
    flags: u32,
    auto_detect_flags: u32,
    auto_config_url: *const u16,
    reserved: *mut c_void,
    reserved2: u32,
    auto_logon_if_challenged: i32,
}

#[repr(C)]
struct ProxyInfo {
    access_type: u32,
    proxy: *mut u16,
    proxy_bypass: *mut u16,
}

#[repr(C)]
struct IeProxyConfig {
    auto_detect: i32,
    auto_config_url: *mut u16,
    proxy: *mut u16,
    proxy_bypass: *mut u16,
}

#[link(name = "winhttp")]
extern "system" {
    fn WinHttpOpen(agent: *const u16, access: u32, proxy: *const u16, bypass: *const u16, flags: u32) -> *mut c_void;
    fn WinHttpCloseHandle(handle: *mut c_void) -> i32;
    fn WinHttpGetProxyForUrl(
        session: *mut c_void,
        url: *const u16,
        options: *const AutoProxyOptions,
        info: *mut ProxyInfo,
    ) -> i32;
    fn WinHttpGetIEProxyConfigForCurrentUser(config: *mut IeProxyConfig) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GlobalFree(mem: *mut c_void) -> *mut c_void;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read and free a string WinHTTP handed out.
///
/// SAFETY: `p` is null or a NUL-terminated string from WinHTTP that no one
/// else frees.
unsafe fn take(p: *mut u16) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut len = 0;
    while *p.add(len) != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
    GlobalFree(p as *mut c_void);
    Some(s)
}

/// The script Windows' Internet settings point at, if any: a configuration
/// script URL, or automatic detection.
pub fn windows_source() -> Option<PacSource> {
    let mut c = IeProxyConfig {
        auto_detect: 0,
        auto_config_url: ptr::null_mut(),
        proxy: ptr::null_mut(),
        proxy_bypass: ptr::null_mut(),
    };
    // SAFETY: fills `c`; its strings are freed by `take`.
    if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut c) } == 0 {
        return None;
    }
    let (url, _, _) = unsafe { (take(c.auto_config_url), take(c.proxy), take(c.proxy_bypass)) };
    match url.filter(|u| !u.trim().is_empty()) {
        Some(u) => Some(PacSource::Url(u.trim().to_string())),
        None => (c.auto_detect != 0).then_some(PacSource::AutoDetect),
    }
}

/// The proxy WinHTTP's first answer names, as a URL ureq understands.
/// `None` connects directly.
fn to_proxy_url(list: &str) -> Option<String> {
    let first = list.split([';', ' ']).map(str::trim).find(|s| !s.is_empty())?;
    if first.eq_ignore_ascii_case("direct") {
        return None;
    }
    if let Some((scheme, addr)) = first.split_once('=') {
        // `http=host:port` style, as WinHTTP gives for some scripts.
        return Some(match scheme.to_ascii_lowercase().as_str() {
            "socks" => format!("socks4://{addr}"),
            _ => format!("http://{addr}"),
        });
    }
    if first.contains("://") {
        return Some(first.to_string());
    }
    Some(format!("http://{first}"))
}

/// Ask WinHTTP, without the cache. `Err` when no script could be found or
/// run, so the caller can fall back.
fn ask(source: &PacSource, url: &str) -> Result<Option<String>, NoScript> {
    let agent = wide("UIDM");
    // SAFETY: plain calls with valid strings; the session is closed below.
    let session = unsafe { WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_NO_PROXY, ptr::null(), ptr::null(), 0) };
    if session.is_null() {
        return Err(NoScript);
    }
    let script = match source {
        PacSource::Url(u) => Some(wide(u)),
        PacSource::AutoDetect => None,
    };
    let options = AutoProxyOptions {
        flags: if script.is_some() { WINHTTP_AUTOPROXY_CONFIG_URL } else { WINHTTP_AUTOPROXY_AUTO_DETECT },
        auto_detect_flags: if script.is_some() { 0 } else { WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A },
        auto_config_url: script.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        reserved: ptr::null_mut(),
        reserved2: 0,
        auto_logon_if_challenged: 1,
    };
    let target = wide(url);
    let mut info = ProxyInfo {
        access_type: 0,
        proxy: ptr::null_mut(),
        proxy_bypass: ptr::null_mut(),
    };
    // SAFETY: every pointer is valid for the call; strings in `info` are
    // freed by `take`.
    let ok = unsafe { WinHttpGetProxyForUrl(session, target.as_ptr(), &options, &mut info) };
    let (proxy, _) = unsafe { (take(info.proxy), take(info.proxy_bypass)) };
    unsafe { WinHttpCloseHandle(session) };
    if ok == 0 {
        return Err(NoScript);
    }
    Ok(match info.access_type {
        WINHTTP_ACCESS_TYPE_NAMED_PROXY => proxy.as_deref().and_then(to_proxy_url),
        WINHTTP_ACCESS_TYPE_NO_PROXY => None,
        _ => None,
    })
}

type Cache = HashMap<(PacSource, String), (Result<Option<String>, NoScript>, Instant)>;

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// The proxy the script picks for `url`: `Ok(None)` for direct, `Err` when
/// there is no usable script. Cached per host for ten minutes.
pub fn proxy_for(source: &PacSource, url: &str) -> Result<Option<String>, NoScript> {
    // Automatic detection that found no script is not tried again for
    // other hosts: on a network without WPAD each try takes seconds.
    let any_host = (source.clone(), String::new());
    if *source == PacSource::AutoDetect {
        if let Some((Err(NoScript), at)) = CACHE.lock().unwrap().get_or_insert_with(HashMap::new).get(&any_host) {
            if at.elapsed() < CACHE_FOR {
                return Err(NoScript);
            }
        }
    }
    let key = (source.clone(), crate::net::host_of(url));
    if let Some((answer, at)) = CACHE.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        if at.elapsed() < CACHE_FOR {
            return answer.clone();
        }
    }
    let answer = ask(source, url);
    let mut cache = CACHE.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    if answer.is_err() && *source == PacSource::AutoDetect {
        cache.insert(any_host, (Err(NoScript), Instant::now()));
    }
    cache.insert(key, (answer.clone(), Instant::now()));
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn proxy_lists_become_proxy_urls() {
        assert_eq!(to_proxy_url("proxy.test:3128").as_deref(), Some("http://proxy.test:3128"));
        assert_eq!(to_proxy_url("a:1; b:2").as_deref(), Some("http://a:1"));
        assert_eq!(to_proxy_url("socks=s:1080").as_deref(), Some("socks4://s:1080"));
        assert_eq!(to_proxy_url("DIRECT"), None);
        assert_eq!(to_proxy_url(""), None);
    }

    #[test]
    fn a_served_script_picks_the_proxy_and_is_cached() {
        const SCRIPT: &str = "function FindProxyForURL(url, host) {\n  if (host == \"direct.test\") return \"DIRECT\";\n  return \"PROXY proxy.test:3128\";\n}\n";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = std::sync::Arc::clone(&served);
        thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    SCRIPT.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(SCRIPT.as_bytes());
            }
        });
        let source = PacSource::Url(format!("http://{addr}/proxy.pac"));
        assert_eq!(proxy_for(&source, "http://files.test/a.zip"), Ok(Some("http://proxy.test:3128".into())));
        assert_eq!(proxy_for(&source, "https://direct.test/b.zip"), Ok(None));
        let fetched = served.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(proxy_for(&source, "http://files.test/other.zip"), Ok(Some("http://proxy.test:3128".into())));
        assert_eq!(served.load(std::sync::atomic::Ordering::Relaxed), fetched, "the host's answer was cached");
        let missing = PacSource::Url("http://127.0.0.1:1/none.pac".into());
        assert!(proxy_for(&missing, "http://files.test/a.zip").is_err());
    }
}
