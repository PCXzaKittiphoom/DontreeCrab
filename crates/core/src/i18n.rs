//! Translation catalogs loaded from GNU gettext `.po` files.
//!
//! English and Thai are compiled into the binary; any additional `.po` file
//! dropped into a `locales` directory (next to the executable, in the working
//! directory, or in the per-user config directory) is picked up at startup.
//! `.po` files are read directly, so users never have to compile `.mo` files
//! or rebuild the player.

use std::collections::HashMap;
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

const BUILTIN_EN: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../locales/en.po"));
const BUILTIN_TH: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../locales/th.po"));

/// A language found in the bundled or external `.po` files.
#[derive(Clone, Debug)]
pub struct LanguageInfo {
    pub code: String,
    /// Native name shown in the language picker (e.g. `ไทย`, `English`).
    pub name: String,
}

#[derive(Clone, Debug, Default)]
struct Catalog {
    code: String,
    name: String,
    entries: HashMap<String, String>,
}

struct State {
    current: Catalog,
    catalogs: HashMap<String, Catalog>,
    languages: Vec<LanguageInfo>,
}

static STATE: OnceLock<RwLock<State>> = OnceLock::new();

fn state() -> &'static RwLock<State> {
    STATE.get_or_init(|| RwLock::new(load_state()))
}

fn read_state() -> std::sync::RwLockReadGuard<'static, State> {
    state().read().unwrap_or_else(|err| err.into_inner())
}

fn write_state() -> std::sync::RwLockWriteGuard<'static, State> {
    state().write().unwrap_or_else(|err| err.into_inner())
}

/// Load the bundled and external catalogs. Safe to call more than once.
pub fn init() {
    let _ = state();
}

/// Select a language. `"auto"` follows the operating system locale and falls
/// back to English when that language has no catalog.
pub fn set_language(preference: &str) {
    let mut state = write_state();
    let code = resolve_language(preference, &state.catalogs);
    if let Some(catalog) = state.catalogs.get(&code) {
        state.current = catalog.clone();
    }
}

pub fn current_language() -> String {
    read_state().current.code.clone()
}

/// Catalogs available in the picker, sorted by native name.
pub fn available_languages() -> Vec<LanguageInfo> {
    read_state().languages.clone()
}

/// Translate a single message. Missing entries fall back to the English
/// message id, so a partially translated catalog still works.
pub fn tr(msgid: &str) -> String {
    let state = read_state();
    state
        .current
        .entries
        .get(msgid)
        .cloned()
        .unwrap_or_else(|| msgid.to_owned())
}

/// Translate a message containing `{}` placeholders and fill them in order.
/// Translators may move placeholders around in the translated text.
pub fn tr_args(msgid: &str, args: &[&dyn Display]) -> String {
    fill(&tr(msgid), args)
}

fn fill(template: &str, args: &[&dyn Display]) -> String {
    let mut out = template.to_owned();
    for arg in args {
        let Some(index) = out.find("{}") else {
            break;
        };
        out.replace_range(index..index + 2, &arg.to_string());
    }
    out
}

fn resolve_language(preference: &str, catalogs: &HashMap<String, Catalog>) -> String {
    let preference = preference.trim().to_lowercase();
    let candidate = if preference.is_empty() || preference == "auto" {
        system_locale().to_lowercase()
    } else {
        preference
    };

    if catalogs.contains_key(&candidate) {
        return candidate;
    }
    let base = candidate
        .split(['-', '_'])
        .next()
        .unwrap_or("en")
        .to_owned();
    if catalogs.contains_key(&base) {
        return base;
    }
    if catalogs.contains_key("en") {
        return "en".to_owned();
    }
    catalogs
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| "en".to_owned())
}

#[cfg(windows)]
fn system_locale() -> String {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Control Panel\\International");
    key.and_then(|key| key.get_value::<String, _>("LocaleName"))
        .unwrap_or_else(|_| "en".to_owned())
}

#[cfg(not(windows))]
fn system_locale() -> String {
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_else(|_| "en".to_owned())
}

fn locale_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join("locales"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("locales"));
    }
    if let Some(project) = directories::ProjectDirs::from("dev", "DontreeCrab", "DontreeCrab") {
        dirs.push(project.config_dir().join("locales"));
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

fn load_state() -> State {
    let mut catalogs = HashMap::new();
    for (source, fallback) in [(BUILTIN_EN, "en"), (BUILTIN_TH, "th")] {
        if let Some(catalog) = parse_po(source, fallback) {
            catalogs.insert(catalog.code.clone(), catalog);
        }
    }

    for dir in locale_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("po"))
            {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let fallback = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("en");
            if let Some(catalog) = parse_po(&source, fallback) {
                catalogs.insert(catalog.code.clone(), catalog);
            }
        }
    }

    let current = catalogs.get("en").cloned().unwrap_or_else(Catalog::default);
    let mut languages: Vec<LanguageInfo> = catalogs
        .values()
        .map(|catalog| LanguageInfo {
            code: catalog.code.clone(),
            name: catalog.name.clone(),
        })
        .collect();
    languages.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.code.cmp(&b.code))
    });

    State {
        current,
        catalogs,
        languages,
    }
}

#[derive(Default)]
struct RawEntry {
    msgid: String,
    msgstr: String,
    fuzzy: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    None,
    MsgId,
    MsgStr,
}

