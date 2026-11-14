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

/// A network adapter as the system names it: the interface's name on Linux
/// and macOS (`wg0`, `utun3`), the adapter's GUID on Windows. A setting
/// naming an adapter that no longer exists still loads, and binds to
/// nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AdapterId(String);

/// The longest interface name Linux and macOS take (`IFNAMSIZ`, 16, less
/// its final NUL).
const INTERFACE_NAME: usize = 15;
/// The longest id a Windows adapter has: a GUID in braces is 38.
const WINDOWS_ID: usize = 64;

/// Interface names the system hands out by number, in turn, so that one
/// connection's name is another's later: macOS' `utun3`, Linux's `tun0`.
const NUMBERED: [&str; 5] = ["utun", "tun", "tap", "ppp", "ipsec"];

impl AdapterId {
    /// An adapter's id as the system gives it or the window (or a settings
    /// file) sends it back: not empty once trimmed, and no control
    /// characters. An interface name (not a Windows adapter's `{GUID}`) has
    /// at most 15 bytes, no spaces and no `/`, as the system's own do, so
    /// it can always be handed to the system.
    pub fn parse(id: &str) -> Result<AdapterId, NetworkError> {
        let id = id.trim();
        if id.is_empty() {
            return Err(NetworkError::NoAdapter);
        }
        if id.chars().any(char::is_control) {
            return Err(NetworkError::BadAdapter);
        }
        let windows = cfg!(windows) || (id.starts_with('{') && id.ends_with('}'));
        let fits = if windows {
            id.len() <= WINDOWS_ID
        } else {
            id.len() <= INTERFACE_NAME && !id.contains('/') && !id.chars().any(char::is_whitespace)
        };
        if !fits {
            return Err(NetworkError::BadAdapter);
        }
        Ok(AdapterId(id.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the system hands this name out by number (`utun3`, `tun0`),
    /// so it may name another connection later.
    pub fn is_numbered(&self) -> bool {
        NUMBERED
            .iter()
            .any(|p| self.0.strip_prefix(p).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
    }
}

impl TryFrom<String> for AdapterId {
    type Error = NetworkError;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        AdapterId::parse(&id)
    }
}

impl From<AdapterId> for String {
    fn from(id: AdapterId) -> String {
        id.0
    }
}

/// A network an adapter is on: an address with the bits that name the
/// network (`10.2.0.0/24`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Subnet {
    base: IpAddr,
    prefix: u8,
}

impl Subnet {
    /// The network of `ip` with a prefix of `prefix` bits; `None` for a
    /// prefix longer than the address.
    pub fn new(ip: IpAddr, prefix: u8) -> Option<Subnet> {
        let base = match ip {
            IpAddr::V4(v4) if prefix <= 32 => {
                IpAddr::V4(Ipv4Addr::from(u32::from(v4) & u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)))
            }
            IpAddr::V6(v6) if prefix <= 128 => {
                IpAddr::V6(Ipv6Addr::from(u128::from(v6) & u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0)))
            }
            _ => return None,
        };
        Some(Subnet { base, prefix })
    }

    /// Whether `ip` is on this network.
    pub fn contains(&self, ip: IpAddr) -> bool {
        Subnet::new(ip, self.prefix).is_some_and(|s| s.base == self.base)
    }
}

impl fmt::Display for Subnet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.base, self.prefix)
    }
}

impl TryFrom<String> for Subnet {
    type Error = NetworkError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let (ip, prefix) = text.split_once('/').ok_or(NetworkError::BadAdapter)?;
        let ip = ip.parse().map_err(|_| NetworkError::BadAdapter)?;
        let prefix = prefix.parse().map_err(|_| NetworkError::BadAdapter)?;
        Subnet::new(ip, prefix).ok_or(NetworkError::BadAdapter)
    }
}

