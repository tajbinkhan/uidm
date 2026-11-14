//! Downloading through one network adapter, and where names are looked up.
//!
//! With an adapter chosen (`Settings::network_adapter`), every connection
//! a download makes is bound to it, and when it is down nothing connects:
//! the download stops with [`AdapterDown`] instead of going out through
//! another network (a kill switch, for downloading only through a VPN).
//! [`bound_adapter`] is that decision. The adapters come from the
//! `NetworkAdapters` port.
//!
//! Names are looked up by the system, or with DNS over HTTPS at a resolver
//! the user picks ([`DnsChoice`]). The presets name their resolvers'
//! addresses, so finding the resolver needs no lookup of its own.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

/// A network adapter as the system names it for good: the interface's name
/// on Linux and macOS (`wg0`, `utun3`), the adapter's GUID on Windows. Read
/// back as written, so a setting naming an adapter that no longer exists
/// still loads, and binds to nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AdapterId(String);

impl AdapterId {
    /// An adapter's id as the system gives it or the window sends it back:
    /// not empty once trimmed.
    pub fn parse(id: &str) -> Result<AdapterId, NetworkError> {
        let id = id.trim();
        if id.is_empty() {
            return Err(NetworkError::NoAdapter);
        }
        Ok(AdapterId(id.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AdapterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The adapter downloads go through, as the settings keep it: its id, and
/// the name it showed when it was chosen, for saying which one is down
/// once it is gone from the system's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterChoice {
    pub id: AdapterId,
    #[serde(default)]
    pub name: String,
}

impl AdapterChoice {
    /// The name to show for it.
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            self.id.as_str()
        } else {
            &self.name
        }
    }
}

/// One of the computer's network adapters, as the `NetworkAdapters` port
/// lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkAdapter {
    pub id: AdapterId,
    /// The name the system shows for it (Windows' friendly name; the id
    /// elsewhere).
    pub name: String,
    pub addresses: Vec<IpAddr>,
    /// The system says it is running.
    pub up: bool,
}

impl NetworkAdapter {
    /// The address connections bound to it come from where they need one
    /// (Windows binds by address): its first IPv4 address, else its first
    /// IPv6 one, preferring ones that are not link-local, which reach only
    /// the local network.
    pub fn bind_address(&self) -> Option<IpAddr> {
        let routable = |a: &&IpAddr| !link_local(**a);
        let v4 = |a: &&IpAddr| a.is_ipv4();
        let v6 = |a: &&IpAddr| a.is_ipv6();
        let pick = |family: &dyn Fn(&&IpAddr) -> bool| {
            self.addresses.iter().filter(family).find(routable).or_else(|| self.addresses.iter().find(family)).copied()
        };
        pick(&v4).or_else(|| pick(&v6))
    }

    /// Its address of `ip`'s family, for a connection to `ip`.
    pub fn address_for(&self, ip: IpAddr) -> Option<IpAddr> {
        let same = |a: &&IpAddr| a.is_ipv4() == ip.is_ipv4();
        self.addresses
            .iter()
            .filter(same)
            .find(|a| !link_local(**a))
            .or_else(|| self.addresses.iter().find(same))
            .copied()
    }

    /// Whether connections can go through it now: it is running and has an
    /// address.
    pub fn usable(&self) -> bool {
        self.up && !self.addresses.is_empty()
    }

    /// The adapter's addresses as Options lists them.
    pub fn addresses_text(&self) -> String {
        self.addresses.iter().map(IpAddr::to_string).collect::<Vec<_>>().join(", ")
    }
}

fn link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// Why a connection was not made: the adapter downloads must go through is
/// missing, stopped, or has no address. The download stops rather than use
/// another network.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{ADAPTER_DOWN}{adapter}{IS_DOWN}")]
pub struct AdapterDown {
    /// The adapter's name, as the user chose it.
    pub adapter: String,
}

const ADAPTER_DOWN: &str = "The network adapter ";
const IS_DOWN: &str = " is down";

impl AdapterDown {
    pub fn new(choice: &AdapterChoice) -> AdapterDown {
        AdapterDown { adapter: choice.label().to_string() }
    }

