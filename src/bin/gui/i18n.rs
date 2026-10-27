//! The window's text in the user's language.
//!
//! Every string the window shows is written in the code in English, and that
//! English text is also its message ID: `tr("Save")` looks it up in the
//! chosen language's table and gives back the English when the table lacks
//! it. `trf!` does the same for text with `{name}` placeholders, filling them
//! in after the lookup so a translation can move them.
//!
//! The tables are `lang/<code>.json`, built into the program: one JSON object
//! from English text to its translation. To add a language, add its file and
//! a line to `LANGUAGES`; the tests check that every key is still used and
//! keeps its placeholders.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

pub struct Language {
    /// The ISO 639-1 code, as Windows names its display languages.
    pub code: &'static str,
    /// The language's name in that language, for the Options menu.
    pub name: &'static str,
    json: &'static str,
}

pub const LANGUAGES: &[Language] = &[
    Language {
        code: "en",
        name: "English",
        json: "{}",
    },
    Language {
        code: "de",
        name: "Deutsch",
        json: include_str!("lang/de.json"),
    },
    Language {
        code: "es",
        name: "Español",
        json: include_str!("lang/es.json"),
    },
    Language {
        code: "fr",
        name: "Français",
        json: include_str!("lang/fr.json"),
    },
];

type Table = HashMap<&'static str, &'static str>;

/// The current language's table; `None` while it is English.
static CURRENT: RwLock<Option<&'static Table>> = RwLock::new(None);

/// Each language's table, parsed on first use. The strings live as long as
/// the program, so `tr` can hand out `&'static str` like the literals it
/// replaces, and switching back and forth parses nothing twice.
fn table(index: usize) -> &'static Table {
    static TABLES: OnceLock<Vec<OnceLock<Table>>> = OnceLock::new();
    let tables = TABLES.get_or_init(|| LANGUAGES.iter().map(|_| OnceLock::new()).collect());
    tables[index].get_or_init(|| {
        let parsed: HashMap<String, String> = serde_json::from_str(LANGUAGES[index].json).unwrap_or_default();
        parsed
            .into_iter()
            .filter(|(_, text)| !text.is_empty())
            .map(|(en, text)| (&*Box::leak(en.into_boxed_str()), &*Box::leak(text.into_boxed_str())))
            .collect()
    })
}

/// The language Options names, or for an empty setting the first Windows
/// display language UIDM has, else English.
pub fn resolve(setting: &str) -> &'static Language {
    let wanted = if setting.is_empty() {
        crate::win::display_languages()
    } else {
        vec![setting.to_string()]
    };
    wanted
        .iter()
        .find_map(|tag| {
            let primary = tag.split(['-', '_']).next().unwrap_or("");
            LANGUAGES.iter().find(|l| l.code.eq_ignore_ascii_case(primary))
        })
        .unwrap_or(&LANGUAGES[0])
}

/// Switch the window's text to the language a setting names (see `resolve`).
pub fn set_language(setting: &str) {
    let language = resolve(setting);
    let index = LANGUAGES.iter().position(|l| std::ptr::eq(l, language)).unwrap_or(0);
    *CURRENT.write().unwrap() = (index != 0).then(|| table(index));
}

/// `english` in the current language.
pub fn tr(english: &'static str) -> &'static str {
    match *CURRENT.read().unwrap() {
        Some(table) => table.get(english).copied().unwrap_or(english),
        None => english,
    }
}

/// `tr` for text with `{name}` placeholders, which are filled in afterwards:
/// `trf!("{n} files", n = count)`.
macro_rules! trf {
    ($english:literal $(, $name:ident = $value:expr)* $(,)?) => {{
        #[allow(unused_mut)]
        let mut text = $crate::i18n::tr($english).to_string();
        $(text = text.replace(concat!("{", stringify!($name), "}"), &$value.to_string());)*
        text
    }};
}
pub(crate) use trf;

#[cfg(test)]
mod tests {
    use super::*;

    /// The sources the window's strings come from.
    const SOURCES: &[&str] = &[
        include_str!("../uidm-gui.rs"),
        include_str!("columns.rs"),
        include_str!("connections.rs"),
        include_str!("droptarget.rs"),
        include_str!("find.rs"),
        include_str!("links.rs"),
        include_str!("properties.rs"),
        include_str!("queues.rs"),
        include_str!("tools.rs"),
        include_str!("tray.rs"),
        // Labels the window passes through `tr`.
        include_str!("../../queue.rs"),
        include_str!("../../settings.rs"),
    ];

    /// The `{name}` placeholders in a string, sorted.
    fn placeholders(text: &str) -> Vec<&str> {
        let mut found: Vec<&str> = text
            .match_indices('{')
            .filter_map(|(i, _)| {
                let rest = &text[i + 1..];
                let end = rest.find('}')?;
                let name = &rest[..end];
                (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(name)
            })
            .collect();
        found.sort_unstable();
        found
    }

    /// `text` as it appears in the source, inside a string literal.
    fn escaped(text: &str) -> String {
        format!("{text:?}")
    }

    #[test]
    fn every_table_parses_and_keeps_placeholders() {
        for (i, language) in LANGUAGES.iter().enumerate() {
            let parsed: HashMap<String, String> = serde_json::from_str(language.json)
                .unwrap_or_else(|e| panic!("lang/{}.json: {e}", language.code));
            for (en, text) in &parsed {
                if !text.is_empty() {
                    assert_eq!(placeholders(en), placeholders(text), "{}: {en:?} -> {text:?}", language.code);
                }
            }
            assert_eq!(table(i).len(), parsed.values().filter(|t| !t.is_empty()).count());
        }
    }

    #[test]
    fn every_key_is_a_string_in_the_window() {
        for language in &LANGUAGES[1..] {
            let parsed: HashMap<String, String> = serde_json::from_str(language.json).unwrap();
            for en in parsed.keys() {
                let literal = escaped(en);
                assert!(
                    SOURCES.iter().any(|s| s.contains(&literal)),
                    "lang/{}.json has {literal}, which the window no longer uses",
                    language.code
                );
            }
        }
    }

    #[test]
    fn resolve_picks_the_language() {
        assert_eq!(resolve("de").code, "de");
        assert_eq!(resolve("fr-CA").code, "fr");
        assert_eq!(resolve("xx").code, "en");
    }

    #[test]
    fn trf_fills_placeholders() {
        assert_eq!(trf!("{n} of {total}", n = 2, total = "5"), "2 of 5");
    }
}