impl From<Subnet> for String {
    fn from(s: Subnet) -> String {
        s.to_string()
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
    /// The networks it was on when it was chosen, kept only for a name the
    /// system hands out by number (`utun3`, `tun0`): an adapter of that
    /// name on none of them is another connection, and counts as down.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subnets: Vec<Subnet>,
}

impl AdapterChoice {
    /// `adapter`, chosen now: with its networks when its name is numbered.
    pub fn of(adapter: &NetworkAdapter) -> AdapterChoice {
        AdapterChoice { id: adapter.id.clone(), name: adapter.name.clone(), subnets: adapter.networks_to_keep() }
    }

    /// The name to show for it.
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            self.id.as_str()
        } else {
            &self.name
        }
    }

    /// Whether `adapter` is the connection that was chosen: it has the id
    /// and, when networks were kept, an address on one of them.
    pub fn is(&self, adapter: &NetworkAdapter) -> bool {
        adapter.id == self.id
            && (self.subnets.is_empty()
                || adapter.addresses.iter().any(|a| self.subnets.iter().any(|s| s.contains(*a))))
    }

    /// The networks to keep with this choice, as the settings are saved:
    /// those it had before when it is the same adapter (so another
    /// connection that took its name is still not taken for it), else those
    /// the adapter is on now when its name is numbered, if it is there.
    pub fn remember(&mut self, before: Option<&AdapterChoice>, adapters: &[NetworkAdapter]) {
        if let Some(before) = before.filter(|b| b.id == self.id && !b.subnets.is_empty()) {
            self.subnets = before.subnets.clone();
            return;
        }
        self.subnets =
            adapters.iter().find(|a| a.id == self.id).map(NetworkAdapter::networks_to_keep).unwrap_or_default();
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
    /// The networks those addresses are on, where the system says.
    pub subnets: Vec<Subnet>,
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

    /// The networks a choice of it keeps: when its name is numbered, those
    /// that are not link-local.
    fn networks_to_keep(&self) -> Vec<Subnet> {
        if !self.id.is_numbered() {
            return Vec::new();
        }
        self.subnets.iter().filter(|s| !link_local(s.base)).copied().collect()
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
/// `adapters`: there, the same connection ([`AdapterChoice::is`]), running,
/// and with an address. Anything else is [`AdapterDown`]; there is no other
/// network to fall back to.
pub fn bound_adapter<'a>(
    choice: &AdapterChoice,
    adapters: &'a [NetworkAdapter],
) -> Result<&'a NetworkAdapter, AdapterDown> {
    adapters.iter().find(|a| choice.is(a) && a.usable()).ok_or_else(|| AdapterDown::new(choice))
}

/// Why no connection could be bound to the adapter: the system refused it
/// (Linux before 5.7 lets only administrators bind to an interface). The
/// download stops rather than go out unbound.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{BIND_REFUSED}{adapter}{BIND_REFUSED_END}")]
pub struct BindRefused {
    /// The adapter's name, as the user chose it.
    pub adapter: String,
}

const BIND_REFUSED: &str = "The system would not bind connections to the network adapter ";
const BIND_REFUSED_END: &str = " (on Linux, this needs version 5.7 or later)";

impl BindRefused {
    pub fn new(choice: &AdapterChoice) -> BindRefused {
        BindRefused { adapter: choice.label().to_string() }
    }

    /// The reason read back from a download's error text, as
    /// [`AdapterDown::read`].
    pub fn read(error: &str) -> Option<BindRefused> {
        let adapter = error.strip_prefix(BIND_REFUSED)?.strip_suffix(BIND_REFUSED_END)?;
        Some(BindRefused { adapter: adapter.to_string() })
    }
}

/// Why a network setting or the adapters' list is not usable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetworkError {
    #[error("Choose a network adapter.")]
    NoAdapter,
    /// An adapter's id no system gives: control characters, or an
    /// interface name that is too long or has spaces.
    #[error("That is not the name of a network adapter.")]
    BadAdapter,
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