    /// The reason a download failed for, read back from its error text (the
    /// engine reports failures as text), so the window can say it in the
    /// user's language.
    pub fn read(error: &str) -> Option<AdapterDown> {
        let adapter = error.strip_prefix(ADAPTER_DOWN)?.strip_suffix(IS_DOWN)?;
        Some(AdapterDown { adapter: adapter.to_string() })
    }
}

/// The adapter connections must go through under `choice`, among
/// `adapters`: there, running, and with an address. Anything else is
/// [`AdapterDown`]; there is no other network to fall back to.
pub fn bound_adapter<'a>(
    choice: &AdapterChoice,
    adapters: &'a [NetworkAdapter],
) -> Result<&'a NetworkAdapter, AdapterDown> {
    adapters.iter().find(|a| a.id == choice.id && a.usable()).ok_or_else(|| AdapterDown::new(choice))
}

/// Why a network setting or the adapters' list is not usable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetworkError {
    #[error("Choose a network adapter.")]
    NoAdapter,
    /// A custom DNS over HTTPS resolver's address that is not an https://
    /// address with a host.
    #[error("The DNS over HTTPS address must start with https:// and name a server.")]
    DohAddress,
    /// The system would not list its adapters; its own words.
    #[error("The network adapters could not be read: {0}")]
    Adapters(String),
}

/// A DNS over HTTPS resolver's address (RFC 8484): https only, with a
/// host, and no user name or password in it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct DohUrl(String);

