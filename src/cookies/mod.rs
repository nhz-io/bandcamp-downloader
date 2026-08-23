mod chromium;
mod firefox;

use std::error::Error;
use std::fs::{copy, remove_file};
use std::path::{Path, PathBuf};
use std::process;

pub const COOKIE_NAME: &str = "identity";
pub const HOST_SUFFIX: &str = "bandcamp.com";

/// A Bandcamp login that the tool found in a browser
#[derive(Debug)]
pub struct FoundCookie {
    pub value: String,
    /// Microseconds after the unix epoch. Thus the tool can compare cookies from different browsers.
    pub last_access: i64,
    pub browser: String,
    pub profile: String,
}

impl FoundCookie {
    pub fn describe(&self) -> String {
        format!("{} ({})", self.browser, self.profile)
    }
}

/// Copy a cookie store before you read it. A browser keeps its store open. The write
/// ahead log must come with the store, or the most recent changes are absent.
pub(crate) fn copy_store(path: &Path, tag: &str) -> Result<PathBuf, Box<dyn Error>> {
    let copied = std::env::temp_dir().join(format!("bcdl-{}-{}.sqlite", tag, process::id()));

    copy(path, &copied)?;

    for suffix in ["-wal", "-shm"] {
        let alongside = PathBuf::from(format!("{}{}", path.display(), suffix));

        if alongside.exists() {
            let _ = copy(&alongside, PathBuf::from(format!("{}{}", copied.display(), suffix)));
        }
    }

    Ok(copied)
}

pub(crate) fn discard_store(copied: &Path) {
    let _ = remove_file(copied);

    for suffix in ["-wal", "-shm"] {
        let _ = remove_file(PathBuf::from(format!("{}{}", copied.display(), suffix)));
    }
}

/// All Bandcamp logins on this computer, the most recent one first.
///
/// The tool reads Firefox first, because Firefox keeps its values as plain text.
/// Chromium encrypts its values with a key in the operating system keychain. A request
/// for that key causes a permission dialog. Thus the tool reads Chromium only if
/// Firefox has no login.
pub fn find_all(browser: Option<&str>, profile: Option<&str>) -> Vec<FoundCookie> {
    let asked_for = |name: &str| match browser {
        Some(wanted) => name.to_lowercase().contains(&wanted.to_lowercase()),
        _ => true,
    };

    let mut found = Vec::new();

    if asked_for("firefox") {
        found.extend(firefox::find());
    }

    if found.is_empty() {
        found.extend(chromium::find(browser));
    }

    if let Some(wanted) = profile {
        found.retain(|found| found.profile.to_lowercase().contains(&wanted.to_lowercase()));
    }

    // The profile that you use has the most recent access time. An older profile can
    // contain a login that is not valid, or a different account.
    found.sort_by_key(|found| -found.last_access);

    found
}
