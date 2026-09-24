//! Translations.
//!
//! Text the user reads goes through [`t`] (or [`args`] when it has values in it), which
//! looks a key up in the language in force. English ships inside the binary and is the
//! fallback for anything a translation hasn't covered yet, so a half-finished
//! translation is still usable and a missing key shows as itself rather than blank.
//!
//! Languages are JSON files of `"key": "text"`, in `crates/app/locales/` (built in) and
//! in `<config>/locales/` — so a translation can be dropped in beside the app without
//! rebuilding it. The language comes from the settings file, `OA_LANG`, or the system,
//! in that order.
//!
//! Only the start page, the notifications and error messages are translated so far; the
//! editor's own labels are being moved over key by key.

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// English, always available: the fallback and the source of truth for the key list.
const EN: &str = include_str!("../locales/en.json");

type Catalog = BTreeMap<String, String>;

struct Active {
    lang: String,
    strings: Catalog,
    english: Catalog,
}

static ACTIVE: OnceLock<Active> = OnceLock::new();

fn parse(text: &str) -> Catalog {
    serde_json::from_str(text).unwrap_or_default()
}

fn active() -> &'static Active {
    ACTIVE.get_or_init(|| Active { lang: "en".into(), strings: parse(EN), english: parse(EN) })
}

/// The language files that can be picked: `en` plus every `<config>/locales/*.json`.
pub fn available(dir: &std::path::Path) -> Vec<String> {
    let mut out = vec!["en".to_string()];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().is_some_and(|x| x == "json")
                && let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().to_string())
                && !out.contains(&stem)
            {
                out.push(stem);
            }
        }
    }
    out.sort();
    out
}

/// Picks the language once, at startup: `lang` (from the settings file), else `OA_LANG`,
/// else the system's, else English. Extra strings are read from `<dir>/<lang>.json`.
pub fn init(lang: Option<&str>, dir: &std::path::Path) {
    let wanted = lang
        .map(str::to_string)
        .or_else(|| std::env::var("OA_LANG").ok())
        .or_else(system_language)
        .unwrap_or_else(|| "en".into());
    // "en-GB" falls back to "en" when only the base language is translated.
    let base = wanted.split(['-', '_']).next().unwrap_or("en").to_string();
    let english = parse(EN);
    let mut strings = Catalog::new();
    for name in [base.clone(), wanted.clone()] {
        if name == "en" {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(dir.join(format!("{name}.json"))) {
            strings.extend(parse(&text));
        }
    }
    let lang = if strings.is_empty() { "en".to_string() } else { wanted };
    let _ = ACTIVE.set(Active { lang, strings, english });
}

/// The language in force.
pub fn language() -> &'static str {
    &active().lang
}

/// The text for `key`, in the language in force, falling back to English and then to the
/// key itself (so a missing key is obvious but harmless).
pub fn t(key: &str) -> &'static str {
    let a = active();
    a.strings.get(key).or_else(|| a.english.get(key)).map(|s| s.as_str()).unwrap_or_else(|| leak(key))
}

/// [`t`] with values filled into `{placeholders}`.
pub fn args(key: &str, values: &[(&str, &str)]) -> String {
    let mut text = t(key).to_string();
    for (name, value) in values {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

/// A key with no translation: kept alive so callers can treat every result the same.
fn leak(key: &str) -> &'static str {
    static MISSING: OnceLock<std::sync::Mutex<Vec<&'static str>>> = OnceLock::new();
    let list = MISSING.get_or_init(Default::default);
    let mut list = list.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = list.iter().find(|k| **k == key) {
        return found;
    }
    let leaked: &'static str = Box::leak(key.to_string().into_boxed_str());
    list.push(leaked);
    leaked
}

#[cfg(windows)]
fn system_language() -> Option<String> {
    // The user's display language, e.g. "en-GB" or "fr-FR".
    let mut buf = [0u16; 85];
    let n = unsafe { windows::Win32::Globalization::GetUserDefaultLocaleName(&mut buf) };
    (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]))
}

#[cfg(not(windows))]
fn system_language() -> Option<String> {
    std::env::var("LC_ALL").or_else(|_| std::env::var("LANG")).ok().map(|l| l.split('.').next().unwrap_or("en").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// English is embedded, so keys resolve with no files around; unknown keys come back
    /// as themselves rather than panicking or showing nothing.
    #[test]
    fn english_is_the_fallback() {
        assert_eq!(t("home.new_project"), "New project");
        assert_eq!(t("nope.not.a.key"), "nope.not.a.key");
        assert_eq!(args("error.name_empty", &[("kind", "track")]), "A track needs a name.");
    }

    /// Every value in the catalog is non-empty and the file parses (a broken catalog
    /// would silently blank the UI).
    #[test]
    fn the_catalog_is_sound() {
        let en = parse(EN);
        assert!(en.len() > 20, "{} keys", en.len());
        for (k, v) in &en {
            assert!(!v.trim().is_empty(), "{k} is empty");
        }
    }
}

