//! Linux implementation: XDG `.desktop` entries, shared-mime-info globs,
//! `mimeapps.list` / `mimeinfo.cache` associations, icon theme lookup.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::freedesktop::{
    apply_field_codes, builtin_ext_mime, exec_tokens, merge_mimeapps, parse_desktop_entry,
    parse_globs, parse_mimeapps, parse_mimeinfo_cache, DesktopEntry, MimeApps,
};
use crate::icon::{icon_from_dynamic, icon_from_rgba};
use crate::model::{AppInfo, Icon, LaunchSpec, ScanSource};
use crate::Result;

// -------------------------------------------------------------- XDG paths --

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn xdg_data_home() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home_dir()
            .map(|h| h.join(".local/share"))
            .unwrap_or_else(|| PathBuf::from("/usr/local/share")),
    }
}

fn xdg_data_dirs() -> Vec<PathBuf> {
    match std::env::var_os("XDG_DATA_DIRS") {
        Some(v) if !v.is_empty() => std::env::split_paths(&v).collect(),
        _ => vec![
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
        ],
    }
}

fn xdg_config_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => dirs.push(PathBuf::from(v)),
        _ => {
            if let Some(h) = home_dir() {
                dirs.push(h.join(".config"));
            }
        }
    }
    match std::env::var_os("XDG_CONFIG_DIRS") {
        Some(v) if !v.is_empty() => dirs.extend(std::env::split_paths(&v)),
        _ => dirs.push(PathBuf::from("/etc/xdg")),
    }
    dirs
}

/// All directories that may contain `.desktop` application entries.
fn applications_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![xdg_data_home().join("applications")];
    for d in xdg_data_dirs() {
        dirs.push(d.join("applications"));
    }
    if let Some(h) = home_dir() {
        dirs.push(h.join(".local/share/flatpak/exports/share/applications"));
    }
    dirs.push(PathBuf::from(
        "/var/lib/flatpak/exports/share/applications",
    ));
    dirs.push(PathBuf::from("/var/lib/snapd/desktop/applications"));
    let mut seen = HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

fn current_locale() -> String {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() && v != "C" && v != "POSIX" {
                return v;
            }
        }
    }
    String::new()
}

// ---------------------------------------------------------- desktop scan --

struct DesktopApp {
    id: String,
    entry: DesktopEntry,
    app: AppInfo,
}

fn walk_desktop_files(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_desktop_files(&p, base, out);
        } else if p.extension().map(|x| x == "desktop") == Some(true) {
            let rel = p.strip_prefix(base).unwrap_or(&p);
            let id = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("-");
            out.push((id, p));
        }
    }
}

