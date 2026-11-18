//! Public data model: application records, launch specs and icons.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// Placeholder that `LaunchSpec::Command::args` uses for the file to open.
pub const FILE_PLACEHOLDER: &str = "{file}";

/// How an application opens a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchSpec {
    /// Direct command line: run `program` with `args`, substituting every
    /// [`FILE_PLACEHOLDER`] occurrence with the target file path.
    Command {
        program: PathBuf,
        args: Vec<String>,
    },
    /// macOS Launch Services route: `open -a <app_path> <file>`.
    MacOpen { app_path: PathBuf },
}

impl LaunchSpec {
    /// Build a ready-to-run [`Command`] that opens `file` with this app.
    ///
    /// On macOS the [`LaunchSpec::Command`] variant runs its program directly,
    /// while [`LaunchSpec::MacOpen`] invokes `/usr/bin/open`.
    pub fn build_command(&self, file: &Path) -> Command {
        match self {
            LaunchSpec::Command { program, args } => {
                let mut cmd = Command::new(program);
                for arg in args {
                    cmd.arg(arg.replace(FILE_PLACEHOLDER, &file.to_string_lossy()));
                }
                cmd
            }
            LaunchSpec::MacOpen { app_path } => {
                let mut cmd = Command::new("open");
                cmd.arg("-a").arg(app_path).arg(file);
                cmd
            }
        }
    }

    /// Human-readable template, e.g. `C:\app.exe {file}` or `open -a /Applications/X.app {file}`.
    pub fn template(&self) -> String {
        match self {
            LaunchSpec::Command { program, args } => {
                let mut s = program.display().to_string();
                for a in args {
                    s.push(' ');
                    s.push_str(a);
                }
                s
            }
            LaunchSpec::MacOpen { app_path } => {
                format!("open -a {} {}", app_path.display(), FILE_PLACEHOLDER)
            }
        }
    }
}

/// An application icon, normalised to PNG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Icon {
    /// Base64-encoded PNG bytes (no data-URI prefix).
    pub base64: String,
    /// Width in pixels (≤ 256).
    pub width: u32,
    /// Height in pixels (≤ 256).
    pub height: u32,
}

impl Icon {
    /// The icon as a `data:` URI (`image/png;base64,…`).
    pub fn data_uri(&self) -> String {
        format!("data:image/png;base64,{}", self.base64)
    }
}

/// Where an [`AppInfo`] was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanSource {
    /// Windows uninstall registry (HKLM/HKCU …\Uninstall).
    UninstallRegistry,
    /// Windows `App Paths` registry.
    AppPaths,
    /// Windows Start Menu shortcut.
    StartMenu,
    /// Linux XDG `.desktop` entry.
    DesktopEntry,
    /// macOS `.app` bundle.
    AppBundle,
    /// Discovered purely through file-association registration (ProgId).
    FileAssociation,
}

/// One installed application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    /// Stable identifier: registry key path, desktop-file id or bundle id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Install location: executable path, `.app` bundle or `.desktop` file.
    pub install_path: PathBuf,
    /// Resolved main executable, when known and different from `install_path`.
    pub executable: Option<PathBuf>,
    /// Vendor / publisher, when known.
    pub publisher: Option<String>,
    /// Version string, when known.
    pub version: Option<String>,
    /// How to open a file with this app.
    pub launch: LaunchSpec,
    /// PNG icon (base64), when one could be extracted.
    pub icon: Option<Icon>,
    /// File extensions (without dot, lowercase) this app registers handling for.
    pub extensions: Vec<String>,
    /// Where the record was discovered.
    pub source: ScanSource,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_template_substitutes_file() {
        let spec = LaunchSpec::Command {
            program: PathBuf::from(r"C:\app\edit.exe"),
            args: vec!["--open".into(), FILE_PLACEHOLDER.into()],
        };
        assert_eq!(
            spec.template(),
            r"C:\app\edit.exe --open {file}"
        );
        let cmd = spec.build_command(Path::new(r"C:\docs\a.txt"));
        let dbg = format!("{cmd:?}");
        // Debug escapes backslashes on Windows, so match on the file name.
        assert!(dbg.contains("a.txt"), "got {dbg}");
    }

    #[test]
    fn mac_open_template_and_command() {
        let spec = LaunchSpec::MacOpen {
            app_path: PathBuf::from("/Applications/TextEdit.app"),
        };
        assert_eq!(
            spec.template(),
            "open -a /Applications/TextEdit.app {file}"
        );
        let cmd = spec.build_command(Path::new("/tmp/x.html"));
        let dbg = format!("{cmd:?}");
        assert!(dbg.contains("open"));
        assert!(dbg.contains("/Applications/TextEdit.app"));
        assert!(dbg.contains("/tmp/x.html"));
    }

    #[test]
    fn icon_data_uri() {
        let icon = Icon {
            base64: "AAAA".into(),
            width: 256,
            height: 128,
        };
        assert_eq!(icon.data_uri(), "data:image/png;base64,AAAA");
    }

    #[test]
    fn app_info_serde_roundtrip() {
        let app = AppInfo {
            id: "demo".into(),
            name: "Demo".into(),
            install_path: PathBuf::from("/opt/demo"),
            executable: None,
            publisher: Some("ACME".into()),
            version: Some("1.0".into()),
            launch: LaunchSpec::Command {
                program: PathBuf::from("/opt/demo/run"),
                args: vec![FILE_PLACEHOLDER.into()],
            },
            icon: None,
            extensions: vec!["html".into()],
            source: ScanSource::DesktopEntry,
        };
        let json = serde_json::to_string(&app).unwrap();
        let back: AppInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(app, back);
    }
}
