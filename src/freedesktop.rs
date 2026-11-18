//! Pure parsers for freedesktop.org formats: `.desktop` entries, shared-mime
//! globs, `mimeapps.list` and `mimeinfo.cache`.
//!
//! Compiled on Linux, and everywhere for tests (`cfg(any(target_os, test))`),
//! so the parsing logic can be unit-tested on any host.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::model::FILE_PLACEHOLDER;

// ---------------------------------------------------------- .desktop files --

/// A parsed `[Desktop Entry]` section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopEntry {
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    /// `Type` key; only `Application` entries are apps.
    pub entry_type: String,
    pub exec: Option<String>,
    pub try_exec: Option<String>,
    pub icon: Option<String>,
    pub mime_types: Vec<String>,
    pub no_display: bool,
    pub hidden: bool,
    pub terminal: bool,
}

/// Parse one `.desktop` file's text. `locale` (e.g. `zh_CN`) selects
/// localized `Name[zh_CN]` keys when present.
pub fn parse_desktop_entry(text: &str, locale: &str) -> DesktopEntry {
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut in_desktop_entry = false;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            fields.insert(k.trim().to_string(), v.to_string());
        }
    }

    let get = |k: &str| fields.get(k).map(|v| unescape_desktop(v));
    let localized = |k: &str| pick_localized(&fields, k, locale);

    let bool_of = |k: &str| get(k).map(|v| v == "true").unwrap_or(false);

    DesktopEntry {
        name: localized("Name").unwrap_or_default(),
        generic_name: localized("GenericName"),
        comment: localized("Comment"),
        entry_type: get("Type").unwrap_or_else(|| "Application".into()),
        exec: get("Exec"),
        try_exec: get("TryExec"),
        icon: get("Icon"),
        mime_types: get("MimeType")
            .map(|s| {
                s.split(';')
                    .map(|m| m.trim().to_lowercase())
                    .filter(|m| !m.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        no_display: bool_of("NoDisplay"),
        hidden: bool_of("Hidden"),
        terminal: bool_of("Terminal"),
    }
}

/// Pick `Key[locale]` with the best specificity match, falling back to the
/// plain `Key`. Locale tags that do not match `locale` are ignored.
fn pick_localized(fields: &BTreeMap<String, String>, key: &str, locale: &str) -> Option<String> {
    let loc = locale.split('.').next().unwrap_or("");
    let (loc, modifier) = match loc.split_once('@') {
        Some((l, m)) => (l, Some(m)),
        None => (loc, None),
    };
    let lang = loc.split('_').next().unwrap_or("");

    let mut best: Option<(u8, String)> = None;
    for (k, v) in fields {
        let score = if k == key {
            2
        } else if let Some(tag) = k
            .strip_prefix(&format!("{key}["))
            .and_then(|t| t.strip_suffix(']'))
        {
            if modifier.is_some() && !loc.is_empty() && tag == format!("{loc}@{}", modifier.unwrap_or(""))
            {
                6
            } else if !loc.is_empty() && tag == loc {
                5
            } else if !lang.is_empty() && tag == lang {
                4
            } else {
                continue; // a locale we do not understand never wins
            }
        } else {
            continue;
        };
        if best.as_ref().map(|(s, _)| *s < score).unwrap_or(true) {
            best = Some((score, unescape_desktop(v)));
        }
    }
    best.map(|(_, v)| v)
}

/// Undo desktop-entry string escapes (`\s`, `\n`, `\t`, `\r`, `\\`).
fn unescape_desktop(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('s') => out.push(' '),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------- Exec line --

/// Tokenize a desktop `Exec=` value per the spec (double quotes group, `\`
/// escapes inside quotes).
pub fn exec_tokens(exec: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => in_quotes = !in_quotes,
            '\\' if in_quotes => {
                if let Some(n) = chars.next() {
                    match n {
                        '"' | '`' | '$' | '\\' => cur.push(n),
                        other => {
                            cur.push('\\');
                            cur.push(other);
                        }
                    }
                }
            }
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// Apply desktop-entry field codes to tokenized `Exec` arguments (tokens
/// after the program) in a single pass. `%f/%F/%u/%U` become
/// [`FILE_PLACEHOLDER`]; `%%` is a literal `%`; `%i` drops its token (icon
/// switch); `%c`/`%k` expand to the display name / desktop-file path; other
/// codes are dropped.
///
/// When no file placeholder survives, one is appended so the target file
/// still reaches the application.
pub fn apply_field_codes(
    args: &[String],
    name: &str,
    desktop_path: &Path,
) -> Vec<String> {
    let mut out = Vec::new();
    for a in args {
        if a == "%i" {
            continue;
        }
        let mut s = String::with_capacity(a.len());
        let mut chars = a.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '%' {
                s.push(c);
                continue;
            }
            match chars.next() {
                Some('%') => s.push('%'),
                Some('f') | Some('F') | Some('u') | Some('U') => {
                    s.push_str(FILE_PLACEHOLDER)
                }
                Some('c') => s.push_str(name),
                Some('k') => s.push_str(&desktop_path.to_string_lossy()),
                Some(_) => {} // unsupported code: drop
                None => {}
            }
        }
        if !s.is_empty() {
            out.push(s);
        }
    }
    if !out.iter().any(|a| a.contains(FILE_PLACEHOLDER)) {
        out.push(FILE_PLACEHOLDER.to_string());
    }
    out
}

// ------------------------------------------------- shared-mime-info globs --

/// Parse `globs` / `globs2` text into `ext -> mime`.
///
/// Accepts both one-glob-per-line (`text/html:*.html`) and semicolon lists
/// (`text/html:*.html;*.htm`), plus the optional globs2 `:weight` field.
/// Only simple `*.ext` globs are indexed; compound suffixes like `*.tar.gz`
/// are indexed under `tar.gz`.
pub fn parse_globs(text: &str) -> HashMap<String, String> {
    let mut out: HashMap<String, (String, u32)> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(3, ':');
        let (Some(mime), Some(globs), weight) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let weight: u32 = weight
            .and_then(|w| w.trim().parse().ok())
            .unwrap_or(50);
        let mime = mime.trim().to_lowercase();
        for glob in globs.split(';') {
            let glob = glob.trim();
            let Some(ext) = glob.strip_prefix("*.") else {
                continue;
            };
            if ext.is_empty() || ext.contains('*') {
                continue;
            }
            let ext = ext.to_lowercase();
            match out.get(&ext) {
                Some((_, w)) if *w >= weight => {}
                _ => {
                    out.insert(ext, (mime.clone(), weight));
                }
            }
        }
    }
    out.into_iter().map(|(k, (m, _))| (k, m)).collect()
}

/// Built-in extension -> MIME fallback for minimal systems without
/// `/usr/share/mime/globs2`.
pub fn builtin_ext_mime() -> HashMap<String, &'static str> {
    const TABLE: &[(&str, &str)] = &[
        ("html", "text/html"),
        ("htm", "text/html"),
        ("txt", "text/plain"),
        ("text", "text/plain"),
        ("log", "text/plain"),
        ("md", "text/markdown"),
        ("csv", "text/csv"),
        ("tsv", "text/tab-separated-values"),
        ("json", "application/json"),
        ("xml", "application/xml"),
        ("pdf", "application/pdf"),
        ("png", "image/png"),
        ("jpg", "image/jpeg"),
        ("jpeg", "image/jpeg"),
        ("gif", "image/gif"),
        ("webp", "image/webp"),
        ("svg", "image/svg+xml"),
        ("ico", "image/vnd.microsoft.icon"),
        ("bmp", "image/bmp"),
        ("css", "text/css"),
        ("js", "text/javascript"),
        ("mjs", "text/javascript"),
        ("rs", "text/rust"),
        ("py", "text/x-python"),
        ("sh", "application/x-shellscript"),
        ("yaml", "application/yaml"),
        ("yml", "application/yaml"),
        ("toml", "application/toml"),
        ("ini", "text/plain"),
        ("zip", "application/zip"),
        ("gz", "application/gzip"),
        ("tar", "application/x-tar"),
        ("xz", "application/x-xz"),
        ("7z", "application/x-7z-compressed"),
        ("rar", "application/vnd.rar"),
        ("mp3", "audio/mpeg"),
        ("flac", "audio/flac"),
        ("wav", "audio/x-wav"),
        ("ogg", "audio/ogg"),
        ("mp4", "video/mp4"),
        ("mkv", "video/x-matroska"),
        ("webm", "video/webm"),
        ("avi", "video/x-msvideo"),
        ("doc", "application/msword"),
        ("docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        ("xls", "application/vnd.ms-excel"),
        ("xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        ("ppt", "application/vnd.ms-powerpoint"),
        ("pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        ("odt", "application/vnd.oasis.opendocument.text"),
        ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
        ("epub", "application/epub+zip"),
    ];
    TABLE
        .iter()
        .map(|(e, m)| (e.to_string(), *m))
        .collect()
}

// ------------------------------------------------------- association files --

/// Parsed `mimeapps.list`: per-mime ordered desktop-file ids and removals.
#[derive(Debug, Default, Clone)]
pub struct MimeApps {
    /// Ordered ids per mime type (defaults first, then added).
    pub associations: HashMap<String, Vec<String>>,
    /// Ids explicitly removed.
    pub removed: HashMap<String, Vec<String>>,
}

/// Parse a `mimeapps.list` file (`[Default Applications]` and
/// `[Added Associations]` merge into [`MimeApps::associations`]).
pub fn parse_mimeapps(text: &str) -> MimeApps {
    let mut out = MimeApps::default();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            section = line.to_string();
            continue;
        }
        let Some((mime, ids)) = line.split_once('=') else {
            continue;
        };
        let mime = mime.trim().to_lowercase();
        let ids: Vec<String> = ids
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        match section.as_str() {
            "[Default Applications]" | "[Added Associations]" => {
                let list = out.associations.entry(mime).or_default();
                for id in ids {
                    if !list.contains(&id) {
                        list.push(id);
                    }
                }
            }
            "[Removed Associations]" => {
                out.removed.entry(mime).or_default().extend(ids);
            }
            _ => {}
        }
    }
    out
}

/// Merge `other` into `base` giving **`other` priority**: its association
/// ids come first, `base` ids follow if not already listed, and `other`'s
/// `[Removed Associations]` drop ids from `base`.
pub fn merge_mimeapps(base: &mut MimeApps, other: &MimeApps) {
    for (mime, ids) in &other.removed {
        if let Some(list) = base.associations.get_mut(mime) {
            list.retain(|id| !ids.contains(id));
        }
        base.removed
            .entry(mime.clone())
            .or_default()
            .extend(ids.iter().cloned());
    }
    for (mime, ids) in &other.associations {
        let base_list = base.associations.remove(mime).unwrap_or_default();
        let mut merged = ids.clone();
        for id in base_list {
            let removed = other
                .removed
                .get(mime)
                .map(|r| r.contains(&id))
                .unwrap_or(false);
            if !removed && !merged.contains(&id) {
                merged.push(id);
            }
        }
        base.associations.insert(mime.clone(), merged);
    }
}

/// Parse a `mimeinfo.cache` file (`[MIME Cache]` section):
/// `mime=app1.desktop;app2.desktop;`.
pub fn parse_mimeinfo_cache(text: &str) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    let mut in_cache = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_cache = line == "[MIME Cache]";
            continue;
        }
        if !in_cache {
            continue;
        }
        if let Some((mime, ids)) = line.split_once('=') {
            let mime = mime.trim().to_lowercase();
            let list = out.entry(mime).or_insert_with(Vec::new);
            for id in ids.split(';') {
                let id = id.trim();
                if !id.is_empty() && !list.iter().any(|x| x == id) {
                    list.push(id.to_string());
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------- tests --

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_desktop_entry() {
        let text = "\
[Desktop Entry]
Type=Application
Name=Text Editor
Name[zh_CN]=文本编辑器
GenericName=Editor
Comment=Edit text files
Exec=gedit %F
TryExec=gedit
Icon=org.gnome.gedit
MimeType=text/plain;text/html;
Terminal=false
NoDisplay=false

[Desktop Action new-window]
Name=New Window
Exec=gedit --new-window
";
        let e = parse_desktop_entry(text, "zh_CN.UTF-8");
        assert_eq!(e.name, "文本编辑器");
        assert_eq!(e.entry_type, "Application");
        assert_eq!(e.exec.as_deref(), Some("gedit %F"));
        assert_eq!(e.icon.as_deref(), Some("org.gnome.gedit"));
        assert_eq!(e.mime_types, vec!["text/plain", "text/html"]);
        assert!(!e.hidden && !e.no_display && !e.terminal);

        let e = parse_desktop_entry(text, "en_US");
        assert_eq!(e.name, "Text Editor");
    }

    #[test]
    fn desktop_entry_escapes() {
        let text = "[Desktop Entry]\nName=A\\sB\\nC\n";
        let e = parse_desktop_entry(text, "C");
        assert_eq!(e.name, "A B\nC");
    }

    #[test]
    fn exec_tokenizes_quotes() {
        assert_eq!(
            exec_tokens(r#"/usr/bin/app --flag "two words" %f"#),
            vec!["/usr/bin/app", "--flag", "two words", "%f"]
        );
    }

    #[test]
    fn field_codes_become_placeholder() {
        let args = vec!["--open".to_string(), "%F".to_string()];
        assert_eq!(
            apply_field_codes(&args, "App", Path::new("/x/app.desktop")),
            vec!["--open", FILE_PLACEHOLDER]
        );
        // %i drops; %% is a literal percent; missing placeholder gets one.
        let args = vec!["%i".to_string(), "--title=%c".to_string(), "100%%".to_string()];
        assert_eq!(
            apply_field_codes(&args, "App", Path::new("/x/app.desktop")),
            vec!["--title=App", "100%", FILE_PLACEHOLDER]
        );
    }

    #[test]
    fn globs_parse_both_shapes() {
        let text = "\
text/html:*.html
text/html:*.htm:40
application/x-tar:*.tar.gz:60
# comment
image/png:*.png
";
        let g = parse_globs(text);
        assert_eq!(g.get("html").map(String::as_str), Some("text/html"));
        assert_eq!(g.get("htm").map(String::as_str), Some("text/html"));
        assert_eq!(g.get("png").map(String::as_str), Some("image/png"));
        assert_eq!(
            g.get("tar.gz").map(String::as_str),
            Some("application/x-tar")
        );
    }

    #[test]
    fn globs_weight_wins() {
        let text = "text/x-a:*.aa:40\ntext/x-b:*.aa:60\n";
        let g = parse_globs(text);
        assert_eq!(g.get("aa").map(String::as_str), Some("text/x-b"));
    }

    #[test]
    fn mimeapps_parse_and_merge() {
        let user = parse_mimeapps(
            "[Default Applications]\ntext/html=org.gnome.Epiphany.desktop;\n\n\
             [Removed Associations]\ntext/html=bad.desktop;\n",
        );
        let sys = parse_mimeapps(
            "[Default Applications]\ntext/html=firefox.desktop;\n\n\
             [Added Associations]\ntext/html=code.desktop;firefox.desktop;\n",
        );
        let mut base = sys;
        merge_mimeapps(&mut base, &user);
        assert_eq!(
            base.associations.get("text/html").unwrap(),
            &vec![
                "org.gnome.Epiphany.desktop".to_string(),
                "firefox.desktop".to_string(),
                "code.desktop".to_string()
            ]
        );
    }

    #[test]
    fn mimeinfo_cache_parse() {
        let text = "[MIME Cache]\ntext/html=firefox.desktop;chromium.desktop;\ntext/plain=gedit.desktop;\n";
        let m = parse_mimeinfo_cache(text);
        assert_eq!(
            m.get("text/html").unwrap(),
            &vec!["firefox.desktop", "chromium.desktop"]
        );
        assert_eq!(m.get("text/plain").unwrap(), &vec!["gedit.desktop"]);
    }

    #[test]
    fn builtin_table_has_html() {
        assert_eq!(builtin_ext_mime().get("html").copied(), Some("text/html"));
    }
}
