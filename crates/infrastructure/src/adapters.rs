//! The computer's network adapters (`NetworkAdapters`), read with `if-addrs`
//! each time they are asked for: interfaces with their addresses, grouped
//! by adapter. On Windows an adapter is named for good by its GUID and
//! shown by its friendly name; elsewhere its interface name is both.

use std::collections::BTreeMap;

use udm_domain::network::{AdapterId, NetworkAdapter, NetworkError};
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
                up,
            });
            entry.up &= up;
            if !entry.addresses.contains(&interface.ip()) {
                entry.addresses.push(interface.ip());
            }
        }
        Ok(adapters.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn the_loopback_adapter_is_listed_with_its_address() {
        let all = SystemAdapters.list().unwrap();
        let lo = all.iter().find(|a| a.addresses.contains(&IpAddr::V4(Ipv4Addr::LOCALHOST))).expect("loopback");
        assert!(lo.usable(), "{lo:?}");
        assert!(!lo.name.is_empty());
    }
}
