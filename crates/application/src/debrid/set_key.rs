use udm_domain::debrid::DebridKey;
use udm_domain::ports::{DebridKeys, SettingsError, SettingsStore};

/// Store the debrid account's API key, or remove it, and note in the
/// settings whether one is stored, so the window can say so.
pub struct SetKey<'a, S> {
    keys: &'a dyn DebridKeys,
    settings: &'a S,
}

impl<'a, S: SettingsStore> SetKey<'a, S> {
    pub fn new(keys: &'a dyn DebridKeys, settings: &'a S) -> Self {
        SetKey { keys, settings }
    }

    pub fn run(&self, key: Option<DebridKey>) -> Result<(), SettingsError> {
        self.keys.set_key(key.as_ref())?;
        let mut settings = self.settings.settings();
        settings.debrid.key_stored = key.is_some();
        self.settings.save_settings(settings)?;
        tracing::info!(stored = key.is_some(), "debrid key changed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Keyring, Prefs};
    use udm_domain::ports::DebridKeys;
    use udm_domain::settings::Settings;

    #[test]
    fn the_settings_say_whether_a_key_is_stored() {
        let (keys, prefs) = (Keyring::default(), Prefs::with(Settings::default()));
        SetKey::new(&keys, &prefs).run(DebridKey::new("abc")).unwrap();
        assert_eq!(keys.key().map(|k| k.expose().to_string()).as_deref(), Some("abc"));
        assert!(prefs.settings().debrid.key_stored);
        SetKey::new(&keys, &prefs).run(None).unwrap();
        assert!(keys.key().is_none());
        assert!(!prefs.settings().debrid.key_stored);
        assert_eq!(prefs.saves(), 2);
    }
}
