//! macOS implementation: `.app` bundle scanning, `Info.plist` metadata,
//! `CFBundleDocumentTypes` file associations, `.icns` icons.

use std::path::{Path, PathBuf};

use crate::icon::icon_from_rgba;
use crate::model::{AppInfo, Icon, LaunchSpec, ScanSource};
use crate::Result;

/// Well-known directories that hold `.app` bundles.
fn app_search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [
        "/Applications",
        "/System/Applications",
        "/System/Applications/Utilities",
        "/Applications/Utilities",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Applications"));
    }
    dirs
}

/// Recursively collect `.app` bundles under `dir`, not descending into
/// bundles themselves.
fn find_bundles(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.extension().map(|x| x == "app") == Some(true) {
                out.push(p);
            } else {
                find_bundles(&p, out);
            }
        }
    }
}

// ------------------------------------------------------------ Info.plist --

struct BundleInfo {
    name: String,
    bundle_id: Option<String>,
    version: Option<String>,
    executable: Option<PathBuf>,
    icon_file: Option<String>,
    extensions: Vec<String>,
}

fn plist_str(dict: &plist::Dictionary, key: &str) -> Option<String> {
    dict.get(key)
        .and_then(|v| v.as_string())
        .map(|s| s.to_string())
}

/// Read the parts of `Info.plist` we care about, mapping
/// `LSItemContentTypes` through [`uti_extensions`].
fn parse_bundle_info(plist: &plist::Value, bundle_stem: &str) -> Option<BundleInfo> {
    let dict = plist.as_dictionary()?;

    let name = plist_str(dict, "CFBundleDisplayName")
        .or_else(|| plist_str(dict, "CFBundleName"))
        .unwrap_or_else(|| bundle_stem.to_string());
    if name.is_empty() {
        return None;
    }

    let executable = plist_str(dict, "CFBundleExecutable")
        .map(|exe| PathBuf::from("Contents/MacOS").join(exe));

    // Collect document-type extensions.
    let mut extensions = Vec::new();
    if let Some(types) = dict.get("CFBundleDocumentTypes").and_then(|v| v.as_array()) {
        for t in types {
            let Some(t) = t.as_dictionary() else { continue };
            if let Some(exts) = t.get("CFBundleTypeExtensions").and_then(|v| v.as_array()) {
                for e in exts {
                    if let Some(e) = e.as_string() {
                        let e = e.trim_start_matches('.').to_lowercase();
                        if !e.is_empty() {
                            extensions.push(e);
                        }
                    }
                }
            }
            if let Some(utis) = t.get("LSItemContentTypes").and_then(|v| v.as_array()) {
                for u in utis {
                    if let Some(u) = u.as_string() {
                        extensions.extend(uti_extensions(u).iter().map(|s| s.to_string()));
                    }
                }
            }
        }
    }
    extensions.sort();
    extensions.dedup();

    Some(BundleInfo {
        name,
        bundle_id: plist_str(dict, "CFBundleIdentifier"),
        version: plist_str(dict, "CFBundleShortVersionString")
            .or_else(|| plist_str(dict, "CFBundleVersion")),
        executable,
        icon_file: plist_str(dict, "CFBundleIconFile"),
        extensions,
    })
}

/// Map common UTIs to file extensions (system declarations live in the
/// Launch Services database; this table covers the usual suspects).
fn uti_extensions(uti: &str) -> &'static [&'static str] {
    match uti {
        "public.html" => &["html", "htm"],
        "public.xhtml" => &["xhtml"],
        "public.plain-text" => &["txt", "text", "log"],
        "public.utf8-plain-text" | "public.utf16-external-plain-text"
        | "public.utf16-plain-text" => &["txt"],
        "net.daringfireball.markdown" => &["md", "markdown"],
        "public.rtf" => &["rtf"],
        "com.adobe.pdf" | "public.pdf" => &["pdf"],
        "public.png" => &["png"],
        "public.jpeg" => &["jpg", "jpeg"],
        "public.gif" => &["gif"],
        "public.tiff" => &["tiff", "tif"],
        "com.microsoft.bmp" => &["bmp"],
        "public.svg-image" => &["svg"],
        "com.apple.icns" => &["icns"],
        "public.json" => &["json"],
        "public.xml" => &["xml"],
        "public.source-code" => &["c", "h", "cpp", "hpp", "rs", "py", "js", "ts", "java"],
        "public.shell-script" => &["sh", "bash", "zsh"],
        "public.tar-archive" => &["tar"],
        "public.zip-archive" => &["zip"],
        "org.gnu.gnu-zip-archive" | "public.gzip-archive" | "public.gzip" => &["gz"],
        "public.7-zip-archive" => &["7z"],
        "com.rarlab.rar-archive" => &["rar"],
        "public.mp3" => &["mp3"],
        "public.mpeg-4-audio" => &["m4a", "aac"],
        "com.microsoft.waveform-audio" | "public.aiff-audio" => &["wav", "aiff"],
        "public.audio" => &["mp3", "wav", "flac", "ogg"],
        "public.mpeg-4" => &["mp4", "m4v"],
        "com.apple.quicktime-movie" => &["mov"],
        "public.avi" => &["avi"],
        "public.movie" => &["mp4", "mkv", "mov", "avi", "webm"],
        "com.microsoft.word.doc" => &["doc"],
        "org.openxmlformats.wordprocessingml.document" => &["docx"],
        "com.microsoft.excel.xls" => &["xls"],
        "org.openxmlformats.spreadsheetml.sheet" => &["xlsx"],
        "com.microsoft.powerpoint.ppt" => &["ppt"],
        "org.openxmlformats.presentationml.presentation" => &["pptx"],
        "org.oasis-open.opendocument.text" => &["odt"],
        "org.oasis-open.opendocument.spreadsheet" => &["ods"],
        "org.idpf.epub-container" => &["epub"],
        "com.apple.iwork.pages.pages" => &["pages"],
        "com.apple.iwork.numbers.numbers" => &["numbers"],
        "com.apple.iwork.keynote.key" => &["key"],
        _ => &[],
    }
}

