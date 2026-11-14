use std::sync::{Arc, Mutex};

use udm_domain::address::Address;
use udm_domain::debrid::{wants_unrestrict, DebridError, DebridService, DebridSettings, Refused};
use udm_domain::debrid::{SupportedHosts, Unrestricted};
use udm_domain::ports::{Debrid, DebridKeys, Timestamps};
use udm_domain::site::host_of;
use udm_domain::sync::LockExt;

/// How long a service's list of hosts is used before it is read again, in
/// seconds.
pub const HOSTS_FOR: u64 = 6 * 60 * 60;
/// How long a link the service unlocked is used again for the same link, in
/// seconds: the save-as dialog's look at the file and the download that
/// follows ask once. Direct links last hours.
pub const LINK_FOR: u64 = 10 * 60;
/// How long a list of hosts that could not be read is not asked for again,
/// in seconds, so a service that is down does not hold up every download.
const FAILED_HOSTS_FOR: u64 = 5 * 60;

/// What `ResolveLink` keeps between downloads.
#[derive(Default)]
struct Memo {
    /// The service's hosts when they were read, or why they could not be.
    hosts: Option<(DebridService, u64, Result<SupportedHosts, DebridError>)>,
    /// Links lately unlocked, with when.
    links: Vec<(DebridService, Address, u64, Unrestricted)>,
}

/// Turn a link to a file host into the direct link the debrid account gives
/// for it, when the settings say links go through the account and its
/// service supports the host. Every start of a download asks, so a direct
/// link that expired is replaced by a fresh one, as New link's is.
pub struct ResolveLink {
    debrid: Arc<dyn Debrid>,
    keys: Arc<dyn DebridKeys>,
    clock: Arc<dyn Timestamps>,
    memo: Mutex<Memo>,
}

impl ResolveLink {
    pub fn new(debrid: Arc<dyn Debrid>, keys: Arc<dyn DebridKeys>, clock: Arc<dyn Timestamps>) -> Self {
        ResolveLink { debrid, keys, clock, memo: Mutex::default() }
    }

    /// The direct link to download `link` from, or `None` to download it as
    /// it is: the account is off, the host is not one the service supports
    /// (or the service says so when asked), or the service cannot be
    /// reached for its list of hosts. Any other answer from the service,
    /// such as a refused key, fails the download rather than download the
    /// file host's page in place of the file.
    pub fn run(&self, settings: &DebridSettings, link: &Address) -> Result<Option<Unrestricted>, Refused> {
        let Some(service) = settings.active() else {
            return Ok(None);
        };
        if !link.is_web() {
            return Ok(None);
        }
        let Some(key) = self.keys.key(service) else {
            tracing::warn!(service = %service, "no debrid key can be read for the service");
            return Ok(None);
        };
        let now = self.clock.unix_now();
        let hosts = match self.hosts(service, &key, now) {
            Ok(hosts) => hosts,
            Err(DebridError::Network(_)) => return Ok(None),
            Err(error) => return Err(Refused { service, error }),
        };
        if !wants_unrestrict(settings, &hosts, link) {
            return Ok(None);
        }
        if let Some(done) = self.recent(service, link, now) {
            return Ok(Some(done));
        }
        let host = host_of(link.as_str());
        match self.debrid.unrestrict(service, &key, link) {
            Ok(done) => {
                tracing::info!(service = %service, host = %host, "link unlocked through the debrid account");
                let mut memo = self.memo.lock_or_recover();
                memo.links.retain(|(_, _, at, _)| now.saturating_sub(*at) < LINK_FOR);
                memo.links.push((service, link.clone(), now, done.clone()));
                Ok(Some(done))
            }
            Err(DebridError::HostNotSupported) => {
                tracing::info!(service = %service, host = %host, "the debrid service does not take this host");
                Ok(None)
            }
            Err(error) => {
                tracing::warn!(service = %service, host = %host, error = %error, "the debrid service refused the link");
                Err(Refused { service, error })
            }
        }
    }

    /// Forget the hosts and links kept, as when the key changes.
    pub fn forget(&self) {
        *self.memo.lock_or_recover() = Memo::default();
    }

