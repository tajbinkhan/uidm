use udm_domain::debrid::{DebridKey, DebridService};
use udm_domain::ports::{DebridKeys, SettingsError, SettingsStore};

/// Store the debrid account's API key for a service, or remove it, and note
/// in the settings which service a key is stored for, so the window can say
/// so.
pub struct SetKey<'a, S> {
    keys: &'a dyn DebridKeys,
    settings: &'a S,
}

impl<'a, S: SettingsStore> SetKey<'a, S> {
    pub fn new(keys: &'a dyn DebridKeys, settings: &'a S) -> Self {
        SetKey { keys, settings }
    }

    /// Store `key` for `service`, in place of any other.
    pub fn store(&self, service: DebridService, key: &DebridKey) -> Result<(), SettingsError> {
        self.keys.set_key(service, key)?;
        self.note(Some(service))?;
        tracing::info!(service = %service, "debrid key stored");
        Ok(())
    }

    /// Remove the stored key.
    pub fn remove(&self) -> Result<(), SettingsError> {
        self.keys.remove_key()?;
        self.note(None)?;
        tracing::info!("debrid key removed");
        Ok(())
    }

    fn note(&self, service: Option<DebridService>) -> Result<(), SettingsError> {
        let mut settings = self.settings.settings();
        settings.debrid.key_service = service;
        self.settings.save_settings(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Keyring, Prefs};
    use udm_domain::settings::Settings;

    #[test]
    fn the_settings_say_which_service_a_key_is_stored_for() {
        let (keys, prefs) = (Keyring::default(), Prefs::with(Settings::default()));
        let set = SetKey::new(&keys, &prefs);
        set.store(DebridService::AllDebrid, &DebridKey::new("abc").unwrap()).unwrap();
        assert_eq!(keys.key(DebridService::AllDebrid).map(|k| k.expose().to_string()).as_deref(), Some("abc"));
        assert_eq!(keys.key(DebridService::RealDebrid), None, "the key is AllDebrid's only");
        assert_eq!(prefs.settings().debrid.key_service, Some(DebridService::AllDebrid));
        set.remove().unwrap();
        assert!(keys.stored().is_none());
        assert_eq!(prefs.settings().debrid.key_service, None);
        assert_eq!(prefs.saves(), 2);
    }
}
