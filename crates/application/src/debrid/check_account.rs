use udm_domain::debrid::{DebridCheck, DebridError, DebridKey, DebridService};
use udm_domain::ports::{Debrid, DebridKeys};

/// Ask a debrid service about the account, with a key just typed or else
/// the key stored for that service, as Options' Check account does.
pub struct CheckAccount<'a> {
    debrid: &'a dyn Debrid,
    keys: &'a dyn DebridKeys,
}

impl<'a> CheckAccount<'a> {
    pub fn new(debrid: &'a dyn Debrid, keys: &'a dyn DebridKeys) -> Self {
        CheckAccount { debrid, keys }
    }

    /// The account `service` sees for `key`, or for the key stored for it
    /// without one. A key stored for another service is never sent:
    /// that is `KeyForOther`, and no key at all is `BadKey`.
    pub fn run(&self, service: DebridService, key: Option<DebridKey>) -> DebridCheck {
        let outcome = match key.or_else(|| self.keys.key(service)) {
            Some(key) => self.debrid.account(service, &key),
            None => match self.keys.stored() {
                Some(other) if other != service => Err(DebridError::KeyForOther(other)),
                _ => Err(DebridError::BadKey),
            },
        };
        match &outcome {
            Ok(account) => tracing::info!(service = %service, premium = account.premium, "debrid account checked"),
            Err(e) => tracing::info!(service = %service, error = %e, "debrid account check failed"),
        }
        DebridCheck { service, outcome }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{DebridDesk, Keyring};
    use udm_domain::debrid::DebridAccount;

    #[test]
    fn a_typed_key_goes_before_the_stored_one() {
        let account = DebridAccount { username: Some("ann".into()), premium: true, premium_until: Some(10) };
        let desk = DebridDesk { account: Some(account.clone()), ..DebridDesk::default() };
        let keys = Keyring::holding(DebridService::Premiumize, "stored");
        let check = CheckAccount::new(&desk, &keys);
        assert_eq!(check.run(DebridService::RealDebrid, DebridKey::new("typed")).outcome, Ok(account.clone()));
        assert_eq!(check.run(DebridService::Premiumize, None).outcome, Ok(account));
        assert_eq!(desk.calls(), ["account Real-Debrid typed", "account Premiumize stored"]);
    }

    #[test]
    fn another_services_key_is_never_sent() {
        let desk = DebridDesk::default();
        let keys = Keyring::holding(DebridService::RealDebrid, "rd-token");
        let check = CheckAccount::new(&desk, &keys).run(DebridService::AllDebrid, None);
        assert_eq!(check.outcome, Err(DebridError::KeyForOther(DebridService::RealDebrid)));
        assert!(desk.calls().is_empty());
    }

    #[test]
    fn without_a_key_nothing_is_asked() {
        let desk = DebridDesk::default();
        let check = CheckAccount::new(&desk, &Keyring::default()).run(DebridService::AllDebrid, None);
        assert_eq!(check, DebridCheck { service: DebridService::AllDebrid, outcome: Err(DebridError::BadKey) });
        assert!(desk.calls().is_empty());
    }
}