    /// The hosts `service` supports, read again once they are older than
    /// `HOSTS_FOR`; why not, a while, when they cannot be read and no older
    /// list of the service's is kept.
    fn hosts(
        &self,
        service: DebridService,
        key: &udm_domain::debrid::DebridKey,
        now: u64,
    ) -> Result<SupportedHosts, DebridError> {
        if let Some((kept, at, hosts)) = &self.memo.lock_or_recover().hosts {
            let age = now.saturating_sub(*at);
            let fresh = if hosts.is_ok() { HOSTS_FOR } else { FAILED_HOSTS_FOR };
            if *kept == service && age < fresh {
                return hosts.clone();
            }
        }
        let read = match self.debrid.hosts(service, key) {
            Ok(hosts) => Ok(hosts),
            Err(e) => {
                tracing::warn!(service = %service, error = %e, "could not read the debrid service's hosts");
                // An older list of the service's still tells which links
                // are its hosts'; those then fail when they are unlocked.
                match &self.memo.lock_or_recover().hosts {
                    Some((kept, _, Ok(old))) if *kept == service => Ok(old.clone()),
                    _ => Err(e),
                }
            }
        };
        self.memo.lock_or_recover().hosts = Some((service, now, read.clone()));
        read
    }

    fn recent(&self, service: DebridService, link: &Address, now: u64) -> Option<Unrestricted> {
        let memo = self.memo.lock_or_recover();
        memo.links
            .iter()
            .find(|(s, l, at, _)| *s == service && l == link && now.saturating_sub(*at) < LINK_FOR)
            .map(|(_, _, _, done)| done.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{DebridDesk, Keyring, Seconds};

    const RG: &str = "https://rapidgator.net/file/1/a.zip.html";
    const DIRECT: &str = "https://dl.real-debrid.com/d/1/a.zip";

    fn on() -> DebridSettings {
        DebridSettings {
            enabled: true,
            service: Some(DebridService::RealDebrid),
            key_service: Some(DebridService::RealDebrid),
        }
    }

    fn resolve(desk: DebridDesk, now: u64) -> (ResolveLink, Arc<DebridDesk>, Arc<Seconds>) {
        let (desk, clock) = (Arc::new(desk), Arc::new(Seconds::at(now)));
        let links =
            ResolveLink::new(desk.clone(), Arc::new(Keyring::holding(DebridService::RealDebrid, "k")), clock.clone());
        (links, desk, clock)
    }

    fn addr(text: &str) -> Address {
        Address::parse(text).unwrap()
    }

    #[test]
    fn a_supported_link_becomes_the_direct_one() {
        let (links, desk, _) = resolve(DebridDesk::new(&["rapidgator.net"], &[(RG, DIRECT)]), 1000);
        let done = links.run(&on(), &addr(RG)).unwrap().unwrap();
        assert_eq!(done.address.as_str(), DIRECT);
        assert_eq!(links.run(&on(), &addr("https://example.com/f.zip")), Ok(None));
        assert_eq!(desk.calls(), ["hosts Real-Debrid k", &format!("unrestrict {RG} Real-Debrid k")]);
    }

    #[test]
    fn an_account_that_is_off_asks_nothing() {
        let (links, desk, _) = resolve(DebridDesk::new(&["rapidgator.net"], &[(RG, DIRECT)]), 0);
        assert_eq!(links.run(&DebridSettings { enabled: false, ..on() }, &addr(RG)), Ok(None));
        assert_eq!(links.run(&DebridSettings { key_service: None, ..on() }, &addr(RG)), Ok(None));
        assert_eq!(links.run(&on(), &addr("ftp://rapidgator.net/a")), Ok(None));
        assert!(desk.calls().is_empty());
        let none = ResolveLink::new(desk.clone(), Arc::new(Keyring::default()), Arc::new(Seconds::at(0)));
        assert_eq!(none.run(&on(), &addr(RG)), Ok(None), "a key that cannot be read");
        // A key stored for another service is never sent to this one.
        let other = Arc::new(Keyring::holding(DebridService::Premiumize, "pm"));
        let wrong = ResolveLink::new(desk.clone(), other, Arc::new(Seconds::at(0)));
        assert_eq!(wrong.run(&on(), &addr(RG)), Ok(None));
        assert!(desk.calls().is_empty());
    }

    #[test]
    fn hosts_are_read_again_after_six_hours_and_links_after_ten_minutes() {
        let (links, desk, clock) = resolve(DebridDesk::new(&["rapidgator.net"], &[(RG, DIRECT)]), 0);
        links.run(&on(), &addr(RG)).unwrap();
        clock.set(LINK_FOR - 1);
        links.run(&on(), &addr(RG)).unwrap();
        assert_eq!(desk.calls().len(), 2, "one hosts list and one unlock: {:?}", desk.calls());
        clock.set(LINK_FOR);
        links.run(&on(), &addr(RG)).unwrap();
        assert_eq!(desk.calls().len(), 3, "the link is unlocked again");
        clock.set(HOSTS_FOR);
        links.run(&on(), &addr(RG)).unwrap();
        assert_eq!(desk.calls().iter().filter(|c| c.starts_with("hosts")).count(), 2);
        links.forget();
        links.run(&on(), &addr(RG)).unwrap();
        assert_eq!(desk.calls().iter().filter(|c| c.starts_with("hosts")).count(), 3);
    }

    #[test]
    fn hosts_that_cannot_be_read_leave_links_as_they_are() {
        let desk = DebridDesk::new(&["rapidgator.net"], &[(RG, DIRECT)]);
        *desk.hosts.lock().unwrap() = Err(DebridError::Network("timed out".into()));
        let (links, desk, clock) = resolve(desk, 0);
        assert_eq!(links.run(&on(), &addr(RG)), Ok(None));
        assert_eq!(links.run(&on(), &addr(RG)), Ok(None));
        assert_eq!(desk.calls().len(), 1, "the failure is kept a while");
        *desk.hosts.lock().unwrap() = Ok(vec!["rapidgator.net".into()]);
        clock.set(FAILED_HOSTS_FOR);
        assert!(links.run(&on(), &addr(RG)).unwrap().is_some());
        // Later, an older list stands in for one that cannot be read.
        *desk.hosts.lock().unwrap() = Err(DebridError::Network("down".into()));
        clock.set(FAILED_HOSTS_FOR + HOSTS_FOR + LINK_FOR);
        assert!(links.run(&on(), &addr(RG)).unwrap().is_some());
    }

    #[test]
    fn a_refused_key_on_the_hosts_list_fails_the_download() {
        let desk = DebridDesk::new(&["rapidgator.net"], &[(RG, DIRECT)]);
        *desk.hosts.lock().unwrap() = Err(DebridError::BadKey);
        let (links, desk, _) = resolve(desk, 0);
        let want = Err(Refused { service: DebridService::RealDebrid, error: DebridError::BadKey });
        assert_eq!(links.run(&on(), &addr(RG)), want);
        assert_eq!(links.run(&on(), &addr("https://example.com/a.zip")), want, "no list says it is not a host's");
        assert_eq!(desk.calls().len(), 1, "the refusal is kept a while");
    }

    #[test]
    fn with_an_older_list_a_refused_key_fails_only_the_hosts_links() {
        let mut desk = DebridDesk::new(&["rapidgator.net"], &[]);
        desk.links.insert(RG.into(), Err(DebridError::BadKey));
        let (links, desk, clock) = resolve(desk, 0);
        assert_eq!(links.run(&on(), &addr("https://example.com/a.zip")), Ok(None));
        *desk.hosts.lock().unwrap() = Err(DebridError::BadKey);
        clock.set(HOSTS_FOR);
        assert_eq!(links.run(&on(), &addr("https://example.com/a.zip")), Ok(None));
        let refused = links.run(&on(), &addr(RG)).unwrap_err();
        assert_eq!(refused.error, DebridError::BadKey);
    }

    #[test]
    fn the_services_refusal_fails_the_download_but_an_unsupported_host_does_not() {
        let mut desk = DebridDesk::new(&["rapidgator.net", "1fichier.com"], &[]);
        desk.links.insert(RG.into(), Err(DebridError::NotPremium));
        let (links, _, _) = resolve(desk, 0);
        let refused = links.run(&on(), &addr(RG)).unwrap_err();
        assert_eq!(refused, Refused { service: DebridService::RealDebrid, error: DebridError::NotPremium });
        assert_eq!(refused.to_string(), "Real-Debrid could not unlock this link: the account is not premium.");
        assert_eq!(links.run(&on(), &addr("https://1fichier.com/?abc")), Ok(None), "the service said no such host");
    }
}
