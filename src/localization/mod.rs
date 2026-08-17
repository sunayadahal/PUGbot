//! User-facing strings and locale resolution.
//!
//! Every string a player can see is looked up by key. Catalogs are embedded at
//! compile time so a deployment cannot start with missing translation files,
//! and any key missing from a translation falls back to English rather than
//! rendering a blank message.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The locale used when a channel requests one that is not installed, and the
/// source of truth for which keys exist.
pub const FALLBACK_LOCALE: &str = "en";

/// Embedded catalogs, compiled into the binary so a deployment cannot start
/// with a translation file missing. Adding a language means adding a file and
/// one line here.
const CATALOGS: &[(&str, &str)] = &[
    ("en", include_str!("../../locales/en.json")),
    ("fr", include_str!("../../locales/fr.json")),
    ("ru", include_str!("../../locales/ru.json")),
    ("es", include_str!("../../locales/es.json")),
    ("it", include_str!("../../locales/it.json")),
    ("ko", include_str!("../../locales/ko.json")),
    ("pt-BR", include_str!("../../locales/pt-BR.json")),
    ("tr", include_str!("../../locales/tr.json")),
];

type Catalog = HashMap<String, String>;

fn catalogs() -> &'static HashMap<String, Catalog> {
    static LOADED: OnceLock<HashMap<String, Catalog>> = OnceLock::new();
    LOADED.get_or_init(|| {
        CATALOGS
            .iter()
            .map(|(locale, source)| {
                let catalog: Catalog = serde_json::from_str(source).unwrap_or_else(|error| {
                    // A malformed embedded catalog is a build-time mistake, and
                    // failing loudly at first use is better than shipping
                    // half-translated output.
                    panic!("locale catalog {locale} is not valid JSON: {error}")
                });
                ((*locale).to_string(), catalog)
            })
            .collect()
    })
}

/// Every installed locale, sorted. Used to build the `/channel set locale`
/// choice list.
#[must_use]
pub fn available_locales() -> Vec<&'static str> {
    let mut locales: Vec<&'static str> = CATALOGS.iter().map(|(locale, _)| *locale).collect();
    locales.sort_unstable();
    locales
}

/// Whether a requested locale resolves to an installed catalog.
#[must_use]
pub fn locale_installed(locale: &str) -> bool {
    resolve_locale(locale).is_some()
}

/// Maps a requested locale onto an installed one.
///
/// Matching is case-insensitive and falls back from a regional variant to its
/// base language, so `pt-br`, `PT-BR` and `pt` all reach the same catalog.
fn resolve_locale(requested: &str) -> Option<&'static str> {
    let requested = requested.trim();
    if requested.is_empty() {
        return None;
    }
    let installed = catalogs();
    if let Some((name, _)) = installed
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(requested))
    {
        return CATALOGS
            .iter()
            .map(|(locale, _)| *locale)
            .find(|locale| locale.eq_ignore_ascii_case(name));
    }
    let base = requested.split(['-', '_']).next().unwrap_or(requested);
    CATALOGS.iter().map(|(locale, _)| *locale).find(|locale| {
        locale.eq_ignore_ascii_case(base)
            || locale
                .split('-')
                .next()
                .is_some_and(|l| l.eq_ignore_ascii_case(base))
    })
}

/// A resolved locale, used to render every message in one interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Locale(&'static str);

impl Locale {
    /// Resolves a requested locale, falling back to English if it is not
    /// installed. Matching is case-insensitive and accepts a regional variant
    /// or its base language.
    #[must_use]
    pub fn resolve(requested: &str) -> Self {
        Self(resolve_locale(requested).unwrap_or(FALLBACK_LOCALE))
    }

    /// The English catalog, which is the source of truth for which keys exist.
    #[must_use]
    pub fn fallback() -> Self {
        Self(FALLBACK_LOCALE)
    }

