//! Windows implementation.
//!
//! Sources (UWP/Store apps are intentionally not scanned):
//! * uninstall registry (HKLM/HKCU, both 32- and 64-bit views),
//! * `App Paths` registry,
//! * Start Menu shortcuts,
//! * file associations (HKCU `FileExts` UserChoice + HKCR ProgIds).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFOHEADER,
    DIB_RGB_COLORS, HBITMAP, RGBQUAD,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CLASSES_ROOT,
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    REG_SAM_FLAGS, REG_VALUE_TYPE,
};
use windows::Win32::UI::Shell::{
    ExtractIconExW, SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

use crate::icon::icon_from_bgra;
use crate::model::{AppInfo, Icon, LaunchSpec, ScanSource, FILE_PLACEHOLDER};
use crate::Result;

// ---------------------------------------------------------------- registry --

fn wstr(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Thin safe wrapper over a registry key.
struct RegKey {
    h: HKEY,
}

impl RegKey {
    fn open(root: HKEY, path: &str, view: REG_SAM_FLAGS) -> Result<Self> {
        let mut h = HKEY::default();
        let status = unsafe {
            RegOpenKeyExW(
                root,
                PCWSTR(wstr(path).as_ptr()),
                None,
                KEY_READ | view,
                &mut h,
            )
        };
        status
            .ok()
            .map_err(|e| crate::Error::system(format!("RegOpenKeyExW {path}: {e}")))?;
        Ok(Self { h })
    }

    /// Read a REG_SZ / REG_EXPAND_SZ value (REG_EXPAND_SZ is expanded).
    fn value_sz(&self, name: Option<&str>) -> Option<String> {
        let wname: Vec<u16> = match name {
            Some(n) => wstr(n),
            None => Vec::new(), // default value
        };
        let name_ptr = if name.is_some() {
            PCWSTR(wname.as_ptr())
        } else {
            PCWSTR::null()
        };
        let mut ty = REG_VALUE_TYPE::default();
        let mut size = 0u32;
        let status = unsafe {
            RegQueryValueExW(
                self.h,
                name_ptr,
                None,
                Some(&mut ty),
                None,
                Some(&mut size),
            )
        };
        status.ok().ok()?;
        if size == 0 || !size.is_multiple_of(2) {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let mut size2 = size;
        let status = unsafe {
            RegQueryValueExW(
                self.h,
                name_ptr,
                None,
                Some(&mut ty),
                Some(buf.as_mut_ptr()),
                Some(&mut size2),
            )
        };
        status.ok().ok()?;
        // Decode UTF-16LE, trimming at the first NUL.
        let units: Vec<u16> = buf[..size2 as usize]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|u| *u != 0)
            .collect();
        let s = String::from_utf16_lossy(&units);
        let s = s.trim().to_string();
        if s.is_empty() {
            return None;
        }
        if ty == windows::Win32::System::Registry::REG_EXPAND_SZ {
            Some(expand_env(&s))
        } else {
            Some(s)
        }
    }

    fn value_u32(&self, name: &str) -> Option<u32> {
        let wname = wstr(name);
        let mut ty = REG_VALUE_TYPE::default();
        let mut val = 0u32;
        let mut size = 4u32;
        let status = unsafe {
            RegQueryValueExW(
                self.h,
                PCWSTR(wname.as_ptr()),
                None,
                Some(&mut ty),
                Some((&mut val as *mut u32).cast()),
                Some(&mut size),
            )
        };
        status.ok().ok()?;
        Some(val)
    }

    fn subkeys(&self) -> Vec<String> {
        let mut out = Vec::new();
        for idx in 0.. {
            let mut buf = [0u16; 256];
            let mut len = buf.len() as u32;
            let status = unsafe {
                RegEnumKeyExW(
                    self.h,
                    idx,
                    Some(PWSTR(buf.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if status.is_err() {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        out
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.h);
        }
    }
}

/// Expand `%VAR%` environment references.
fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// ------------------------------------------------------ command templates --

/// Split a Windows command line into (program, args), honouring quotes.
fn split_command_line(cmd: &str) -> (String, Vec<String>) {
    let s = cmd.trim();
    if s.is_empty() {
        return (String::new(), Vec::new());
    }
    let (program, rest) = if let Some(after) = s.strip_prefix('"') {
        match after.find('"') {
            Some(end) => (after[..end].to_string(), &after[end + 1..]),
            None => (after.to_string(), ""),
        }
    } else {
        match s.find(char::is_whitespace) {
            Some(end) => (s[..end].to_string(), &s[end..]),
            None => (s.to_string(), ""),
        }
    };
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let chars = rest.chars().peekable();
    for c in chars {
        match c {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        args.push(cur);
    }
    (program, args)
}

/// Convert raw command args into a [`LaunchSpec`] template: replace the
/// file placeholder (`%1`, `%L`, `%l`, `%*`) with [`FILE_PLACEHOLDER`]; when
/// the command has no placeholder at all, append one so the file still
/// reaches the application.
fn template_args(args: &[String]) -> Vec<String> {
    let mut out: Vec<String> = args
        .iter()
        .map(|a| {
            a.replace("%1", FILE_PLACEHOLDER)
                .replace("%L", FILE_PLACEHOLDER)
                .replace("%l", FILE_PLACEHOLDER)
                .replace("%*", FILE_PLACEHOLDER)
        })
        .collect();
    if !out.iter().any(|a| a.contains(FILE_PLACEHOLDER)) {
        out.push(FILE_PLACEHOLDER.to_string());
    }
    out
}

/// Parse a `DisplayIcon`-style `"C:\path\app.exe",5` reference.
fn parse_icon_ref(s: &str) -> (PathBuf, i32) {
    let s = expand_env(s.trim());
    let (path_str, index) = if let Some(after) = s.strip_prefix('"') {
        match after.find('"') {
            Some(end) => {
                let p = &after[..end];
                let rest = &after[end + 1..];
                let idx = rest
                    .trim_start_matches(',')
                    .trim()
                    .parse::<i32>()
                    .unwrap_or(0);
                (p.to_string(), idx)
            }
            None => (after.to_string(), 0),
        }
    } else {
        // Split on the last comma only when its tail parses as an index.
        match s.rsplit_once(',') {
            Some((p, tail)) => match tail.trim().parse::<i32>() {
                Ok(i) => (p.trim().to_string(), i),
                Err(_) => (s.clone(), 0),
            },
            None => (s.clone(), 0),
        }
    };
    (PathBuf::from(path_str), index)
}

// ------------------------------------------------------------------- icons --

/// Extract a PNG icon from an executable/ico/dll at `index`.
fn icon_for_file(path: &Path, index: i32) -> Option<Icon> {
    let wpath = wstr(&path.to_string_lossy());
    unsafe {
        // 1. Explicit icon resource at `index`.
        if index >= 0 {
            let mut large = HICON::default();
            let n = ExtractIconExW(PCWSTR(wpath.as_ptr()), index, Some(&mut large), None, 1);
            if n == 1 && !large.is_invalid() {
                let icon = hicon_to_png(large);
                let _ = DestroyIcon(large);
                if icon.is_some() {
                    return icon;
                }
            }
        }
        // 2. Shell fallback: the file's default icon.
        let mut shinfo = SHFILEINFOW::default();
        let ok = SHGetFileInfoW(
            PCWSTR(wpath.as_ptr()),
            Default::default(),
            Some(&mut shinfo),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if ok != 0 && !shinfo.hIcon.is_invalid() {
            let icon = hicon_to_png(shinfo.hIcon);
            let _ = DestroyIcon(shinfo.hIcon);
            return icon;
        }
    }
    None
}

/// Convert an HICON into the crate's PNG/base64 [`Icon`].
fn hicon_to_png(hicon: HICON) -> Option<Icon> {
    unsafe {
        let mut info = ICONINFO::default();
        if GetIconInfo(hicon, &mut info).is_err() {
            return None;
        }
        let result = read_icon_bitmaps(info);
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(info.hbmColor.into());
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(info.hbmMask.into());
        }
        result
    }
}

/// `BITMAPINFO` with room for a 2-entry colour table.
///
/// The `windows` crate's `BITMAPINFO` carries only `bmiColors: [RGBQUAD; 1]`,
/// but `GetDIBits` writes a full colour table (2 entries for 1bpp = 8 bytes)
/// into the struct — overflowing the stack if we pass the stock type.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BmiWithColors {
    header: BITMAPINFOHEADER,
    colors: [RGBQUAD; 2],
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn read_icon_bitmaps(info: ICONINFO) -> Option<Icon> {
    // Dimensions from the colour bitmap (or the mask for monochrome icons).
    let mut bmp = BITMAP::default();
    let src = if !info.hbmColor.is_invalid() {
        info.hbmColor
    } else if !info.hbmMask.is_invalid() {
        info.hbmMask
    } else {
        return None;
    };
    if GetObjectW(src.into(), std::mem::size_of::<BITMAP>() as i32, Some((&mut bmp as *mut BITMAP).cast()))
        == 0
    {
        return None;
    }
    let (w, h) = (bmp.bmWidth, bmp.bmHeight.abs());
    if w <= 0 || h <= 0 {
        return None;
    }

    // Read 32bpp BGRA pixels of a bitmap (top-down for sanity).
    let read_dib = |hbm: HBITMAP, width: u32, height: u32, bit_count: u16| -> Option<Vec<u8>> {
        let mut bmi = BmiWithColors {
            header: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: bit_count,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            colors: [RGBQUAD::default(); 2],
        };
        let bytes = match bit_count {
            1 => (width as usize).div_ceil(32) * 4 * height as usize,
            32 => width as usize * height as usize * 4,
            _ => return None,
        };
        let mut pixels = vec![0u8; bytes];
        let hdc = GetDC(None);
        let lines = GetDIBits(
            hdc,
            hbm,
            0,
            height,
            Some(pixels.as_mut_ptr().cast()),
            (&mut bmi as *mut BmiWithColors).cast(),
            DIB_RGB_COLORS,
        );
        let _ = ReleaseDC(None, hdc);
        if lines == 0 {
            None
        } else {
            Some(pixels)
        }
    };

    if !info.hbmColor.is_invalid() {
        let mut pixels = read_dib(info.hbmColor, w as u32, h as u32, 32)?;
        // Premultiplied -> straight alpha; recover alpha from the AND mask
        // when the colour bitmap carries none (older icons).
        let has_alpha = pixels.chunks_exact(4).any(|p| p[3] != 0);
        if !has_alpha {
            if !info.hbmMask.is_invalid() {
                apply_mask_alpha(&mut pixels, w as u32, h as u32, info.hbmMask);
            } else {
                for p in pixels.chunks_exact_mut(4) {
                    p[3] = 255;
                }
            }
        } else {
            for p in pixels.chunks_exact_mut(4) {
                let a = p[3] as u32;
                if a > 0 && a < 255 {
                    for c in &mut p[..3] {
                        *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                    }
                }
            }
        }
        return icon_from_bgra(&pixels, w as u32, h as u32, w as usize * 4).ok();
    }

    // Monochrome icon: the mask is a double-height bitmap (AND row above XOR
    // row); render the AND mask as a black silhouette.
    if !info.hbmMask.is_invalid() {
        let icon_h = (if h % 2 == 0 { h / 2 } else { h }) as u32;
        let mask = read_dib(info.hbmMask, w as u32, h as u32, 1)?;
        let stride = (w as usize).div_ceil(32) * 4;
        let mut pixels = vec![255u8; w as usize * icon_h as usize * 4];
        apply_mask_rows(&mut pixels, w as u32, icon_h, &mask, stride, 0);
        for p in pixels.chunks_exact_mut(4) {
            let a = p[3];
            p[0] = 0;
            p[1] = 0;
            p[2] = 0;
            p[3] = a;
        }
        return icon_from_bgra(&pixels, w as u32, icon_h, w as usize * 4).ok();
    }
    None
}

/// Copy a 1bpp AND mask into the alpha channel (1 = transparent).
///
/// `mask_stride` is the row stride of `mask` in bytes; `row_offset` selects
/// the first mask row to use (for double-height AND+XOR masks).
fn apply_mask_rows(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    mask: &[u8],
    mask_stride: usize,
    row_offset: usize,
) {
    for y in 0..h as usize {
        for x in 0..w as usize {
            let Some(&byte) = mask.get((y + row_offset) * mask_stride + x / 8) else {
                continue;
            };
            let bit = byte >> (7 - x % 8) & 1;
            if bit == 1 {
                let idx = (y * w as usize + x) * 4 + 3;
                if let Some(a) = pixels.get_mut(idx) {
                    *a = 0;
                }
            }
        }
    }
}

/// Read a 1bpp AND mask from `hbm_mask` and clear the covered alpha channel.
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn apply_mask_alpha(pixels: &mut [u8], w: u32, h: u32, hbm_mask: HBITMAP) {
    let stride = (w as usize).div_ceil(32) * 4;
    let mut mask = vec![0u8; stride * h as usize];
    let mut bmi = BmiWithColors {
        header: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 1,
            biCompression: 0,
            ..Default::default()
        },
        colors: [RGBQUAD::default(); 2],
    };
    let hdc = GetDC(None);
    let lines = GetDIBits(
        hdc,
        hbm_mask,
        0,
        h,
        Some(mask.as_mut_ptr().cast()),
        (&mut bmi as *mut BmiWithColors).cast(),
        DIB_RGB_COLORS,
    );
    let _ = ReleaseDC(None, hdc);
    if lines == 0 {
        return;
    }
    apply_mask_rows(pixels, w, h, &mask, stride, 0);
}

// ----------------------------------------------------------- app discovery --

/// Heuristic: these uninstall entries are maintenance packages, not apps.
fn is_system_entry(name: &str, release_type: Option<&str>) -> bool {
    if let Some(rt) = release_type {
        let rt = rt.to_lowercase();
        if matches!(
            rt.as_str(),
            "update" | "hotfix" | "service pack" | "security update"
        ) {
            return true;
        }
    }
    let n = name.to_lowercase();
    n.starts_with("update for ")
        || n.starts_with("security update for ")
        || n.starts_with("hotfix for ")
}

fn exe_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase().replace('/', "\\")
}

fn collect_uninstall_apps(apps: &mut Vec<AppInfo>) {
    let views = [KEY_WOW64_64KEY, KEY_WOW64_32KEY];
    let roots = [
        (HKEY_LOCAL_MACHINE, "HKLM"),
        (HKEY_CURRENT_USER, "HKCU"),
    ];
    let rel = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    for (root, hive) in roots {
        for view in views {
            let key = match RegKey::open(root, rel, view) {
                Ok(k) => k,
                Err(_) => continue,
            };
            for sub in key.subkeys() {
                let subkey = match RegKey::open(root, &format!(r"{rel}\{sub}"), view) {
                    Ok(k) => k,
                    Err(_) => continue,
                };
                let name = match subkey.value_sz(Some("DisplayName")) {
                    Some(n) => n,
                    None => continue,
                };
                if subkey.value_u32("SystemComponent") == Some(1) {
                    continue;
                }
                let release = subkey.value_sz(Some("ReleaseType"));
                if is_system_entry(&name, release.as_deref()) {
                    continue;
                }
                let display_icon = subkey.value_sz(Some("DisplayIcon"));
                let install_location = subkey.value_sz(Some("InstallLocation"));
                let uninstall = subkey.value_sz(Some("UninstallString"));

                // Resolve the application executable.
                let mut exe: Option<PathBuf> = None;
                if let Some(di) = &display_icon {
                    let (p, _) = parse_icon_ref(di);
                    if p.is_file() {
                        exe = Some(p);
                    }
                }
                if exe.is_none()
                    && let Some(loc) = &install_location {
                        let dir = PathBuf::from(loc);
                        if dir.is_dir() {
                            exe = find_exe_in_dir(&dir, &name);
                        }
                    }
                if exe.is_none()
                    && let Some(us) = &uninstall {
                        let (p, _) = split_command_line_path(us);
                        if p.is_file() {
                            let stem = p
                                .file_stem()
                                .map(|s| s.to_string_lossy().to_lowercase())
                                .unwrap_or_default();
                            if !stem.starts_with("unins")
                                && !stem.starts_with("uninstall")
                                && !stem.starts_with("msiexec")
                                && stem != "setup"
                            {
                                exe = Some(p);
                            }
                        }
                    }
                let Some(exe) = exe else { continue };

                let (_, icon_index) = display_icon
                    .as_deref()
                    .map(parse_icon_ref)
                    .unwrap_or((PathBuf::new(), 0));
                let install_path = exe.clone();
                apps.push(AppInfo {
                    id: format!("uninstall:{hive}\\{rel}\\{sub}"),
                    name,
                    install_path: install_path.clone(),
                    executable: Some(exe.clone()),
                    publisher: subkey.value_sz(Some("Publisher")),
                    version: subkey.value_sz(Some("DisplayVersion")),
                    launch: LaunchSpec::Command {
                        program: exe.clone(),
                        args: vec![FILE_PLACEHOLDER.to_string()],
                    },
                    icon: icon_for_file(&exe, icon_index),
                    extensions: Vec::new(),
                    source: ScanSource::UninstallRegistry,
                });
            }
        }
    }
}

/// Split a command line and return only its program path.
fn split_command_line_path(cmd: &str) -> (PathBuf, i32) {
    let (program, _args) = split_command_line(cmd);
    (PathBuf::from(expand_env(&program)), 0)
}

/// Find an exe inside `dir` whose stem resembles `name`, else any launcher exe.
fn find_exe_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let wanted: String = name
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    let mut fallback: Option<PathBuf> = None;
    let mut entries = std::fs::read_dir(dir).ok()?;
    while let Some(Ok(e)) = entries.next() {
        let p = e.path();
        if p.extension().map(|x| x.eq_ignore_ascii_case("exe")) != Some(true) {
            continue;
        }
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if stem.starts_with("unins") || stem == "setup" || stem == "install" {
            continue;
        }
        let norm: String = stem.chars().filter(|c| c.is_alphanumeric()).collect();
        if !wanted.is_empty() && (norm == wanted || wanted.contains(&norm) && norm.len() >= 4) {
            return Some(p);
        }
        if fallback.is_none() {
            fallback = Some(p);
        }
    }
    fallback
}

fn collect_app_paths(apps: &mut Vec<AppInfo>) {
    let rel = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths";
    for (root, hive) in [(HKEY_LOCAL_MACHINE, "HKLM"), (HKEY_CURRENT_USER, "HKCU")] {
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            let key = match RegKey::open(root, rel, view) {
                Ok(k) => k,
                Err(_) => continue,
            };
            for sub in key.subkeys() {
                let subkey = match RegKey::open(root, &format!(r"{rel}\{sub}"), view) {
                    Ok(k) => k,
                    Err(_) => continue,
                };
                let Some(path) = subkey.value_sz(None) else {
                    continue;
                };
                let exe = PathBuf::from(path);
                if !exe.is_file() {
                    continue;
                }
                let name = exe
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| sub.clone());
                apps.push(AppInfo {
                    id: format!("app-paths:{hive}:{sub}"),
                    name,
                    install_path: exe.clone(),
                    executable: Some(exe.clone()),
                    publisher: None,
                    version: None,
                    launch: LaunchSpec::Command {
                        program: exe.clone(),
                        args: vec![FILE_PLACEHOLDER.to_string()],
                    },
                    icon: icon_for_file(&exe, 0),
                    extensions: Vec::new(),
                    source: ScanSource::AppPaths,
                });
            }
        }
    }
}

fn collect_start_menu_apps(apps: &mut Vec<AppInfo>) {
    let mut roots = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        roots.push(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(progdata) = std::env::var("PROGRAMDATA") {
        roots.push(
            PathBuf::from(progdata).join(r"Microsoft\Windows\Start Menu\Programs"),
        );
    }
    let mut seen = std::collections::HashSet::new();
    for root in &roots {
        walk_shortcuts(root, &mut |lnk| {
            let Ok(link) = lnk::ShellLink::open(lnk, encoding_rs::UTF_8) else {
                return;
            };
            let Some(target) = link.link_target() else {
                return;
            };
            let target = PathBuf::from(target);
            if target.extension().map(|e| e.eq_ignore_ascii_case("exe")) != Some(true) {
                return; // only launcher executables
            }
            if !target.is_file() {
                return;
            }
            let stem = lnk
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let low = stem.to_lowercase();
            if low.contains("unins") || low.contains("uninstall") {
                return;
            }
            if !seen.insert(exe_key(&target)) {
                return;
            }
            let icon_loc = link.string_data().icon_location().clone();
            let icon = match icon_loc.as_deref().map(parse_icon_ref) {
                Some((p, idx)) if p.is_file() => icon_for_file(&p, idx),
                _ => icon_for_file(&target, 0),
            };
            apps.push(AppInfo {
                id: format!("start-menu:{}", lnk.display()),
                name: stem,
                install_path: target.clone(),
                executable: Some(target.clone()),
                publisher: None,
                version: None,
                launch: LaunchSpec::Command {
                    program: target,
                    args: vec![FILE_PLACEHOLDER.to_string()],
                },
                icon,
                extensions: Vec::new(),
                source: ScanSource::StartMenu,
            });
        });
    }
}

fn walk_shortcuts(dir: &Path, cb: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_shortcuts(&p, cb);
        } else if p.extension().map(|x| x.eq_ignore_ascii_case("lnk")) == Some(true) {
            cb(&p);
        }
    }
}

// ------------------------------------------------------- file associations --

struct AssocEntry {
    progid: String,
    program: PathBuf,
    launch: LaunchSpec,
    icon: Option<Icon>,
}

struct AssocIndex {
    /// ext -> association entries, user's default (UserChoice) first.
    by_ext: HashMap<String, Vec<AssocEntry>>,
    /// exe path (normalised) -> extensions it claims.
    by_exe: HashMap<String, Vec<String>>,
}

/// Read HKCR `<ext>\OpenWithProgids` names plus the default ProgId.
fn ext_progids(ext: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let ext_path = format!(".{ext}");
    // HKCU FileExts UserChoice = the user's default handler.
    let fc = format!(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\FileExts\{ext_path}\UserChoice"
    );
    if let Ok(k) = RegKey::open(HKEY_CURRENT_USER, &fc, KEY_WOW64_64KEY)
        && let Some(p) = k.value_sz(Some("ProgId"))
            && !p.is_empty() {
                ids.push(p);
            }
    // HKCR\.ext default value and OpenWithProgids.
    if let Ok(k) = RegKey::open(HKEY_CLASSES_ROOT, &ext_path, KEY_WOW64_64KEY) {
        if let Some(p) = k.value_sz(None)
            && !p.is_empty() && !ids.contains(&p) {
                ids.push(p);
            }
        if let Ok(open_with) = RegKey::open(HKEY_CLASSES_ROOT, &format!(r"{ext_path}\OpenWithProgids"), KEY_WOW64_64KEY) {
            for id in open_with.subkeys() {
                if !id.is_empty() && !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

/// Resolve a ProgId to its `shell\open\command` template and icon.
fn progid_assoc(progid: &str) -> Option<(LaunchSpec, PathBuf, Option<Icon>)> {
    let key = RegKey::open(
        HKEY_CLASSES_ROOT,
        &format!(r"{progid}\shell\open\command"),
        KEY_WOW64_64KEY,
    )
    .ok()?;
    let cmd = key.value_sz(None)?;
    let (program_str, raw_args) = split_command_line(&cmd);
    let program = PathBuf::from(expand_env(&program_str));
    let launch = LaunchSpec::Command {
        program: program.clone(),
        args: template_args(&raw_args),
    };
    let icon = icon_for_file(&program, 0).or_else(|| {
        let di = RegKey::open(
            HKEY_CLASSES_ROOT,
            &format!(r"{progid}\DefaultIcon"),
            KEY_WOW64_64KEY,
        )
        .ok()?;
        let ref_ = di.value_sz(None)?;
        let (p, idx) = parse_icon_ref(&ref_);
        icon_for_file(&p, idx)
    });
    Some((launch, program, icon))
}

fn association_index() -> AssocIndex {
    // Common extensions worth indexing even when no HKCR key exists.
    let mut by_ext: HashMap<String, Vec<AssocEntry>> = HashMap::new();
    let mut by_exe: HashMap<String, Vec<String>> = HashMap::new();

    // Enumerate every ".ext" key under HKCR.
    let hkcr = match RegKey::open(HKEY_CLASSES_ROOT, "", KEY_WOW64_64KEY) {
        Ok(k) => k,
        Err(_) => return AssocIndex { by_ext, by_exe },
    };
    for sub in hkcr.subkeys() {
        if !sub.starts_with('.') {
            continue;
        }
        let ext = sub[1..].to_lowercase();
        if ext.is_empty() {
            continue;
        }
        let ids = ext_progids(&ext);
        let mut entries = Vec::new();
        for progid in ids {
            if let Some((launch, program, icon)) = progid_assoc(&progid) {
                by_exe
                    .entry(exe_key(&program))
                    .or_default()
                    .push(ext.clone());
                entries.push(AssocEntry {
                    progid,
                    program,
                    launch,
                    icon,
                });
            }
        }
        if !entries.is_empty() {
            by_ext.insert(ext, entries);
        }
    }
    AssocIndex { by_ext, by_exe }
}

// ----------------------------------------------------------- public entry --

pub(crate) fn scan_apps() -> Result<Vec<AppInfo>> {
    let mut apps = Vec::new();
    collect_uninstall_apps(&mut apps);
    collect_app_paths(&mut apps);
    collect_start_menu_apps(&mut apps);
    let assoc = association_index();
    for app in &mut apps {
        let key = app
            .executable
            .as_deref()
            .or(Some(app.install_path.as_path()))
            .map(exe_key)
            .unwrap_or_default();
        if let Some(exts) = assoc.by_exe.get(&key) {
            let mut e = exts.clone();
            e.sort();
            e.dedup();
            app.extensions = e;
        }
    }
    Ok(apps)
}

pub(crate) fn apps_for_extension(ext: &str) -> Result<Vec<AppInfo>> {
    let ext = ext.to_lowercase();
    let assoc = association_index();
    let Some(entries) = assoc.by_ext.get(&ext) else {
        return Ok(Vec::new());
    };
    let apps = scan_apps()?;
    let mut out = Vec::new();
    let mut used = std::collections::HashSet::new();
    for entry in entries {
        // Attach the association's real launch template to a known app.
        let matched = apps.iter().find(|a| {
            a.executable
                .as_deref()
                .map(exe_key)
                .map(|k| k == exe_key(&entry.program))
                .unwrap_or(false)
                || exe_key(&a.install_path) == exe_key(&entry.program)
        });
        match matched {
            Some(app) => {
                if used.insert(app.id.clone()) {
                    let mut app = app.clone();
                    app.launch = entry.launch.clone();
                    out.push(app);
                }
            }
            None => {
                if used.insert(entry.progid.clone()) {
                    out.push(AppInfo {
                        id: format!("progid:{}", entry.progid),
                        name: entry
                            .program
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| entry.progid.clone()),
                        install_path: entry.program.clone(),
                        executable: Some(entry.program.clone()),
                        publisher: None,
                        version: None,
                        launch: entry.launch.clone(),
                        icon: entry.icon.clone(),
                        extensions: vec![ext.clone()],
                        source: ScanSource::FileAssociation,
                    });
                }
            }
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------- tests --

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_quoted_program() {
        let (p, args) = split_command_line(r#""C:\Program Files\App\app.exe" --flag "%1""#);
        assert_eq!(p, r"C:\Program Files\App\app.exe");
        assert_eq!(args, vec!["--flag", "%1"]);
    }

    #[test]
    fn split_unquoted_program() {
        let (p, args) = split_command_line(r"C:\tools\ed.exe -n %1");
        assert_eq!(p, r"C:\tools\ed.exe");
        assert_eq!(args, vec!["-n", "%1"]);
    }

    #[test]
    fn template_replaces_placeholders() {
        let args = vec!["--single-argument".to_string(), "%1".to_string()];
        assert_eq!(
            template_args(&args),
            vec!["--single-argument", FILE_PLACEHOLDER]
        );
        // No placeholder -> append one.
        let args = vec!["-x".to_string()];
        assert_eq!(template_args(&args), vec!["-x", FILE_PLACEHOLDER]);
    }

    #[test]
    fn parse_display_icon_forms() {
        let (p, i) = parse_icon_ref(r#""C:\Program Files\App\app.exe",3"#);
        assert_eq!(p, PathBuf::from(r"C:\Program Files\App\app.exe"));
        assert_eq!(i, 3);
        let (p, i) = parse_icon_ref(r"C:\app\app.exe");
        assert_eq!(p, PathBuf::from(r"C:\app\app.exe"));
        assert_eq!(i, 0);
        let (p, i) = parse_icon_ref(r#""C:\my, app\app.exe", -1"#);
        assert_eq!(p, PathBuf::from(r"C:\my, app\app.exe"));
        assert_eq!(i, -1);
    }

    #[test]
    fn expand_env_vars() {
        // SAFETY: test-only env mutation; no other threads read this var.
        unsafe { std::env::set_var("IA_TEST_VAR", "xyz") };
        assert_eq!(expand_env(r"%IA_TEST_VAR%\sub"), r"xyz\sub");
        assert_eq!(expand_env(r"%NOT_A_VAR%\sub"), r"%NOT_A_VAR%\sub");
        assert_eq!(expand_env("plain"), "plain");
    }

    #[test]
    fn apply_mask_rows_clears_alpha() {
        // 2x2 RGBA pixels; 1bpp mask (stride 4): top row = 0b11… (both
        // transparent), bottom row empty (both opaque).
        let mut pixels = vec![255u8; 2 * 2 * 4];
        let mask = [0xC0u8, 0, 0, 0];
        apply_mask_rows(&mut pixels, 2, 2, &mask, 4, 0);
        assert_eq!(pixels[3], 0, "(0,0) must be transparent");
        assert_eq!(pixels[7], 0, "(1,0) must be transparent");
        assert_eq!(pixels[11], 255, "(0,1) must stay opaque");
        assert_eq!(pixels[15], 255, "(1,1) must stay opaque");
    }

    #[test]
    fn apply_mask_rows_out_of_range_mask_is_safe() {
        // Mask shorter than the loop expects: must not panic.
        let mut pixels = vec![255u8; 4 * 4 * 4];
        let mask = [0xFFu8; 2];
        apply_mask_rows(&mut pixels, 4, 4, &mask, 1, 3);
    }
}