/// Parse one `.po` catalog. `fallback_code` is the file stem, used when the
/// header has no `Language:` field.
fn parse_po(source: &str, fallback_code: &str) -> Option<Catalog> {
    let mut entries = HashMap::new();
    let mut code = String::new();
    let mut name = String::new();
    let mut entry = RawEntry::default();
    let mut field = Field::None;

    for line in source.lines() {
        let line = line.trim_end();
        let trimmed = line.trim_start();

        if trimmed.is_empty() {
            commit_entry(&mut entry, &mut entries, &mut code, &mut name);
            field = Field::None;
            continue;
        }
        if let Some(comment) = trimmed.strip_prefix('#') {
            if comment.starts_with(',') && comment.contains("fuzzy") {
                entry.fuzzy = true;
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("msgid_plural") {
            let _ = rest;
            field = Field::None;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("msgid") {
            if !entry.msgid.is_empty() && field == Field::MsgStr {
                commit_entry(&mut entry, &mut entries, &mut code, &mut name);
            }
            entry.msgid = parse_quoted(rest);
            field = Field::MsgId;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("msgstr[0]") {
            entry.msgstr = parse_quoted(rest);
            field = Field::MsgStr;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("msgstr[") {
            let _ = rest;
            field = Field::None;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("msgstr") {
            entry.msgstr = parse_quoted(rest);
            field = Field::MsgStr;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("msgctxt") {
            let _ = rest;
            field = Field::None;
            continue;
        }
        if trimmed.starts_with('"') {
            let text = parse_quoted(trimmed);
            match field {
                Field::MsgId => entry.msgid.push_str(&text),
                Field::MsgStr => entry.msgstr.push_str(&text),
                Field::None => {}
            }
        }
    }
    commit_entry(&mut entry, &mut entries, &mut code, &mut name);

    if code.is_empty() {
        code = fallback_code.trim().to_lowercase();
    }
    if code.is_empty() {
        return None;
    }
    if name.is_empty() {
        name = default_language_name(&code);
    }

    Some(Catalog {
        code,
        name,
        entries,
    })
}

fn commit_entry(
    entry: &mut RawEntry,
    entries: &mut HashMap<String, String>,
    code: &mut String,
    name: &mut String,
) {
    if entry.msgid.is_empty() {
        for line in entry.msgstr.lines() {
            if let Some(value) = line.strip_prefix("Language:") {
                *code = value.trim().to_lowercase();
            } else if let Some(value) = line.strip_prefix("Language-Name:") {
                *name = value.trim().to_owned();
            }
        }
    } else if !entry.fuzzy && !entry.msgstr.is_empty() {
        entries.insert(entry.msgid.clone(), entry.msgstr.clone());
    }
    *entry = RawEntry::default();
}

fn parse_quoted(rest: &str) -> String {
    let rest = rest.trim();
    let Some(start) = rest.find('"') else {
        return String::new();
    };
    let body = &rest[start + 1..];
    let end = body.rfind('"').unwrap_or(body.len());
    unescape(&body[..end])
}

fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn default_language_name(code: &str) -> String {
    match code {
        "en" => "English".to_owned(),
        "th" => "ไทย".to_owned(),
        _ => code.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_po_catalog() {
        let source = r#"
msgid ""
msgstr ""
"Language: th\n"
"Language-Name: ไทย\n"

msgid "Play"
msgstr "เล่น"

#, fuzzy
msgid "Stop"
msgstr "หยุด"

msgid ""
"multi"
"line"
msgstr ""
"หลาย"
"บรรทัด"

msgid "Quote \" and \\ slash"
msgstr "อ้าง \" และ \\"
"#;

        let catalog = parse_po(source, "xx").expect("catalog");
        assert_eq!(catalog.code, "th");
        assert_eq!(catalog.name, "ไทย");
        assert_eq!(catalog.entries.get("Play").map(String::as_str), Some("เล่น"));
        assert!(!catalog.entries.contains_key("Stop"));
        assert_eq!(
            catalog.entries.get("multiline").map(String::as_str),
            Some("หลายบรรทัด")
        );
        assert_eq!(
            catalog
                .entries
                .get("Quote \" and \\ slash")
                .map(String::as_str),
            Some("อ้าง \" และ \\")
        );
    }

    #[test]
    fn falls_back_to_the_file_stem() {
        let catalog = parse_po("msgid \"Play\"\nmsgstr \"Spielen\"\n", "de").expect("catalog");
        assert_eq!(catalog.code, "de");
        assert_eq!(catalog.name, "de");
    }

    #[test]
    fn bundled_catalogs_are_valid() {
        let en = parse_po(BUILTIN_EN, "en").expect("English catalog");
        assert_eq!(en.code, "en");
        assert_eq!(en.name, "English");

        let th = parse_po(BUILTIN_TH, "th").expect("Thai catalog");
        assert_eq!(th.code, "th");
        assert_eq!(th.name, "ไทย");
        assert_eq!(th.entries.get("PLAY").map(String::as_str), Some("เล่น"));
        assert_eq!(th.entries.get("Language").map(String::as_str), Some("ภาษา"));
    }

    #[test]
    fn fills_placeholders_in_order() {
        assert_eq!(fill("BPM : {} [{}]", &[&"120", &"+10"]), "BPM : 120 [+10]");
        assert_eq!(fill("{} lines · {}", &[&7, &"NCN"]), "7 lines · NCN");
    }
}
