//! Crate error types.

use std::path::PathBuf;

/// Errors produced by scanning or icon extraction.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An I/O failure while reading system sources (registry is not I/O; this
    /// covers desktop files, bundles, icon files, …).
    #[error("io error at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A system API (registry, shell, plist, …) failed.
    #[error("system error: {0}")]
    System(String),

    /// A scanned entry is malformed (bad desktop file, unparsable icon, …).
    #[error("parse error: {0}")]
    Parse(String),
}

impl Error {
    /// Used by the Windows registry/shell integration.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(crate) fn system(msg: impl Into<String>) -> Self {
        Self::System(msg.into())
    }
}

/// Crate-wide result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;
