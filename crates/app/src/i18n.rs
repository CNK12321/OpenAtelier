//! Translations.
//!
//! Two ways in, one catalog per language:
//!
//! * **The English text is the key** — [`tr`] for text written in the code
//!   (`ui.button(tr("Save"))`), [`t`] for text that isn't a literal (an effect's name, a
//!   parameter's), [`trf`] for text with values in it (`trf("{n} clips", &[("n", …)])`).
//!   A language that hasn't translated something shows it in English. This is what the
//!   editor uses: no key names to invent, and the code reads as the UI does.
//! * **Named keys** (`home.new_project`) for the start page's older strings, with
//!   English in `locales/en.json`.
//!
//! Languages are JSON files of `"key or English text": "translation"`: built in
//! (`locales/es.json`, Spanish), or dropped into `<config>/locales/<code>.json` —
//! which also adds to or corrects a built-in one — without rebuilding. The language
//! comes from the settings file, `OA_LANG`, or the system, in that order.
//!
//! A test (`every_ui_string_is_translatable`) scans the editor's source for text passed
//! straight to the UI without [`tr`], and for text [`tr`] is given that Spanish lacks.

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

/// English for the named keys, always available.
const EN: &str = include_str!("../locales/en.json");

/// The languages built in: code, name (in itself), catalog.
const BUILT_IN: &[(&str, &str, &str)] = &[("es", "Español", include_str!("../locales/es.json"))];

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
    ACTIVE.get_or_init(|| Active { lang: "en".into(), strings: Catalog::new(), english: parse(EN) })
}

/// A language's name, in itself: "English", "Español"; the code for one dropped in.
pub fn name_of(code: &str) -> String {
    match code {
        "en" => "English".into(),
        _ => BUILT_IN.iter().find(|(c, ..)| *c == code).map_or_else(|| code.to_string(), |(_, name, _)| name.to_string()),
    }
}

/// The languages that can be picked: English, the built-in ones, and every
/// `<dir>/*.json`.
pub fn available(dir: &std::path::Path) -> Vec<String> {
    let mut out = vec!["en".to_string()];
    out.extend(BUILT_IN.iter().map(|(c, ..)| c.to_string()));
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

/// A language's strings: the built-in catalog (if there is one), then `<dir>/<code>.json`
/// over it.
fn catalog_for(code: &str, dir: &std::path::Path) -> Catalog {
    let mut strings = BUILT_IN.iter().find(|(c, ..)| *c == code).map(|(.., text)| parse(text)).unwrap_or_default();
    if let Ok(text) = std::fs::read_to_string(dir.join(format!("{code}.json"))) {
        strings.extend(parse(&text));
    }
    strings
}

/// Picks the language once, at startup: `lang` (from the settings file), else `OA_LANG`,
/// else the system's, else English.
pub fn init(lang: Option<&str>, dir: &std::path::Path) {
    let wanted = lang
        .map(str::to_string)
        .or_else(|| std::env::var("OA_LANG").ok())
        .or_else(system_language)
        .unwrap_or_else(|| "en".into());
    // "es-MX" takes Spanish, with anything "es-MX.json" says over it.
    let base = wanted.split(['-', '_']).next().unwrap_or("en").to_lowercase();
    let mut strings = Catalog::new();
    for name in [base.clone(), wanted.clone()] {
        if name != "en" {
            strings.extend(catalog_for(&name, dir));
        }
    }
    let lang = if strings.is_empty() { "en".to_string() } else { wanted };
    let _ = ACTIVE.set(Active { lang, strings, english: parse(EN) });
}

/// The language in force.
pub fn language() -> &'static str {
    &active().lang
}