/// How long an answer is kept, from the time to live the resolver gave its
/// addresses: at least half a minute, so a resolver that says 0 is not
/// asked for every connection, and at most an hour.
pub fn dns_cache_seconds(ttl: u32) -> u64 {
    u64::from(ttl).clamp(30, 3600)
}

/// The time to live of a name's addresses from the two families' answers,
/// each `(addresses, ttl)`: the shorter of those that gave addresses. An
/// empty answer's time to live (often 0, or the zone's negative one) says
/// nothing about the addresses the other family gave.
pub fn answer_ttl(v4: (usize, u32), v6: (usize, u32)) -> u32 {
    match (v4.0 > 0, v6.0 > 0) {
        (true, true) => v4.1.min(v6.1),
        (true, false) => v4.1,
        (false, true) => v6.1,
        (false, false) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(id: &str, addresses: &[&str], up: bool) -> NetworkAdapter {
        let addresses: Vec<IpAddr> = addresses.iter().map(|a| a.parse().unwrap()).collect();
        let subnets = addresses.iter().map(|a| Subnet::new(*a, if a.is_ipv4() { 24 } else { 64 }).unwrap()).collect();
        NetworkAdapter { id: AdapterId::parse(id).unwrap(), name: format!("{id} name"), addresses, subnets, up }
    }

    fn choice(id: &str) -> AdapterChoice {
        AdapterChoice { id: AdapterId::parse(id).unwrap(), name: "My VPN".into(), subnets: Vec::new() }
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
        assert_eq!(AdapterDown::read(&down.to_string()), Some(down.clone()));
        assert_eq!(AdapterDown::read("server returned HTTP 404"), None);
        let unnamed = AdapterChoice { id: AdapterId::parse("wg0").unwrap(), name: String::new(), subnets: Vec::new() };
        assert_eq!(AdapterDown::new(&unnamed).adapter, "wg0");
        let refused = BindRefused::new(&choice("wg0"));
        assert_eq!(BindRefused::read(&refused.to_string()), Some(refused));
        assert_eq!(BindRefused::read(&down.to_string()), None);
    }

    #[test]
    fn adapter_ids_are_names_the_system_could_give() {
        for good in ["wg0", "utun3", "enp0s31f6", "tun-vpn.1", "{4D36E972-E325-11CE-BFC1-08002BE10318}"] {
            assert_eq!(AdapterId::parse(good).unwrap().as_str(), good);
        }
        assert_eq!(AdapterId::parse(" wg0 ").unwrap().as_str(), "wg0");
        assert_eq!(AdapterId::parse(""), Err(NetworkError::NoAdapter));
        for bad in ["wg\0", "eth\n0", "{x\u{7f}}"] {
            assert_eq!(AdapterId::parse(bad), Err(NetworkError::BadAdapter), "{bad:?}");
        }
        // Interface names: what Linux and macOS would take.
        if !cfg!(windows) {
            for bad in ["a-name-of-16-byt", "my vpn", "a/b"] {
                assert_eq!(AdapterId::parse(bad), Err(NetworkError::BadAdapter), "{bad:?}");
            }
        }
        assert_eq!(AdapterId::parse(&format!("{{{}}}", "x".repeat(70))), Err(NetworkError::BadAdapter));
        // A settings file is held to the same rules.
        assert!(serde_json::from_str::<AdapterChoice>(r#"{"id":"wg\u00000","name":"x"}"#).is_err());
        assert!(serde_json::from_str::<AdapterChoice>(r#"{"id":"wg0"}"#).is_ok());
    }

    #[test]
    fn a_numbered_name_on_other_networks_is_another_connection() {
        let id = |s: &str| AdapterId::parse(s).unwrap();
        assert!(id("utun3").is_numbered() && id("tun0").is_numbered() && id("ppp0").is_numbered());
        assert!(!id("wg0").is_numbered() && !id("utun").is_numbered() && !id("tunnel0").is_numbered());

        let vpn = adapter("utun3", &["10.8.0.6", "fe80::1"], true);
        let chosen = AdapterChoice::of(&vpn);
        assert_eq!(chosen.subnets, vec![Subnet::new("10.8.0.0".parse().unwrap(), 24).unwrap()], "link-local left out");
        assert!(bound_adapter(&chosen, std::slice::from_ref(&vpn)).is_ok());
        // Reconnected with another address on the same network: the same.
        let again = adapter("utun3", &["10.8.0.9"], true);
        assert!(bound_adapter(&chosen, &[again]).is_ok());
        // Another tunnel that took the name: down, never used.
        let other = adapter("utun3", &["172.20.1.2"], true);
        assert_eq!(bound_adapter(&chosen, &[other]), Err(AdapterDown { adapter: "utun3 name".into() }));
        // A name that is the user's own keeps no networks.
        assert!(AdapterChoice::of(&adapter("wg0", &["10.2.0.2"], true)).subnets.is_empty());

        // Saving keeps the networks of the same choice, and takes them
        // afresh for a new one.
        let mut sent = AdapterChoice { subnets: Vec::new(), ..chosen.clone() };
        sent.remember(Some(&chosen), &[adapter("utun3", &["172.20.1.2"], true)]);
        assert_eq!(sent.subnets, chosen.subnets);
        let mut new = AdapterChoice { id: id("tun0"), name: "tun0".into(), subnets: Vec::new() };
        new.remember(Some(&chosen), &[adapter("tun0", &["10.9.0.2"], true)]);
        assert_eq!(new.subnets, vec![Subnet::new("10.9.0.0".parse().unwrap(), 24).unwrap()]);
        let json = serde_json::to_string(&new).unwrap();
        assert_eq!(json, r#"{"id":"tun0","name":"tun0","subnets":["10.9.0.0/24"]}"#);
        assert_eq!(serde_json::from_str::<AdapterChoice>(&json).unwrap(), new);
    }

    #[test]
    fn subnets_hold_the_addresses_under_their_prefix() {
        let net = Subnet::new("10.2.3.4".parse().unwrap(), 16).unwrap();
        assert_eq!(net.to_string(), "10.2.0.0/16");
        assert!(net.contains("10.2.200.1".parse().unwrap()));
        assert!(!net.contains("10.3.0.1".parse().unwrap()));
        assert!(!net.contains("fd00::1".parse().unwrap()));
        let six = Subnet::new("2001:db8:1:2::5".parse().unwrap(), 64).unwrap();
        assert!(six.contains("2001:db8:1:2:ffff::1".parse().unwrap()));
        assert_eq!(Subnet::new("1.2.3.4".parse().unwrap(), 0).unwrap().to_string(), "0.0.0.0/0");
        assert!(Subnet::new("1.2.3.4".parse().unwrap(), 33).is_none());
    }

    #[test]
    fn the_bound_address_prefers_ipv4_and_routable_addresses() {
        let a = adapter("x", &["fe80::1", "169.254.3.4", "2001:db8::5", "10.0.0.7"], true);
        assert_eq!(a.bind_address(), Some("10.0.0.7".parse().unwrap()));
        assert_eq!(a.address_for("2001:db8::9".parse().unwrap()), Some("2001:db8::5".parse().unwrap()));
        assert_eq!(adapter("y", &["fe80::1"], true).bind_address(), Some("fe80::1".parse().unwrap()));
        assert_eq!(adapter("z", &["fd00::1"], true).address_for("1.2.3.4".parse().unwrap()), None);
        assert_eq!(adapter("z", &[], true).bind_address(), None);
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
        // An empty answer's time to live does not shorten the other's.
        assert_eq!(answer_ttl((2, 300), (0, 0)), 300);
        assert_eq!(answer_ttl((0, 5), (1, 600)), 600);
        assert_eq!(answer_ttl((1, 300), (1, 60)), 60);
    }
}
