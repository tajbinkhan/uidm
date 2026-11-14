//! Debrid accounts: services (Real-Debrid, AllDebrid, Premiumize) that turn
//! a link to a file host (Rapidgator, 1fichier and the like) into a fast
//! direct link for their paying users. The settings name the service; the
//! account's API key is stored sealed beside the settings, never in them
//! (`DebridKey` cannot be serialized). Which links go to the service is
//! decided here (`wants_unrestrict`); asking it is the `Debrid` port's job.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::address::Address;
use crate::bytes::ByteCount;
use crate::site::host_of;

/// A debrid service UDM can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebridService {
    RealDebrid,
    AllDebrid,
    Premiumize,
}

impl DebridService {
    pub const ALL: [DebridService; 3] =
        [DebridService::RealDebrid, DebridService::AllDebrid, DebridService::Premiumize];

    /// The service's own name, which is not translated.
    pub fn name(self) -> &'static str {
        match self {
            DebridService::RealDebrid => "Real-Debrid",
            DebridService::AllDebrid => "AllDebrid",
            DebridService::Premiumize => "Premiumize",
        }
    }

    /// The page where a user finds the account's API key.
    pub fn key_page(self) -> &'static str {
        match self {
            DebridService::RealDebrid => "https://real-debrid.com/apitoken",
            DebridService::AllDebrid => "https://alldebrid.com/apikeys/",
            DebridService::Premiumize => "https://www.premiumize.me/account",
        }
    }

    /// The sites (and their subdomains) the service's own direct links and
    /// pages are on. A link there is already unlocked, or is the service's
    /// own page, and never goes back to it.
    pub fn own_sites(self) -> &'static [&'static str] {
        match self {
            DebridService::RealDebrid => &["real-debrid.com", "rdeb.io", "rdb.so"],
            DebridService::AllDebrid => &["alldebrid.com", "alldebrid.fr", "debrid.it"],
            DebridService::Premiumize => &["premiumize.me", "energycdn.com"],
        }
    }
}

impl fmt::Display for DebridService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The debrid account the settings name. Experimental: off unless the user
/// turns it on. The key itself is not here; `key_service` names the service
/// the stored key is for, and only the daemon sets it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DebridSettings {
    /// Send links to hosts the account supports through it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<DebridService>,
    /// The service an API key is stored for. A key is stored for one service
    /// only, so it never goes to another.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_service: Option<DebridService>,
}

impl DebridSettings {
    /// The service links go through: on, with a service and a key stored
    /// for that service.
    pub fn active(&self) -> Option<DebridService> {
        self.service.filter(|s| self.enabled && self.key_service == Some(*s))
    }

    /// Whether a key is stored for the chosen service.
    pub fn key_stored(&self) -> bool {
        self.service.is_some() && self.key_service == self.service
    }
}

/// An account's API key. It is never written down in the clear: it has no
/// `Serialize`, and its `Debug` hides it, so a log line cannot show it.
#[derive(Clone, PartialEq, Eq)]
pub struct DebridKey(String);

impl DebridKey {
    /// `text` as a key, trimmed; `None` when nothing is left.
    pub fn new(text: &str) -> Option<DebridKey> {
        let text = text.trim();
        (!text.is_empty()).then(|| DebridKey(text.to_string()))
    }

    /// The key itself, for the service's API and the sealed file.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DebridKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DebridKey(..)")
    }
}

/// The file hosts a service supports, by their sites: a link to one of them
/// or to any of its subdomains is supported. Case does not matter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SupportedHosts(BTreeSet<String>);