impl DohUrl {
    pub fn parse(text: &str) -> Result<DohUrl, NetworkError> {
        let text = text.trim();
        let url = url::Url::parse(text).map_err(|_| NetworkError::DohAddress)?;
        let named = url.host_str().is_some_and(|h| !h.is_empty());
        if url.scheme() != "https" || !named || !url.username().is_empty() || url.password().is_some() {
            return Err(NetworkError::DohAddress);
        }
        Ok(DohUrl(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The resolver's host name (or address), which the system looks up once.
    pub fn host(&self) -> String {
        url::Url::parse(&self.0).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default()
    }
}

/// Where names are looked up.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "DnsRead", into = "DnsWritten")]
pub enum DnsChoice {
    /// The system's resolver, as every program uses.
    #[default]
    System,
    Cloudflare,
    Google,
    Quad9,
    /// A DNS over HTTPS resolver the user names.
    Custom(DohUrl),
}

/// How `settings.json` keeps the choice: `"system"`, `"cloudflare"`,
/// `"google"`, `"quad9"` or `{"custom": "https://..."}`. Anything else (a
/// newer UDM's, or an address that is not one) reads as the system's, so
/// the file still loads.
#[derive(Deserialize)]
#[serde(untagged)]
enum DnsRead {
    Preset(String),
    Custom { custom: String },
    Other(serde::de::IgnoredAny),
}

#[derive(Serialize)]
#[serde(untagged)]
enum DnsWritten {
    Preset(&'static str),
    Custom { custom: String },
}

impl From<DnsRead> for DnsChoice {
    fn from(read: DnsRead) -> DnsChoice {
        match read {
            DnsRead::Preset(word) => match word.as_str() {
                "cloudflare" => DnsChoice::Cloudflare,
                "google" => DnsChoice::Google,
                "quad9" => DnsChoice::Quad9,
                _ => DnsChoice::System,
            },
            DnsRead::Custom { custom } => DohUrl::parse(&custom).map_or(DnsChoice::System, DnsChoice::Custom),
            DnsRead::Other(_) => DnsChoice::System,
        }
    }
}

impl From<DnsChoice> for DnsWritten {
    fn from(dns: DnsChoice) -> DnsWritten {
        match dns {
            DnsChoice::System => DnsWritten::Preset("system"),
            DnsChoice::Cloudflare => DnsWritten::Preset("cloudflare"),
            DnsChoice::Google => DnsWritten::Preset("google"),
            DnsChoice::Quad9 => DnsWritten::Preset("quad9"),
            DnsChoice::Custom(url) => DnsWritten::Custom { custom: url.0 },
        }
    }
}

/// A DNS over HTTPS resolver to ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DohEndpoint {
    /// The address queries are posted to.
    pub url: String,
    /// Its host, which the TLS certificate must name.
    pub host: String,
    /// The host's addresses, known beforehand for the presets so that no
    /// lookup is made to find the resolver. Empty for a custom resolver,
    /// whose host the system looks up.
    pub bootstrap: Vec<IpAddr>,
}

impl DnsChoice {
    /// The resolvers Options offers, after the system's.
    pub const PRESETS: [DnsChoice; 3] = [DnsChoice::Cloudflare, DnsChoice::Google, DnsChoice::Quad9];

    /// The resolver's own name, for Options.
    pub fn name(&self) -> &'static str {
        match self {
            DnsChoice::System => "System",
            DnsChoice::Cloudflare => "Cloudflare (1.1.1.1)",
            DnsChoice::Google => "Google (8.8.8.8)",
            DnsChoice::Quad9 => "Quad9 (9.9.9.9)",
            DnsChoice::Custom(_) => "Custom",
        }
    }

    /// The resolver to ask, or `None` for the system's.
    pub fn endpoint(&self) -> Option<DohEndpoint> {
        let preset = |host: &str, v4: [Ipv4Addr; 2], v6: [Ipv6Addr; 2]| DohEndpoint {
            url: format!("https://{host}/dns-query"),
            host: host.to_string(),
            bootstrap: v4.into_iter().map(IpAddr::V4).chain(v6.into_iter().map(IpAddr::V6)).collect(),
        };
        match self {
            DnsChoice::System => None,
            DnsChoice::Cloudflare => Some(preset(
                "cloudflare-dns.com",
                [Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(1, 0, 0, 1)],
                [
                    Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111),
                    Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1001),
                ],
            )),
            DnsChoice::Google => Some(preset(
                "dns.google",
                [Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(8, 8, 4, 4)],
                [
                    Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888),
                    Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8844),
                ],
            )),
            DnsChoice::Quad9 => Some(preset(
                "dns.quad9.net",
                [Ipv4Addr::new(9, 9, 9, 9), Ipv4Addr::new(149, 112, 112, 112)],
                [Ipv6Addr::new(0x2620, 0xfe, 0, 0, 0, 0, 0, 0xfe), Ipv6Addr::new(0x2620, 0xfe, 0, 0, 0, 0, 0, 0x9)],
            )),
            DnsChoice::Custom(url) => Some(DohEndpoint { url: url.0.clone(), host: url.host(), bootstrap: Vec::new() }),
        }
    }
}

