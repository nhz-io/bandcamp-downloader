use super::{copy_store, discard_store, FoundCookie, COOKIE_NAME, HOST_SUFFIX};
use rusqlite::Connection;
use std::env;
use std::fs::read_dir;
use std::path::PathBuf;

fn profile_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(home) = env::var("HOME") {
        let home = PathBuf::from(home);

        roots.push(home.join("Library/Application Support/Firefox/Profiles"));
        roots.push(home.join(".mozilla/firefox"));
    }

    if let Ok(appdata) = env::var("APPDATA") {
        roots.push(PathBuf::from(appdata).join("Mozilla/Firefox/Profiles"));
    }

    roots
}

/// All profiles that contain a Bandcamp login.
///
/// The name of a profile does not show if it has a login. Thus the tool examines all
/// profiles and removes the profiles that have no login.
pub fn find() -> Vec<FoundCookie> {
    let mut found = Vec::new();

    for root in profile_roots() {
        let profiles = match read_dir(&root) {
            Ok(profiles) => profiles,
            _ => continue,
        };

        for profile in profiles.flatten() {
            let store = profile.path().join("cookies.sqlite");

            if !store.exists() {
                continue;
            }

            let name = profile.file_name().to_string_lossy().to_string();

            if let Some(cookie) = read(&store, &name) {
                found.push(cookie);
            }
        }
    }

    found
}

fn read(store: &PathBuf, profile: &str) -> Option<FoundCookie> {
    let copied = copy_store(store, "firefox").ok()?;

    let found = (|| {
        let connection = Connection::open(&copied).ok()?;

        // Firefox keeps the cookie values as plain text. Thus no decryption is necessary.
        // The field lastAccessed is microseconds after the unix epoch.
        let mut statement = connection
            .prepare("SELECT value, lastAccessed FROM moz_cookies WHERE host LIKE ?1 AND name = ?2")
            .ok()?;

        let mut rows = statement
            .query_map((format!("%{}", HOST_SUFFIX), COOKIE_NAME), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .ok()?;

        let (value, last_access) = rows.next()?.ok()?;

        Some(FoundCookie {
            value,
            last_access,
            browser: "Firefox".to_string(),
            profile: profile.to_string(),
        })
    })();

    discard_store(&copied);

    found
}