impl SupportedHosts {
    /// The sites in `list`, lowercase, without a leading `www.` or `*.`;
    /// empty entries are left out.
    pub fn new<I: IntoIterator<Item = S>, S: AsRef<str>>(list: I) -> Self {
        SupportedHosts(
            list.into_iter()
                .map(|s| {
                    let s = s.as_ref().trim().trim_end_matches('.').to_ascii_lowercase();
                    let s = s.strip_prefix("*.").unwrap_or(&s);
                    s.strip_prefix("www.").unwrap_or(s).to_string()
                })
                .filter(|s| !s.is_empty() && !s.contains(['/', ' ']))
                .collect(),
        )
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `host` is one of the sites or a subdomain of one.
    pub fn covers(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let mut rest = host.as_str();
        loop {
            if self.0.contains(rest) {
                return true;
            }
            match rest.split_once('.') {
                Some((_, parent)) if parent.contains('.') => rest = parent,
                _ => return false,
            }
        }
    }
}

/// Whether `host` is `site` or one of its subdomains.
fn on_site(host: &str, site: &str) -> bool {
    host == site || host.strip_suffix(site).is_some_and(|rest| rest.ends_with('.'))
}

/// Whether `link` should go through `settings`' account before it is
/// downloaded: the account is on, has a service and a stored key, the link
/// is an http or https address on a host the service supports, and it is
/// not on the service's own sites (a link it already unlocked).
pub fn wants_unrestrict(settings: &DebridSettings, hosts: &SupportedHosts, link: &Address) -> bool {
    let Some(service) = settings.active() else {
        return false;
    };
    if !link.is_web() {
        return false;
    }
    let host = host_of(link.as_str());
    if host.is_empty() || service.own_sites().iter().any(|site| on_site(&host, site)) {
        return false;
    }
    hosts.covers(&host)
}

/// What a service made of a link: the direct address, and the file's name
/// and size when it says them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unrestricted {
    pub address: Address,
    pub filename: Option<String>,
    pub size: Option<ByteCount>,
}

/// What a service says of the account: its user name, whether it is a
/// paying one, and until when (seconds since 1970) when it says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebridAccount {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub premium: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub premium_until: Option<u64>,
}

/// Why a service would not unlock a link or show the account. Variant names
/// are wire format: the daemon sends them to the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum DebridError {
    /// No key is stored, or the service refused it.
    #[error("the API key is missing or was refused")]
    BadKey,
    /// The account is not a paying one.
    #[error("the account is not premium")]
    NotPremium,
    /// The service does not support this link's host.
    #[error("the service does not support this host")]
    HostNotSupported,
    /// The file is gone from the host, or the link is wrong.
    #[error("the file is not available on the host")]
    LinkDead,
    /// A limit of the account or the host was reached: traffic, downloads at
    /// once, or requests.
    #[error("a limit of the account or the host was reached")]
    Limit,
    /// The service will not serve this computer's IP address, as happens
    /// behind a VPN or a proxy.
    #[error("the service does not allow this IP address; a VPN or proxy may be the reason")]
    IpNotAllowed,
    /// The file host is in maintenance or out of order for now.
    #[error("the file host is unavailable for now")]
    HostUnavailable,
    /// The stored key is for another service; a key for this one has to be
    /// typed.
    #[error("the stored API key is for {0}")]
    KeyForOther(DebridService),
    /// The service's own words.
    #[error("{0}")]
    Service(String),
    /// The service could not be reached, and why.
    #[error("could not reach the service: {0}")]
    Network(String),
}

/// What a download that could not go through `service` says, in English as
/// the engine's other failures are.
pub fn failure_text(service: DebridService, error: &DebridError) -> String {
    format!("{service} could not unlock this link: {error}.")
}

/// Why a download could not go through the debrid account: the service and
/// what it said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{}", failure_text(*service, error))]
pub struct Refused {
    pub service: DebridService,
    pub error: DebridError,
}

