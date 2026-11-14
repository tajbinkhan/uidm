//! The proxy and logins a download connects with (`config_for`), from the
//! settings, the download's own login and the proxy its browser reported,
//! and the network adapter and resolver its connections go out by
//! (`Network::route`).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};
use udm_domain::sync::LockExt;

use udm_domain::download::Download;
use udm_domain::network::DnsChoice;
use udm_domain::ports::NetworkAdapters;

use crate::doh::Doh;
use crate::route::{Binding, Route};

use crate::net::{self, Login, NetConfig, ProxyKind, ProxyLogin};
use udm_domain::settings::{ProxyMode, Settings, SocksVersion};

/// Addresses retried through the browser's proxy after failing through
/// UDM's own proxy setting; they keep using it.
#[derive(Default)]
pub struct ProxyRetries(Mutex<HashSet<String>>);

impl ProxyRetries {
    /// Only single inserts change the set, so a lock poisoned by a
    /// panicking thread still holds a usable one.
    fn urls(&self) -> MutexGuard<'_, HashSet<String>> {
        self.0.lock_or_recover()
    }

    /// Whether `url` was retried through the browser's proxy.
    pub fn contains(&self, url: &str) -> bool {
        self.urls().contains(url)
    }

    /// Note that `url` is retried through the browser's proxy. Returns false
    /// when it already was, so each address gets one such retry.
    pub fn insert(&self, url: String) -> bool {
        self.urls().insert(url)
    }
}

/// What every download's connections share: the browser-proxy retries, and
/// the network adapters with the DNS over HTTPS resolver.
#[derive(Default)]
pub struct NetState {
    pub retries: ProxyRetries,
    pub network: Network,
}

impl NetState {
    /// The same, with the adapters from `adapters`: for tests.
    pub fn with_adapters(adapters: Arc<dyn NetworkAdapters>) -> NetState {
        NetState { retries: ProxyRetries::default(), network: Network::new(adapters) }
    }
}

/// The computer's network adapters, and the DNS over HTTPS resolver the
/// settings choose, kept so its answers are shared by every download.
pub struct Network {
    adapters: Arc<dyn NetworkAdapters>,
    /// The resolver made for a choice and the adapter it is bound to.
    doh: Mutex<Option<KeptResolver>>,
}

/// A DNS over HTTPS resolver, with the choice and the adapter it was made
/// for.
type KeptResolver = (DnsChoice, Option<udm_domain::network::AdapterId>, Arc<Doh>);

impl Default for Network {
    fn default() -> Self {
        Network::new(Arc::new(crate::adapters::SystemAdapters))
    }
}

impl Network {
    pub fn new(adapters: Arc<dyn NetworkAdapters>) -> Network {
        Network { adapters, doh: Mutex::new(None) }
    }

    /// The adapters, for the window's list.
    pub fn adapters(&self) -> &Arc<dyn NetworkAdapters> {
        &self.adapters
    }

    /// How connections go out under `s`: bound to its adapter, if one is
    /// chosen, and with its resolver.
    pub fn route(&self, s: &Settings) -> Route {
        let binding = s.network_adapter.clone().map(|choice| Binding::new(choice, Arc::clone(&self.adapters)));
        let dns = s.dns.endpoint().map(|endpoint| {
            let bound = s.network_adapter.as_ref().map(|c| c.id.clone());
            let mut kept = self.doh.lock_or_recover();
            match kept.as_ref() {
                Some((dns, id, doh)) if *dns == s.dns && *id == bound => Arc::clone(doh),
                _ => {
                    let doh = Arc::new(Doh::new(endpoint, binding.clone()));
                    *kept = Some((s.dns.clone(), bound, Arc::clone(&doh)));
                    doh
                }
            }
        });
        Route { binding, dns }
    }
}