/// How long an answer is kept, from the time to live the resolver gave it:
/// at least half a minute, so a resolver that says 0 is not asked for every
/// connection, and at most an hour.
pub fn dns_cache_seconds(ttl: u32) -> u64 {
    u64::from(ttl).clamp(30, 3600)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(id: &str, addresses: &[&str], up: bool) -> NetworkAdapter {
        NetworkAdapter {
            id: AdapterId::parse(id).unwrap(),
            name: format!("{id} name"),
            addresses: addresses.iter().map(|a| a.parse().unwrap()).collect(),
            up,
        }
    }

    fn choice(id: &str) -> AdapterChoice {
        AdapterChoice { id: AdapterId::parse(id).unwrap(), name: "My VPN".into() }
    }

    #[test]
    fn only_a_running_adapter_with_an_address_is_bound_to() {
        let all = [adapter("eth0", &["192.168.1.5"], true), adapter("wg0", &["10.2.0.2", "fd00::2"], true)];
        assert_eq!(bound_adapter(&choice("wg0"), &all).unwrap().id.as_str(), "wg0");
        // Missing, stopped or without an address: down, never another one.
        let down = AdapterDown { adapter: "My VPN".into() };
        assert_eq!(bound_adapter(&choice("tun9"), &all), Err(down.clone()));
        assert_eq!(bound_adapter(&choice("wg0"), &[adapter("wg0", &["10.2.0.2"], false)]), Err(down.clone()));
        assert_eq!(bound_adapter(&choice("wg0"), &[adapter("wg0", &[], true)]), Err(down.clone()));
        assert_eq!(bound_adapter(&choice("wg0"), &[]), Err(down));
    }

    #[test]
    fn the_down_reason_reads_back_from_its_text() {
        let down = AdapterDown::new(&choice("wg0"));
        assert_eq!(down.to_string(), "The network adapter My VPN is down");
        assert_eq!(AdapterDown::read(&down.to_string()), Some(down));
        assert_eq!(AdapterDown::read("server returned HTTP 404"), None);
        let unnamed = AdapterChoice { id: AdapterId::parse("wg0").unwrap(), name: String::new() };
        assert_eq!(AdapterDown::new(&unnamed).adapter, "wg0");
    }

    #[test]
    fn the_bound_address_prefers_ipv4_and_routable_addresses() {
        let a = adapter("x", &["fe80::1", "169.254.3.4", "2001:db8::5", "10.0.0.7"], true);
        assert_eq!(a.bind_address(), Some("10.0.0.7".parse().unwrap()));
        assert_eq!(a.address_for("2001:db8::9".parse().unwrap()), Some("2001:db8::5".parse().unwrap()));
        assert_eq!(adapter("y", &["fe80::1"], true).bind_address(), Some("fe80::1".parse().unwrap()));
        assert_eq!(adapter("z", &["fd00::1"], true).address_for("1.2.3.4".parse().unwrap()), None);
        assert_eq!(adapter("z", &[], true).bind_address(), None);
        assert!(AdapterId::parse("  ").is_err());
    }

    #[test]
    fn doh_addresses_are_https_with_a_host() {
        assert!(DohUrl::parse("https://dns.example/dns-query").is_ok());
        assert_eq!(DohUrl::parse(" https://dns.example/q ").unwrap().host(), "dns.example");
        for bad in ["http://dns.example/dns-query", "dns.example", "https://", "https://u:p@dns.example/q", "ftp://x/"]
        {
            assert_eq!(DohUrl::parse(bad), Err(NetworkError::DohAddress), "{bad}");
        }
    }

    #[test]
    fn presets_name_their_resolvers_addresses() {
        let cf = DnsChoice::Cloudflare.endpoint().unwrap();
        assert_eq!(cf.url, "https://cloudflare-dns.com/dns-query");
        assert!(cf.bootstrap.contains(&"1.1.1.1".parse().unwrap()));
        assert_eq!(DnsChoice::Google.endpoint().unwrap().host, "dns.google");
        assert!(DnsChoice::Quad9.endpoint().unwrap().bootstrap.contains(&"149.112.112.112".parse().unwrap()));
        assert_eq!(DnsChoice::System.endpoint(), None);
        let custom = DnsChoice::Custom(DohUrl::parse("https://doh.example:8443/q").unwrap()).endpoint().unwrap();
        assert!(custom.bootstrap.is_empty() && custom.host == "doh.example");
    }

    #[test]
    fn the_dns_choice_keeps_its_old_words_and_reads_the_unknown_as_the_systems() {
        let custom = DnsChoice::Custom(DohUrl::parse("https://doh.example/q").unwrap());
        for (dns, json) in [
            (DnsChoice::System, r#""system""#),
            (DnsChoice::Quad9, r#""quad9""#),
            (custom, r#"{"custom":"https://doh.example/q"}"#),
        ] {
            assert_eq!(serde_json::to_string(&dns).unwrap(), json);
            assert_eq!(serde_json::from_str::<DnsChoice>(json).unwrap(), dns);
        }
        for odd in [r#""tor""#, r#"{"custom":"http://x/"}"#, "42"] {
            assert_eq!(serde_json::from_str::<DnsChoice>(odd).unwrap(), DnsChoice::System, "{odd}");
        }
    }

    #[test]
    fn answers_are_kept_between_half_a_minute_and_an_hour() {
        assert_eq!(dns_cache_seconds(0), 30);
        assert_eq!(dns_cache_seconds(300), 300);
        assert_eq!(dns_cache_seconds(u32::MAX), 3600);
    }
}