/// The answer to checking an account: the account, or why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebridCheck {
    pub service: DebridService,
    pub outcome: Result<DebridAccount, DebridError>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(service: DebridService) -> DebridSettings {
        DebridSettings { enabled: true, service: Some(service), key_service: Some(service) }
    }

    fn addr(text: &str) -> Address {
        Address::parse(text).unwrap()
    }

    #[test]
    fn hosts_match_their_subdomains_in_any_case() {
        let hosts = SupportedHosts::new(["Rapidgator.net", "www.1fichier.com", "*.uploaded.net", " ", "a/b"]);
        assert_eq!(hosts.len(), 3);
        assert!(hosts.covers("rapidgator.net"));
        assert!(hosts.covers("DL.RAPIDGATOR.NET"));
        assert!(hosts.covers("1fichier.com"));
        assert!(hosts.covers("x.y.uploaded.net."));
        assert!(!hosts.covers("evilrapidgator.net"));
        assert!(!hosts.covers("rapidgator.net.evil.com"));
        assert!(!hosts.covers("net"));
        assert!(!hosts.covers(""));
    }

    #[test]
    fn only_supported_links_of_an_active_account_go_through_it() {
        let hosts = SupportedHosts::new(["rapidgator.net", "1fichier.com"]);
        let rg = addr("https://rapidgator.net/file/abc/f.zip.html");
        assert!(wants_unrestrict(&on(DebridService::RealDebrid), &hosts, &rg));
        assert!(wants_unrestrict(&on(DebridService::AllDebrid), &hosts, &addr("http://1FICHIER.com/?x")));
        assert!(!wants_unrestrict(&on(DebridService::RealDebrid), &hosts, &addr("https://example.com/f.zip")));
        assert!(!wants_unrestrict(&on(DebridService::RealDebrid), &hosts, &addr("ftp://rapidgator.net/f")));
        let off = DebridSettings { enabled: false, ..on(DebridService::RealDebrid) };
        assert!(!wants_unrestrict(&off, &hosts, &rg));
        let no_key = DebridSettings { key_service: None, ..on(DebridService::RealDebrid) };
        assert!(!wants_unrestrict(&no_key, &hosts, &rg));
        // A key stored for another service is not this one's.
        let other = DebridSettings { key_service: Some(DebridService::Premiumize), ..on(DebridService::RealDebrid) };
        assert!(!wants_unrestrict(&other, &hosts, &rg));
        assert!(!other.key_stored() && on(DebridService::RealDebrid).key_stored());
        let no_service = DebridSettings { service: None, ..on(DebridService::RealDebrid) };
        assert!(!wants_unrestrict(&no_service, &hosts, &rg));
    }

    #[test]
    fn the_services_own_links_stay_as_they_are() {
        // A list that names the service's own sites still leaves them alone.
        let hosts = SupportedHosts::new(["real-debrid.com", "debrid.it", "energycdn.com"]);
        let direct = addr("https://a12.download.real-debrid.com/d/XYZ/f.zip");
        assert!(!wants_unrestrict(&on(DebridService::RealDebrid), &hosts, &direct));
        assert!(!wants_unrestrict(&on(DebridService::AllDebrid), &hosts, &addr("https://x.debrid.it/dl/f")));
        assert!(!wants_unrestrict(&on(DebridService::Premiumize), &hosts, &addr("https://e.energycdn.com/f")));
        assert!(wants_unrestrict(&on(DebridService::AllDebrid), &hosts, &direct));
    }

    #[test]
    fn a_key_hides_itself_and_settings_never_hold_it() {
        let key = DebridKey::new("  ABC123 ").unwrap();
        assert_eq!(key.expose(), "ABC123");
        assert_eq!(format!("{key:?}"), "DebridKey(..)");
        assert!(DebridKey::new("   ").is_none());
        let json = serde_json::to_value(on(DebridService::AllDebrid)).unwrap();
        assert_eq!(json, serde_json::json!({"enabled": true, "service": "all_debrid", "key_service": "all_debrid"}));
        assert_eq!(serde_json::to_value(DebridSettings::default()).unwrap(), serde_json::json!({}));
        let read: DebridSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(read, DebridSettings::default());
    }

    #[test]
    fn errors_travel_by_kind_and_read_as_a_failure() {
        let json = serde_json::to_value(DebridError::Service("Hoster in maintenance".into())).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "service", "message": "Hoster in maintenance"}));
        assert_eq!(serde_json::to_value(DebridError::BadKey).unwrap(), serde_json::json!({"kind": "bad_key"}));
        assert_eq!(
            failure_text(DebridService::RealDebrid, &DebridError::NotPremium),
            "Real-Debrid could not unlock this link: the account is not premium."
        );
    }
}