    /// The catalog name, such as `pt-BR`.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.0
    }

    /// Looks up `key`, falling back to English and then to the key itself, so
    /// a message is never empty.
    ///
    /// A key missing from every catalog is a programming error that a test
    /// catches; returning the key rather than panicking means it degrades to an
    /// ugly message instead of a failed command.
    #[must_use]
    pub fn get(self, key: &str) -> &'static str {
        let all = catalogs();
        all.get(self.0)
            .and_then(|catalog| catalog.get(key))
            .or_else(|| all.get(FALLBACK_LOCALE).and_then(|c| c.get(key)))
            .map_or_else(
                || Box::leak(key.to_string().into_boxed_str()) as &'static str,
                |value| value.as_str(),
            )
    }

    /// Looks up `key` and substitutes `{name}` placeholders.
    ///
    /// A test asserts that every translation preserves exactly the placeholders
    /// its English source uses, so a substitution cannot silently go missing.
    ///
    /// # Example
    ///
    /// ```
    /// use pugbot::localization::Locale;
    ///
    /// let text = Locale::fallback().format("match.started", &[("id", "42")]);
    /// assert!(text.contains("42"));
    /// ```
    #[must_use]
    pub fn format(self, key: &str, args: &[(&str, &str)]) -> String {
        let mut text = self.get(key).to_string();
        for (name, value) in args {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        text
    }
}

impl Default for Locale {
    fn default() -> Self {
        Self::fallback()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_is_installed_and_complete() {
        let english = catalogs().get("en").expect("english catalog");
        assert!(!english.is_empty());
    }

    #[test]
    fn every_catalog_has_the_same_keys_as_english() {
        let all = catalogs();
        let english = all.get("en").expect("english catalog");
        for (locale, catalog) in all {
            if locale == "en" {
                continue;
            }
            let missing: Vec<&String> = english
                .keys()
                .filter(|key| !catalog.contains_key(*key))
                .collect();
            assert!(
                missing.is_empty(),
                "locale {locale} is missing {} keys: {missing:?}",
                missing.len()
            );
            let extra: Vec<&String> = catalog
                .keys()
                .filter(|key| !english.contains_key(*key))
                .collect();
            assert!(
                extra.is_empty(),
                "locale {locale} has keys English does not: {extra:?}"
            );
        }
    }

    #[test]
    fn every_catalog_preserves_placeholders() {
        let all = catalogs();
        let english = all.get("en").expect("english catalog");
        let placeholders = |text: &str| {
            let mut found: Vec<String> = Vec::new();
            let mut rest = text;
            while let Some(start) = rest.find('{') {
                let Some(end) = rest[start..].find('}') else {
                    break;
                };
                found.push(rest[start..start + end + 1].to_string());
                rest = &rest[start + end + 1..];
            }
            found.sort();
            found
        };
        for (locale, catalog) in all {
            if locale == "en" {
                continue;
            }
            for (key, source) in english {
                let translated = &catalog[key];
                assert_eq!(
                    placeholders(source),
                    placeholders(translated),
                    "locale {locale} key {key} changed its placeholders"
                );
            }
        }
    }

    #[test]
    fn all_eight_target_languages_are_installed() {
        for locale in ["en", "fr", "ru", "es", "it", "ko", "pt-BR", "tr"] {
            assert!(locale_installed(locale), "{locale} is missing");
        }
    }

    #[test]
    fn locale_resolution_is_case_and_region_tolerant() {
        assert_eq!(Locale::resolve("EN").name(), "en");
        assert_eq!(Locale::resolve("pt-br").name(), "pt-BR");
        assert_eq!(Locale::resolve("pt").name(), "pt-BR");
        assert_eq!(Locale::resolve("fr_FR").name(), "fr");
    }

    #[test]
    fn an_unknown_locale_falls_back_to_english() {
        assert_eq!(Locale::resolve("kl").name(), FALLBACK_LOCALE);
        assert_eq!(Locale::resolve("").name(), FALLBACK_LOCALE);
    }

    #[test]
    fn lookups_substitute_named_arguments() {
        let rendered = Locale::fallback().format(
            "queue.joined",
            &[("user", "ada"), ("current", "3"), ("size", "10")],
        );
        assert!(rendered.contains("ada"), "{rendered}");
        assert!(rendered.contains('3'), "{rendered}");
        assert!(rendered.contains("10"), "{rendered}");
        assert!(
            !rendered.contains('{'),
            "unsubstituted placeholder: {rendered}"
        );
    }

    #[test]
    fn an_unknown_key_returns_the_key_rather_than_an_empty_string() {
        assert_eq!(Locale::fallback().get("no.such.key"), "no.such.key");
    }
}