/// The proxy and logins for `download`. A download from the browser uses
/// the browser's proxy when the settings say so: always, or once it failed
/// through UDM's own setting (`state.retries`). Its connections go out by
/// the network adapter and resolver the settings choose; while bound to an
/// adapter, no proxy configuration script is used, since the system fetches
/// and runs those itself, unbound (the proxies set by hand still apply).
pub fn config_for(download: &Download, s: &Settings, state: &NetState) -> NetConfig {
    let retries = &state.retries;
    let browser_wanted =
        s.use_browser_proxy && (!s.browser_proxy_after_errors || retries.contains(download.url.as_str()));
    // The browser's whole setting when it sent one (its direct
    // connection too, as `Some(None)`), else the fixed proxy an older
    // extension sends.
    let browser = match (&download.browser_proxy, download.proxy.as_deref()) {
        _ if !browser_wanted => None,
        (Some(setting), _) => Some(net::browser_proxy_rules(setting)),
        (None, Some(p)) => Some(Some(net::ProxyRules {
            http: p.strip_prefix("http://").map(str::to_string),
            https: p.strip_prefix("http://").map(str::to_string),
            socks: p.starts_with("socks").then(|| p.to_string()),
            ..net::ProxyRules::default()
        })),
        (None, None) => None,
    };
    let addr = |a: &str| (!a.trim().is_empty()).then(|| a.trim().to_string());
    let proxy_login = |user: &str, kind: ProxyKind| {
        if user.trim().is_empty() {
            return ProxyLogin::default();
        }
        let password = net::proxy_password_path(kind).map(|p| net::load_proxy_password(&p)).unwrap_or_default();
        ProxyLogin::new(user.trim(), &password)
    };
    let proxy = browser.unwrap_or_else(|| match s.proxy_mode {
        ProxyMode::None => None,
        ProxyMode::System => net::system_proxy(),
        ProxyMode::Script => Some(net::ProxyRules {
            pac: Some(crate::pac::PacSource::Url(s.proxy_pac_url.trim().to_string())),
            ..net::ProxyRules::default()
        }),
        ProxyMode::Manual => {
            // An empty HTTPS proxy means the HTTP one, with its login.
            let (https, https_login) = match addr(&s.proxy_https) {
                Some(a) => (Some(a), proxy_login(&s.proxy_https_username, ProxyKind::Https)),
                None => (addr(&s.proxy_http), proxy_login(&s.proxy_username, ProxyKind::Http)),
            };
            Some(net::ProxyRules {
                http: addr(&s.proxy_http),
                https,
                socks: addr(&s.proxy_socks5).map(|a| {
                    let scheme = net::socks_scheme(s.proxy_socks_version == SocksVersion::V5, s.proxy_remote_dns);
                    format!("{scheme}://{a}")
                }),
                // ftp:// goes through `NetConfig::ftp_proxy`, below.
                ftp: None,
                bypass: net::parse_bypass(&s.proxy_bypass),
                http_login: proxy_login(&s.proxy_username, ProxyKind::Http),
                https_login,
                socks_login: proxy_login(&s.proxy_socks_username, ProxyKind::Socks),
                socks_for_https: s.proxy_socks_https,
                socks_for_ftp: s.proxy_socks_ftp,
                pac: None,
            })
        }
    });
    let route = state.network.route(s);
    let mut proxy = proxy;
    if route.binding.is_some() {
        if let Some(rules) = proxy.as_mut() {
            rules.pac = None;
        }
    }
    let manual_ftp = (s.proxy_mode == ProxyMode::Manual).then(|| addr(&s.proxy_ftp)).flatten();
    let mut logins: Vec<Login> = s
        .site_logins
        .iter()
        .filter_map(|l| {
            Some(Login {
                site: l.site.trim().to_string(),
                username: l.username.clone(),
                password: net::load_login_password(&l.site)?,
                plain: l.plain,
            })
        })
        .collect();
    // The download's own login takes the place of the site logins for
    // its host, even those for one of its folders.
    let host = url::Url::parse(download.url.as_str()).ok().and_then(|u| u.host_str().map(str::to_string));
    if let (Some(l), Some(host)) = (&download.login, host) {
        logins.retain(|s| !udm_domain::site::site_matches(udm_domain::site::split_login_pattern(&s.site).0, &host));
        logins.push(Login {
            site: host,
            username: l.username.clone(),
            password: l.password.clone().unwrap_or_default(),
            // Typed for this download, so meant for its address.
            plain: true,
        });
    }
    NetConfig {
        proxy,
        logins,
        ftp_active: s.ftp_active,
        ftp_proxy_login: manual_ftp
            .as_ref()
            .map_or_else(ProxyLogin::default, |_| proxy_login(&s.proxy_ftp_username, ProxyKind::Ftp)),
        ftp_proxy: manual_ftp,
        route,
    }
}