/// Text written in the code, in the language in force (itself when there's no
/// translation — English — without copying anything).
pub fn tr(text: &'static str) -> &'static str {
    let a = active();
    if a.strings.is_empty() {
        return text;
    }
    a.strings.get(text).map_or(text, String::as_str)
}

/// [`tr`] with values filled into `{placeholders}`.
pub fn trf(text: &'static str, values: &[(&str, &str)]) -> String {
    fill(tr(text), values)
}

/// The text for `key` (a named key, or English text that isn't a literal — an effect's
/// name), in the language in force, falling back to English and then to the key itself.
pub fn t(key: &str) -> &'static str {
    let a = active();
    a.strings.get(key).or_else(|| a.english.get(key)).map(|s| s.as_str()).unwrap_or_else(|| leak(key))
}

/// Text that may or may not be English with a translation (a tooltip handed to a
/// helper, which may hold a clip's name): its translation, or itself — never kept, so
/// text that changes all the time costs nothing.
pub fn tx(text: &str) -> std::borrow::Cow<'_, str> {
    match active().strings.get(text) {
        Some(s) => std::borrow::Cow::Borrowed(s.as_str()),
        None => std::borrow::Cow::Borrowed(text),
    }
}

/// [`t`] with values filled into `{placeholders}`.
pub fn args(key: &str, values: &[(&str, &str)]) -> String {
    fill(t(key), values)
}

