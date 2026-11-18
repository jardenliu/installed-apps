//! Cross-platform installed-application scanner.
//!
//! Scans installed applications on **Windows**, **Linux** and **macOS**
//! (x64 and arm64) and answers two questions:
//!
//! * *which applications are installed?* — [`Scanner::scan`]
//! * *which applications can open `.html` (or any extension)?* —
//!   [`Scanner::apps_for_extension`]
//!
//! Each [`AppInfo`] carries at least a name, install path, a file-opening
//! recipe ([`LaunchSpec`]) and a base64-encoded PNG icon (≤ 256 px).
//!
//! ```no_run
//! use installed_apps::Scanner;
//!
//! let scanner = Scanner::new();
//! for app in scanner.apps_for_extension("html").unwrap() {
//!     println!("{} -> {}", app.name, app.launch.template());
//!     if let Some(icon) = &app.icon {
//!         println!("  icon: {}", icon.data_uri());
//!     }
//! }
//! ```

mod error;
#[cfg(any(target_os = "linux", test))]
mod freedesktop;
mod icon;
mod model;
mod platform;

pub use error::{Error, Result};
pub use model::{AppInfo, Icon, LaunchSpec, ScanSource, FILE_PLACEHOLDER};

/// Installed-application scanner.
///
/// The scanner is a cheap, stateless handle; every method performs its own
/// platform queries, so results always reflect the current system state.
#[derive(Debug, Default, Clone, Copy)]
pub struct Scanner;

impl Scanner {
    /// Create a scanner for the current platform.
    pub fn new() -> Self {
        Self
    }

    /// Scan all installed applications.
    ///
    /// Sources per platform:
    /// * **Windows** — uninstall registry (32- and 64-bit views), `App Paths`,
    ///   Start Menu shortcuts (UWP/Store apps are not included).
    /// * **Linux** — XDG `.desktop` entries (system + user + flatpak/snap
    ///   exports).
    /// * **macOS** — `.app` bundles under `/Applications`, `/System/Applications`
    ///   and `~/Applications`.
    ///
    /// Entries are deduplicated by install path; failures on individual
    /// entries are skipped rather than failing the whole scan.
    pub fn scan(&self) -> Result<Vec<AppInfo>> {
        let mut apps = platform::scan_apps()?;
        apps.sort_by_key(|a| a.name.to_lowercase());
        apps.dedup_by(|a, b| a.install_path == b.install_path);
        Ok(apps)
    }

    /// List applications registered to open files with the given extension
    /// (case-insensitive, with or without a leading dot).
    ///
    /// `html` returns browsers and editors, `pdf` returns PDF readers, and so
    /// on, following the platform's own file-association mechanism.
    pub fn apps_for_extension(&self, ext: &str) -> Result<Vec<AppInfo>> {
        let ext = ext.trim().trim_start_matches('.').to_lowercase();
        if ext.is_empty() {
            return Ok(Vec::new());
        }
        platform::apps_for_extension(&ext)
    }

    /// Find a single application by name (case-insensitive; exact match wins,
    /// otherwise the first name that contains `name`).
    ///
    /// The executable / install path may also be matched.
    pub fn find_app(&self, name: &str) -> Result<Option<AppInfo>> {
        let needle = name.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(None);
        }
        let apps = self.scan()?;
        let mut partial: Option<AppInfo> = None;
        for app in apps {
            let name_l = app.name.to_lowercase();
            if name_l == needle {
                return Ok(Some(app));
            }
            if partial.is_none()
                && (name_l.contains(&needle)
                    || app
                        .executable
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_lowercase().contains(&needle))
                        .unwrap_or(false)
                    || app
                        .install_path
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&needle))
            {
                partial = Some(app);
            }
        }
        Ok(partial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_normalisation() {
        let s = Scanner::new();
        // No panic / no error on odd input; empty ext short-circuits.
        assert!(s.apps_for_extension("").unwrap().is_empty());
        assert!(s.apps_for_extension(".").unwrap().is_empty());
    }

    #[test]
    fn find_app_empty_query() {
        assert!(Scanner::new().find_app("  ").unwrap().is_none());
    }
}