/// Find an executable on $PATH (or as a direct path).
fn resolve_program(prog: &str) -> Option<PathBuf> {
    if prog.contains('/') {
        let p = PathBuf::from(prog);
        return p.is_file().then_some(p);
    }
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let p = dir.join(prog);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn collect_all() -> Vec<DesktopApp> {
    let locale = current_locale();
    let mut files = Vec::new();
    for dir in applications_dirs() {
        walk_desktop_files(&dir, &dir, &mut files);
    }

    let ext_mime = load_ext_mime();
    let mut mime_exts: HashMap<String, Vec<String>> = HashMap::new();
    for (ext, mime) in &ext_mime {
        mime_exts.entry(mime.clone()).or_default().push(ext.clone());
    }

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (id, path) in files {
        if !seen.insert(id.clone()) {
            continue; // earlier XDG dir wins
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let entry = parse_desktop_entry(&text, &locale);
        if entry.entry_type != "Application" || entry.hidden || entry.name.is_empty() {
            continue;
        }
        let Some(exec) = entry.exec.as_deref() else {
            continue;
        };
        // TryExec: skip when the check binary is missing.
        if let Some(try_exec) = entry.try_exec.as_deref() {
            if resolve_program(try_exec).is_none() {
                continue;
            }
        }
        let tokens = exec_tokens(exec);
        let (Some(program_tok), args) = (tokens.first().cloned(), &tokens[1..]) else {
            continue;
        };
        let program = resolve_program(&program_tok)
            .unwrap_or_else(|| PathBuf::from(&program_tok));
        let launch = LaunchSpec::Command {
            program: program.clone(),
            args: apply_field_codes(args, &entry.name, &path),
        };
        let icon = entry.icon.as_deref().and_then(load_icon);
        let mut extensions = Vec::new();
        for mime in &entry.mime_types {
            if let Some(exts) = mime_exts.get(mime) {
                extensions.extend(exts.iter().cloned());
            }
        }
        extensions.sort();
        extensions.dedup();
        out.push(DesktopApp {
            id: id.clone(),
            entry,
            app: AppInfo {
                id,
                name: String::new(), // filled below
                install_path: path,
                executable: Some(program),
                publisher: None,
                version: None,
                launch,
                icon,
                extensions,
                source: ScanSource::DesktopEntry,
            },
        });
    }
    for d in &mut out {
        d.app.name = d.entry.name.clone();
    }
    out
}

// ------------------------------------------------------ mime & associations --

/// ext -> mime: `/usr/share/mime/globs2` when present, else the built-in table.
fn load_ext_mime() -> HashMap<String, String> {
    for dir in xdg_data_dirs() {
        let p = dir.join("mime/globs2");
        if let Ok(text) = std::fs::read_to_string(&p) {
            let mut g = parse_globs(&text);
            // Fill gaps from the built-in table.
            for (e, m) in builtin_ext_mime() {
                g.entry(e).or_insert(m.to_string());
            }
            return g;
        }
    }
    builtin_ext_mime()
        .into_iter()
        .map(|(e, m)| (e, m.to_string()))
        .collect()
}

/// Merge association sources into one ordered map.
/// Priority (high -> low): user `mimeapps.list`, system `mimeapps.list`
/// dirs in listed order, then `mimeinfo.cache` backfill.
fn load_mime_assoc() -> MimeApps {
    let mut merged = MimeApps::default();

    // Lowest priority: mimeinfo.cache from every applications dir.
    let mut cache_ids: HashMap<String, Vec<String>> = HashMap::new();
    for dir in applications_dirs() {
        let p = dir.join("mimeinfo.cache");
        if let Ok(text) = std::fs::read_to_string(&p) {
            for (mime, ids) in parse_mimeinfo_cache(&text) {
                let list = cache_ids.entry(mime).or_default();
                for id in ids {
                    if !list.contains(&id) {
                        list.push(id);
                    }
                }
            }
        }
    }
    for (mime, ids) in cache_ids {
        merged.associations.insert(mime, ids);
    }

    // System mimeapps.list dirs (everything past the user config dir),
    // folded so earlier dirs win over later ones.
    let dirs = xdg_config_dirs();
    for dir in dirs.iter().skip(1).rev() {
        let p = dir.join("mimeapps.list");
        if let Ok(text) = std::fs::read_to_string(&p) {
            let parsed = parse_mimeapps(&text);
            merge_mimeapps(&mut merged, &parsed);
        }
    }
    // Highest priority: the user's own mimeapps.list.
    if let Some(dir) = dirs.first() {
        let p = dir.join("mimeapps.list");
        if let Ok(text) = std::fs::read_to_string(&p) {
            let parsed = parse_mimeapps(&text);
            merge_mimeapps(&mut merged, &parsed);
        }
    }
    merged
}

// ------------------------------------------------------------------ icons --

/// Locate an icon file for an `Icon=` key (name or absolute path).
fn find_icon_file(icon: &str) -> Option<PathBuf> {
    if icon.contains('/') {
        let p = PathBuf::from(icon);
        return p.is_file().then_some(p);
    }
    let mut icon_dirs = vec![xdg_data_home().join("icons")];
    for d in xdg_data_dirs() {
        icon_dirs.push(d.join("icons"));
    }
    icon_dirs.push(PathBuf::from("/usr/share/pixmaps"));

    const THEMES: &[&str] = &["hicolor", "Adwaita", "Yaru", "breeze", "Papirus"];
    const SIZES: &[&str] = &[
        "256x256", "512x512", "128x128", "96x96", "64x64", "48x48", "32x32", "24x24", "22x22",
        "16x16",
    ];
    for dir in &icon_dirs {
        for theme in THEMES {
            for size in SIZES {
                for ext in ["png", "svg"] {
                    let p = dir
                        .join(theme)
                        .join(size)
                        .join("apps")
                        .join(format!("{icon}.{ext}"));
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
            // Scalable (vector) variant.
            let p = dir
                .join(theme)
                .join("scalable/apps")
                .join(format!("{icon}.svg"));
            if p.is_file() {
                return Some(p);
            }
        }
        // pixmaps have no theme/size hierarchy
        for ext in ["png", "svg"] {
            let p = dir.join(format!("{icon}.{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Load and normalise an icon (PNG/JPEG/ICO directly, SVG rasterised).
fn load_icon(icon: &str) -> Option<Icon> {
    let path = find_icon_file(icon)?;
    match path.extension().and_then(|e| e.to_str()) {
        Some("svg") => {
            let data = std::fs::read(&path).ok()?;
            let img = rasterize_svg(&data).ok()?;
            icon_from_rgba(img).ok()
        }
        _ => {
            let img = image::open(&path).ok()?;
            icon_from_dynamic(img).ok()
        }
    }
}

/// Rasterise SVG bytes to an RGBA image with longest edge ≤ 256 px.
fn rasterize_svg(data: &[u8]) -> std::result::Result<image::RgbaImage, String> {
    use resvg::tiny_skia;

    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(data, &opt).map_err(|e| format!("svg parse: {e}"))?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if !(w > 0.0 && h > 0.0) {
        return Err("svg has no size".into());
    }
    const MAX: f32 = crate::icon::MAX_ICON_EDGE as f32;
    let scale = MAX / w.max(h);
    let tw = (w * scale).round().max(1.0) as u32;
    let th = (h * scale).round().max(1.0) as u32;
    let mut pixmap =
        tiny_skia::Pixmap::new(tw, th).ok_or_else(|| "svg pixmap alloc".to_string())?;
    let transform = tiny_skia::Transform::from_scale(tw as f32 / w, th as f32 / h);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // Pixmap stores premultiplied RGBA; un-premultiply for PNG output.
    let mut img = image::RgbaImage::new(tw, th);
    for (dst, src) in img.pixels_mut().zip(pixmap.data().chunks_exact(4)) {
        let a = src[3] as u32;
        if a == 0 {
            *dst = image::Rgba([0, 0, 0, 0]);
        } else if a == 255 {
            *dst = image::Rgba([src[0], src[1], src[2], 255]);
        } else {
            let un = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255) as u8;
            *dst = image::Rgba([un(src[0]), un(src[1]), un(src[2]), a as u8]);
        }
    }
    Ok(img)
}

// ----------------------------------------------------------- public entry --

pub(crate) fn scan_apps() -> Result<Vec<AppInfo>> {
    Ok(collect_all().into_iter().map(|d| d.app).collect())
}

pub(crate) fn apps_for_extension(ext: &str) -> Result<Vec<AppInfo>> {
    let ext_mime = load_ext_mime();
    let Some(mime) = ext_mime.get(ext) else {
        return Ok(Vec::new());
    };
    let assoc = load_mime_assoc();
    let all = collect_all();
    let by_id: HashMap<&str, &DesktopApp> =
        all.iter().map(|d| (d.id.as_str(), d)).collect();

    let mut out = Vec::new();
    let mut used = HashSet::new();
    if let Some(ids) = assoc.associations.get(mime) {
        for id in ids {
            if assoc
                .removed
                .get(mime)
                .map(|r| r.contains(id))
                .unwrap_or(false)
            {
                continue;
            }
            // desktop ids may carry a "kde4-" style prefix; match suffix too.
            let hit = by_id.get(id.as_str()).copied().or_else(|| {
                all.iter()
                    .find(|d| d.id.ends_with(id.as_str()) || id.ends_with(d.id.as_str()))
            });
            if let Some(d) = hit {
                if used.insert(d.id.clone()) {
                    let mut app = d.app.clone();
                    app.extensions = vec![ext.to_string()];
                    out.push(app);
                }
            }
        }
    }
    // Fallback: entries advertising the MIME type directly.
    if out.is_empty() {
        for d in &all {
            if d.entry.mime_types.iter().any(|m| m == mime) && used.insert(d.id.clone()) {
                let mut app = d.app.clone();
                app.extensions = vec![ext.to_string()];
                out.push(app);
            }
        }
    }
    Ok(out)
}