fn fill(text: &str, values: &[(&str, &str)]) -> String {
    let mut text = text.to_string();
    for (name, value) in values {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

/// A key with no translation: kept alive so callers can treat every result the same
/// (each once: what can be shown is a bounded set).
fn leak(key: &str) -> &'static str {
    static MISSING: OnceLock<std::sync::Mutex<HashSet<&'static str>>> = OnceLock::new();
    let set = MISSING.get_or_init(Default::default);
    let mut set = set.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = set.get(key) {
        return found;
    }
    let leaked: &'static str = Box::leak(key.to_string().into_boxed_str());
    set.insert(leaked);
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
        for (code, _, text) in BUILT_IN {
            let catalog: Catalog = serde_json::from_str(text).unwrap_or_else(|e| panic!("locales/{code}.json: {e}"));
            for (k, v) in &catalog {
                assert!(!v.trim().is_empty(), "{code}: \"{k}\" is empty");
                // Placeholders stay: a translation that drops "{n}" loses the number.
                for p in placeholders(k) {
                    assert!(v.contains(&p), "{code}: \"{k}\" → \"{v}\" lost {p}");
                }
            }
        }
    }

    fn placeholders(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = s;
        while let Some(i) = rest.find('{') {
            let Some(j) = rest[i..].find('}') else { break };
            let p = &rest[i..i + j + 1];
            if p.len() > 2 && p[1..p.len() - 1].chars().all(|c| c.is_alphanumeric() || c == '_') {
                out.push(p.to_string());
            }
            rest = &rest[i + j + 1..];
        }
        out
    }

    /// `text` with its test modules (`#[cfg(test)] mod … { … }`) blanked out, lines kept
    /// (so line numbers still match) — wherever they are: a file can go on after one.
    fn without_tests(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(i) = rest.find("#[cfg(test)]") {
            out.push_str(&rest[..i]);
            let after = &rest[i..];
            // Its block: from the first `{` to the brace that closes it.
            let Some(open) = after.find('{') else {
                rest = "";
                break;
            };
            let mut depth = 0;
            let mut end = after.len();
            for (k, c) in after[open..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + k + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            out.extend(after[..end].chars().filter(|c| *c == '\n'));
            rest = &after[end..];
        }
        out.push_str(rest);
        out
    }

    #[test]
    fn test_modules_are_left_out_wherever_they_are() {
        let src = "a(\"x\");\n#[cfg(test)]\nmod tests {\n    fn f() { g(\"y\"); }\n}\nb(\"z\");\n";
        let kept = without_tests(src);
        assert!(kept.contains("a(\"x\")") && kept.contains("b(\"z\")") && !kept.contains("\"y\""), "{kept:?}");
        assert_eq!(kept.lines().count(), src.lines().count());
    }

    /// The editor's source, file by file, without its tests.
    fn sources() -> Vec<(String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "rs") && !p.ends_with("i18n.rs"))
            .map(|p| {
                let text = std::fs::read_to_string(&p).unwrap();
                let code = without_tests(&text);
                (p.file_name().unwrap().to_string_lossy().to_string(), code)
            })
            .collect();
        out.sort();
        out
    }

    /// The string literal starting at `s` (just after its opening quote), unescaped, and
    /// where it ends.
    fn literal(s: &str) -> Option<(String, usize)> {
        let mut out = String::new();
        let mut chars = s.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => return Some((out, i + 1)),
                '\\' => match chars.next()?.1 {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    other => out.push(other),
                },
                c => out.push(c),
            }
        }
        None
    }

    /// Words in it: text a person reads (not "✕", "·", " px" alone is fine to leave).
    fn wordy(s: &str) -> bool {
        s.chars().filter(|c| c.is_alphabetic()).count() >= 2
    }

    /// Where the UI is handed text: these, followed by a string literal, show it as is.
    const UI_CALLS: &[&str] = &[
        ".label(\"",
        "button(\"",
        ".on_hover_text(\"",
        ".on_disabled_hover_text(\"",
        ".hint_text(\"",
        ".heading(\"",
        ".selected_text(\"",
        "RichText::new(\"",
        "Button::new(\"",
        "Label::new(\"",
        ".menu_button(\"",
        "notify(\"",
        "report_error(\"",
        ".text(\"",
    ];

    /// Every string the editor hands the UI goes through `tr` (or a key), and Spanish
    /// has every one `tr` is given. Formatted text (`format!` into the UI) is listed
    /// for the record: it's translated through `trf` where it's been moved over.
    #[test]
    fn every_ui_string_is_translatable() {
        let es: Catalog = serde_json::from_str(BUILT_IN[0].2).unwrap();
        let (mut bare, mut missing, mut formatted) = (Vec::new(), Vec::new(), Vec::new());
        for (file, code) in sources() {
            for (n, line) in code.lines().enumerate() {
                let at = format!("{file}:{}", n + 1);
                for call in UI_CALLS {
                    let mut rest = line;
                    while let Some(i) = rest.find(call) {
                        rest = &rest[i + call.len()..];
                        if let Some((text, _)) = literal(rest)
                            && wordy(&text)
                        {
                            bare.push(format!("{at}: \"{text}\""));
                        }
                    }
                    if line.contains(&format!("{}format!(", &call[..call.len() - 1])) {
                        formatted.push(at.clone());
                    }
                }
                let mut rest = line;
                while let Some(i) = rest.find("tr(\"").or_else(|| rest.find("trf(\"")) {
                    let skip = if rest[i..].starts_with("trf") { 5 } else { 4 };
                    // Not another function whose name ends in "tr" (`attr(`).
                    let word_start = i == 0 || !rest[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_');
                    rest = &rest[i + skip..];
                    if let Some((text, end)) = literal(rest) {
                        if word_start && wordy(&text) && !es.contains_key(&text) {
                            missing.push(text);
                        }
                        rest = &rest[end..];
                    }
                }
            }
        }
        missing.sort();
        missing.dedup();
        eprintln!("{} places hand the UI formatted text (see trf): {}", formatted.len(), formatted.join(", "));
        assert!(bare.is_empty(), "{} strings go to the UI without tr():\n{}", bare.len(), bare.join("\n"));
        assert!(missing.is_empty(), "{} strings have no Spanish yet:\n{}", missing.len(), missing.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join("\n"));
    }

    /// Spanish, loaded: literals and named keys come back translated; what it lacks, in
    /// English; values fill in.
    #[test]
    fn spanish_translates() {
        let es = catalog_for("es", std::path::Path::new("no-such-dir"));
        assert_eq!(es.get("Save").map(String::as_str), Some("Guardar"));
        assert_eq!(es.get("home.new_project").map(String::as_str), Some("Proyecto nuevo"));
        assert!(available(std::path::Path::new("no-such-dir")).contains(&"es".to_string()));
        assert_eq!(name_of("es"), "Español");
    }
}