impl udm_domain::ports::ProxyFallback for NetState {
    fn claim_retry(&self, url: udm_domain::address::Link) -> bool {
        self.retries.insert(url.into_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_browsers_proxy_can_wait_for_a_failure() {
        let d = Download { url: "https://x/a.zip".into(), proxy: Some("http://bp:3128".into()), ..Download::default() };
        let state = NetState::default();
        let retries = &state.retries;
        let through = |s: &Settings| config_for(&d, s, &state).proxy.and_then(|p| p.proxy_for(d.url.as_str()));
        let after = Settings { use_browser_proxy: true, browser_proxy_after_errors: true, ..Settings::default() };
        assert_eq!(through(&after), None, "not before it failed");
        let always = Settings { browser_proxy_after_errors: false, ..after.clone() };
        assert_eq!(through(&always).as_deref(), Some("http://bp:3128"));
        assert!(retries.insert(d.url.to_string()));
        assert!(!retries.insert(d.url.to_string()), "one retry each");
        assert_eq!(through(&after).as_deref(), Some("http://bp:3128"), "once it failed");
    }

    #[test]
    fn the_browsers_whole_proxy_setting_is_used() {
        use udm_domain::download::{BrowserProxy, BrowserProxyMode};
        let s = Settings {
            use_browser_proxy: true,
            proxy_mode: ProxyMode::Manual,
            proxy_http: "udm:8080".into(),
            ..Settings::default()
        };
        let through = |setting: Option<BrowserProxy>, s: &Settings| {
            let d = Download {
                url: "http://x.net/a.zip".into(),
                proxy: Some("http://old:1".into()),
                browser_proxy: setting,
                ..Download::default()
            };
            config_for(&d, s, &NetState::default()).proxy.and_then(|p| p.proxy_for(d.url.as_str()))
        };
        // The browser connects directly: so does the download, whatever
        // UDM's own setting and the older field say.
        let direct = BrowserProxy::default();
        assert_eq!(through(Some(direct.clone()), &s), None);
        let fixed = BrowserProxy {
            mode: BrowserProxyMode::Fixed,
            http: Some("http://bp:3128".into()),
            bypass: vec!["x.net".into()],
            ..BrowserProxy::default()
        };
        assert_eq!(through(Some(fixed.clone()), &s), None, "the browser's bypass list");
        let fixed = BrowserProxy { bypass: vec![], ..fixed };
        assert_eq!(through(Some(fixed), &s).as_deref(), Some("http://bp:3128"));
        // Older extensions send only the fixed proxy.
        assert_eq!(through(None, &s).as_deref(), Some("http://old:1"));
        // Without the option, UDM's own setting applies.
        let own = Settings { use_browser_proxy: false, ..s.clone() };
        assert_eq!(through(Some(direct), &own).as_deref(), Some("http://udm:8080"));
    }

    #[test]
    fn a_chosen_adapter_binds_every_connection_and_drops_configuration_scripts() {
        use udm_domain::network::{AdapterChoice, AdapterId, DnsChoice};
        let d = Download { url: "https://x.net/a.zip".into(), ..Download::default() };
        let state = NetState::default();
        let plain = Settings {
            proxy_mode: ProxyMode::Script,
            proxy_pac_url: "http://wpad/p.pac".into(),
            ..Settings::default()
        };
        let net = config_for(&d, &plain, &state);
        assert!(net.route.binding.is_none() && net.route.dns.is_none(), "the system's, as before");
        assert!(net.proxy.and_then(|p| p.pac).is_some());

        let bound = Settings {
            network_adapter: Some(AdapterChoice { id: AdapterId::parse("wg0").unwrap(), name: "VPN".into() }),
            dns: DnsChoice::Quad9,
            ..plain
        };
        let net = config_for(&d, &bound, &state);
        assert_eq!(net.route.binding.as_ref().map(|b| b.choice.id.as_str()), Some("wg0"));
        assert!(net.proxy.and_then(|p| p.pac).is_none(), "the system would fetch the script unbound");
        // Every download shares one resolver, and its answers.
        let again = config_for(&d, &bound, &state);
        assert!(Arc::ptr_eq(net.route.dns.as_ref().unwrap(), again.route.dns.as_ref().unwrap()));
        let other = config_for(&d, &Settings { dns: DnsChoice::Google, ..bound }, &state);
        assert!(!Arc::ptr_eq(net.route.dns.as_ref().unwrap(), other.route.dns.as_ref().unwrap()));
    }

    #[test]
    fn manual_socks_can_carry_https_and_ftp() {
        let s = Settings {
            proxy_mode: ProxyMode::Manual,
            proxy_http: "web:8080".into(),
            proxy_socks5: "sock:1080".into(),
            proxy_socks_https: true,
            ..Settings::default()
        };
        let net = |url: &str, s: &Settings| {
            config_for(&Download { url: url.into(), ..Download::default() }, s, &NetState::default())
        };
        let via = |url: &str, s: &Settings| net(url, s).proxy.and_then(|p| p.proxy_for(url));
        assert_eq!(via("http://x.net/a.zip", &s).as_deref(), Some("http://web:8080"));
        assert_eq!(via("https://x.net/a.zip", &s).as_deref(), Some("socks5://sock:1080"));
        let off = Settings { proxy_socks_https: false, ..s.clone() };
        assert_eq!(
            via("https://x.net/a.zip", &off).as_deref(),
            Some("http://web:8080"),
            "an empty HTTPS proxy is the HTTP one"
        );

        // ftp:// takes the SOCKS proxy (on by default) unless an FTP proxy
        // is set, or it is turned off.
        let ftp = |s: &Settings| net("ftp://x.net/a.zip", s).proxy.and_then(|p| p.socks_for_ftp("ftp://x.net/a.zip"));
        assert_eq!(ftp(&s).map(|p| p.addr).as_deref(), Some("sock:1080"));
        assert_eq!(ftp(&Settings { proxy_socks_ftp: false, ..s.clone() }), None);
        let with_ftp_proxy = Settings { proxy_ftp: "ftpproxy:21".into(), ..s.clone() };
        assert_eq!(net("ftp://x.net/a.zip", &with_ftp_proxy).ftp_proxy.as_deref(), Some("ftpproxy:21"));
    }
}