// ----------------------------------------------------------------- icons --

/// Decode the best icon from an `.icns` file: largest representation is
/// preferred; the crate's pipeline scales the result to ≤ 256 px PNG.
fn load_icns(path: &Path) -> Option<Icon> {
    let file = std::fs::File::open(path).ok()?;
    let family = icns::IconFamily::read(file).ok()?;
    let mut best: Option<(u32, icns::Image)> = None;
    for icon_type in family.available_icons() {
        let Ok(img) = family.get_icon_with_type(icon_type) else {
            continue;
        };
        let area = img.width() * img.height();
        if best.as_ref().map(|(a, _)| area > *a).unwrap_or(true) {
            best = Some((area, img));
        }
    }
    let (_, img) = best?;
    let rgba = img.convert_to(icns::PixelFormat::RGBA);
    let (w, h) = (rgba.width(), rgba.height());
    let mut buf = image::RgbaImage::new(w, h);
    buf.copy_from_slice(rgba.data());
    icon_from_rgba(buf).ok()
}

fn bundle_icon(bundle: &Path, info: &BundleInfo) -> Option<Icon> {
    let name = info.icon_file.as_deref()?;
    let resources = bundle.join("Contents/Resources");
    // CFBundleIconFile may omit the .icns suffix.
    for candidate in [
        resources.join(name),
        resources.join(format!("{name}.icns")),
    ] {
        if candidate.is_file() {
            return load_icns(&candidate);
        }
    }
    None
}

// ----------------------------------------------------------- app bundles --

struct BundleApp {
    app: AppInfo,
    extensions: Vec<String>,
}

fn collect_bundles() -> Vec<BundleApp> {
    let mut bundles = Vec::new();
    for dir in app_search_dirs() {
        find_bundles(&dir, &mut bundles);
    }
    let mut out = Vec::new();
    for bundle in bundles {
        let plist_path = bundle.join("Contents/Info.plist");
        let Ok(plist_val) = plist::Value::from_file(&plist_path) else {
            continue;
        };
        let stem = bundle
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let Some(info) = parse_bundle_info(&plist_val, &stem) else {
            continue;
        };
        // Skip hidden "agent" apps with no document types and no icon? Keep
        // everything visible; only skip empty names (done above).
        let executable = info
            .executable
            .as_ref()
            .map(|rel| bundle.join(rel))
            .filter(|p| p.is_file());
        let icon = bundle_icon(&bundle, &info);
        out.push(BundleApp {
            app: AppInfo {
                id: info
                    .bundle_id
                    .clone()
                    .unwrap_or_else(|| format!("bundle:{}", bundle.display())),
                name: info.name.clone(),
                install_path: bundle.clone(),
                executable,
                publisher: Some("Apple".to_string()).filter(|_| {
                    bundle.starts_with("/System") || info.bundle_id.as_deref().is_some_and(|id| id.starts_with("com.apple."))
                }),
                version: info.version.clone(),
                launch: LaunchSpec::MacOpen {
                    app_path: bundle.clone(),
                },
                icon,
                extensions: info.extensions.clone(),
                source: ScanSource::AppBundle,
            },
            extensions: info.extensions,
        });
    }
    out
}

// ----------------------------------------------------------- public entry --

pub(crate) fn scan_apps() -> Result<Vec<AppInfo>> {
    Ok(collect_bundles().into_iter().map(|b| b.app).collect())
}

pub(crate) fn apps_for_extension(ext: &str) -> Result<Vec<AppInfo>> {
    let ext = ext.to_lowercase();
    let bundles = collect_bundles();
    let mut out = Vec::new();
    for b in bundles {
        if b.extensions.iter().any(|e| e == &ext) {
            let mut app = b.app.clone();
            app.extensions = vec![ext.clone()];
            out.push(app);
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------- tests --

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uti_table_maps_html() {
        assert_eq!(uti_extensions("public.html"), &["html", "htm"]);
        assert_eq!(uti_extensions("com.unknown.thing"), &[] as &[&str]);
    }

    #[test]
    fn parses_bundle_info() {
        let plist_val = plist::Value::from_reader(std::io::Cursor::new(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Demo</string>
    <key>CFBundleIdentifier</key><string>com.example.demo</string>
    <key>CFBundleShortVersionString</key><string>2.1</string>
    <key>CFBundleExecutable</key><string>Demo</string>
    <key>CFBundleIconFile</key><string>DemoIcon</string>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeExtensions</key>
            <array><string>html</string><string>HTM</string></array>
            <key>LSItemContentTypes</key>
            <array><string>public.png</string></array>
        </dict>
    </array>
</dict>
</plist>"#,
        ))
        .unwrap();
        let info = parse_bundle_info(&plist_val, "Demo").unwrap();
        assert_eq!(info.name, "Demo");
        assert_eq!(info.bundle_id.as_deref(), Some("com.example.demo"));
        assert_eq!(info.version.as_deref(), Some("2.1"));
        assert_eq!(
            info.executable,
            Some(PathBuf::from("Contents/MacOS/Demo"))
        );
        assert_eq!(info.icon_file.as_deref(), Some("DemoIcon"));
        assert_eq!(info.extensions, vec!["htm", "html", "png"]);
    }
}
