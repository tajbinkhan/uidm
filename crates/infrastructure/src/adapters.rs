//! The computer's network adapters (`NetworkAdapters`), read with `if-addrs`
//! each time they are asked for: interfaces with their addresses, grouped
//! by adapter. On Windows an adapter is named for good by its GUID and
//! shown by its friendly name; elsewhere its interface name is both.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use udm_domain::sync::LockExt;

use udm_domain::network::{AdapterId, NetworkAdapter, NetworkError, Subnet};
use udm_domain::ports::NetworkAdapters;

/// The system's adapters.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemAdapters;

impl NetworkAdapters for SystemAdapters {
    fn list(&self) -> Result<Vec<NetworkAdapter>, NetworkError> {
        let interfaces = if_addrs::get_if_addrs().map_err(|e| NetworkError::Adapters(e.to_string()))?;
        let mut adapters: BTreeMap<String, NetworkAdapter> = BTreeMap::new();
        for interface in interfaces {
            #[cfg(windows)]
            let id = interface.adapter_name.clone();
            #[cfg(not(windows))]
            let id = interface.name.clone();
            let Ok(adapter_id) = AdapterId::parse(&id) else {
                continue;
            };
            let up = interface.is_oper_up();
            let entry = adapters.entry(id).or_insert_with(|| NetworkAdapter {
                id: adapter_id,
                name: interface.name.clone(),
                addresses: Vec::new(),
                subnets: Vec::new(),
                up,
            });
            entry.up &= up;
            if !entry.addresses.contains(&interface.ip()) {
                entry.addresses.push(interface.ip());
            }
            let prefix = match &interface.addr {
                if_addrs::IfAddr::V4(v4) => v4.prefixlen,
                if_addrs::IfAddr::V6(v6) => v6.prefixlen,
            };
            if let Some(subnet) = Subnet::new(interface.ip(), prefix) {
                if !entry.subnets.contains(&subnet) {
                    entry.subnets.push(subnet);
                }
            }
        }
        Ok(adapters.into_values().collect())
    }
}

/// The adapters from another `NetworkAdapters`, read at most once each
/// `max_age`: every connection and the watcher look at the adapter, and
/// this keeps that to one reading of the system's list a second, however
/// many downloads run. A list that cannot be read is logged once, when it
/// first fails.
pub struct Cached {
    inner: Arc<dyn NetworkAdapters>,
    max_age: Duration,
    last: Mutex<Option<(Instant, Listed)>>,
}

/// One reading of the adapters.
type Listed = Result<Vec<NetworkAdapter>, NetworkError>;

impl Cached {
    pub fn new(inner: Arc<dyn NetworkAdapters>, max_age: Duration) -> Cached {
        Cached { inner, max_age, last: Mutex::new(None) }
    }
}

impl NetworkAdapters for Cached {
    fn list(&self) -> Result<Vec<NetworkAdapter>, NetworkError> {
        // Callers wait for one reading rather than each make their own.
        let mut last = self.last.lock_or_recover();
        if let Some((at, list)) = last.as_ref() {
            if at.elapsed() < self.max_age {
                return list.clone();
            }
        }
        let list = self.inner.list();
        if let Err(e) = &list {
            if !matches!(last.as_ref(), Some((_, Err(_)))) {
                tracing::warn!(error = %e, "the network adapters could not be read");
            }
        }
        *last = Some((Instant::now(), list.clone()));
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn the_list_is_read_once_a_while() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counted(AtomicUsize);
        impl NetworkAdapters for Counted {
            fn list(&self) -> Result<Vec<NetworkAdapter>, NetworkError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            }
        }
        let counted = Arc::new(Counted(AtomicUsize::new(0)));
        let cached = Cached::new(counted.clone(), Duration::from_millis(200));
        for _ in 0..50 {
            cached.list().unwrap();
        }
        assert_eq!(counted.0.load(Ordering::SeqCst), 1);
        std::thread::sleep(Duration::from_millis(250));
        cached.list().unwrap();
        assert_eq!(counted.0.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_loopback_adapter_is_listed_with_its_address() {
        let all = SystemAdapters.list().unwrap();
        let lo = all.iter().find(|a| a.addresses.contains(&IpAddr::V4(Ipv4Addr::LOCALHOST))).expect("loopback");
        assert!(lo.usable(), "{lo:?}");
        assert!(lo.subnets.iter().any(|s| s.contains(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)))), "{lo:?}");
        assert!(!lo.name.is_empty());
    }
}
