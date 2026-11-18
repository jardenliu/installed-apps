//! Per-OS implementations, selected at compile time.

use crate::model::AppInfo;
use crate::Result;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as imp;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as imp;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
compile_error!("installed-apps supports Windows, Linux and macOS only");

pub(crate) fn scan_apps() -> Result<Vec<AppInfo>> {
    imp::scan_apps()
}

pub(crate) fn apps_for_extension(ext: &str) -> Result<Vec<AppInfo>> {
    imp::apps_for_extension(ext)
}
