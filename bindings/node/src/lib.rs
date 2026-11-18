//! Node.js / Bun bindings for `installed-apps` (napi-rs).
//!
//! Exposes three async functions (`scan`, `appsForExtension`, `findApp`) that
//! run on the libuv thread pool and never block the JS event loop.

// JS-facing object fields are camelCase (e.g. `installPath`); the napi macro
// generates matching temporaries, so allow the lint crate-wide.
#![allow(non_snake_case)]

use installed_apps::{
    AppInfo as RsAppInfo, Icon as RsIcon, LaunchSpec as RsLaunch, ScanSource, Scanner,
};
use napi::bindgen_prelude::*;
use napi_derive::napi;

// --------------------------------------------------------- JS-visible types --

/// Application icon: base64-encoded PNG (longest edge ≤ 256 px).
#[napi(object)]
#[allow(non_snake_case)]
pub struct JsIcon {
    /// Base64-encoded PNG bytes (no data-URI prefix).
    pub base64: String,
    pub width: u32,
    pub height: u32,
}

/// How an application opens a file.
#[napi(object)]
#[allow(non_snake_case)]
pub struct JsLaunchSpec {
    /// `"command"` (direct launch) or `"mac_open"` (Launch Services).
    pub kind: String,
    /// Present when `kind == "command"`: the executable to run.
    pub program: Option<String>,
    /// Present when `kind == "command"`: arguments containing the
    /// `"{file}"` placeholder for the target file path.
    pub args: Option<Vec<String>>,
    /// Present when `kind == "mac_open"`: the `.app` bundle path
    /// (used as `open -a <appPath> <file>`).
    pub appPath: Option<String>,
}

/// One installed application.
#[napi(object)]
#[allow(non_snake_case)]
pub struct JsAppInfo {
    /// Stable identifier (registry key / desktop-file id / bundle id).
    pub id: String,
    pub name: String,
    /// Install location: executable, `.app` bundle or `.desktop` file.
    pub installPath: String,
    pub executable: Option<String>,
    pub publisher: Option<String>,
    pub version: Option<String>,
    pub launch: JsLaunchSpec,
    pub icon: Option<JsIcon>,
    /// File extensions (no dot, lowercase) this app registers handling for.
    pub extensions: Vec<String>,
    /// `"uninstall_registry"` | `"app_paths"` | `"start_menu"` |
    /// `"desktop_entry"` | `"app_bundle"` | `"file_association"`.
    pub source: String,
}

impl From<RsLaunch> for JsLaunchSpec {
    fn from(l: RsLaunch) -> Self {
        match l {
            RsLaunch::Command { program, args } => JsLaunchSpec {
                kind: "command".into(),
                program: Some(program.to_string_lossy().into_owned()),
                args: Some(args),
                appPath: None,
            },
            RsLaunch::MacOpen { app_path } => JsLaunchSpec {
                kind: "mac_open".into(),
                program: None,
                args: None,
                appPath: Some(app_path.to_string_lossy().into_owned()),
            },
        }
    }
}

impl From<RsIcon> for JsIcon {
    fn from(i: RsIcon) -> Self {
        JsIcon {
            base64: i.base64,
            width: i.width,
            height: i.height,
        }
    }
}

fn source_str(s: ScanSource) -> &'static str {
    match s {
        ScanSource::UninstallRegistry => "uninstall_registry",
        ScanSource::AppPaths => "app_paths",
        ScanSource::StartMenu => "start_menu",
        ScanSource::DesktopEntry => "desktop_entry",
        ScanSource::AppBundle => "app_bundle",
        ScanSource::FileAssociation => "file_association",
    }
}

impl From<RsAppInfo> for JsAppInfo {
    fn from(a: RsAppInfo) -> Self {
        JsAppInfo {
            id: a.id,
            name: a.name,
            installPath: a.install_path.to_string_lossy().into_owned(),
            executable: a
                .executable
                .map(|p| p.to_string_lossy().into_owned()),
            publisher: a.publisher,
            version: a.version,
            launch: a.launch.into(),
            icon: a.icon.map(Into::into),
            extensions: a.extensions,
            source: source_str(a.source).to_string(),
        }
    }
}

fn map_err(e: installed_apps::Error) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

/// Run scanner work, converting library errors — and any stray panic — into
/// a JS error. A panic must never cross the N-API boundary: it would abort
/// the whole Node/Bun process.
fn guard<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> std::result::Result<T, installed_apps::Error> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(map_err(e)),
        Err(_) => Err(napi::Error::from_reason(
            "internal panic while scanning installed applications",
        )),
    }
}

// -------------------------------------------------------------- async tasks --

/// Full scan, computed off the event loop.
pub struct ScanTask;

impl Task for ScanTask {
    type Output = Vec<JsAppInfo>;
    type JsValue = Vec<JsAppInfo>;

    fn compute(&mut self) -> Result<Self::Output> {
        guard(|| Scanner::new().scan()).map(|apps| apps.into_iter().map(Into::into).collect())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// Apps that can open a given extension.
pub struct AssocTask {
    ext: String,
}

impl Task for AssocTask {
    type Output = Vec<JsAppInfo>;
    type JsValue = Vec<JsAppInfo>;

    fn compute(&mut self) -> Result<Self::Output> {
        let ext = self.ext.clone();
        guard(move || Scanner::new().apps_for_extension(&ext))
            .map(|apps| apps.into_iter().map(Into::into).collect())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// One application looked up by name.
pub struct FindTask {
    name: String,
}

impl Task for FindTask {
    type Output = Option<JsAppInfo>;
    type JsValue = Option<JsAppInfo>;

    fn compute(&mut self) -> Result<Self::Output> {
        let name = self.name.clone();
        guard(move || Scanner::new().find_app(&name)).map(|app| app.map(Into::into))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

// ------------------------------------------------------------------ exports --

/// Scan all installed applications.
#[napi(ts_return_type = "Promise<JsAppInfo[]>")]
pub fn scan() -> AsyncTask<ScanTask> {
    AsyncTask::new(ScanTask)
}

/// List applications registered to open `*.ext` (dot optional),
/// e.g. `appsForExtension("html")` returns browsers and editors.
#[napi(ts_return_type = "Promise<JsAppInfo[]>")]
pub fn apps_for_extension(ext: String) -> AsyncTask<AssocTask> {
    AsyncTask::new(AssocTask { ext })
}

/// Find one application by name (case-insensitive; exact match wins,
/// otherwise the first partial match). Returns `null` when nothing matches.
#[napi(ts_return_type = "Promise<JsAppInfo | null>")]
pub fn find_app(name: String) -> AsyncTask<FindTask> {
    AsyncTask::new(FindTask { name })
}
