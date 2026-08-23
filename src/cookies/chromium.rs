use super::{copy_store, discard_store, FoundCookie, COOKIE_NAME, HOST_SUFFIX};
use aes::Aes128;
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use rusqlite::Connection;
use sha1::Sha1;
use std::env;
use std::error::Error;
use std::fs::read_dir;
use std::path::PathBuf;

type Aes128CbcDec = cbc::Decryptor<Aes128>;

/// Chromium counts the time from 1601, not from 1970
const EPOCH_OFFSET_MICROS: i64 = 11_644_473_600_000_000;

/// The salt and the iv are the same on all systems. Only the password is secret.
const SALT: &[u8] = b"saltysalt";
const IV: [u8; 16] = [0x20; 16];

/// macOS applies many more iterations to the password than the linux default
#[cfg(target_os = "macos")]
const ITERATIONS: u32 = 1003;
#[cfg(not(target_os = "macos"))]
const ITERATIONS: u32 = 1;

struct Vendor {
    name: &'static str,
    mac_dir: &'static str,
    linux_dir: &'static str,
    service: &'static str,
    account: &'static str,
}

const VENDORS: &[Vendor] = &[
    Vendor { name: "Chrome", mac_dir: "Google/Chrome", linux_dir: "google-chrome", service: "Chrome Safe Storage", account: "Chrome" },
    Vendor { name: "Chromium", mac_dir: "Chromium", linux_dir: "chromium", service: "Chromium Safe Storage", account: "Chromium" },
    Vendor { name: "Edge", mac_dir: "Microsoft Edge", linux_dir: "microsoft-edge", service: "Microsoft Edge Safe Storage", account: "Microsoft Edge" },
    Vendor { name: "Brave", mac_dir: "BraveSoftware/Brave-Browser", linux_dir: "BraveSoftware/Brave-Browser", service: "Brave Safe Storage", account: "Brave" },
    Vendor { name: "Vivaldi", mac_dir: "Vivaldi", linux_dir: "vivaldi", service: "Vivaldi Safe Storage", account: "Vivaldi" },
];

fn roots(vendor: &Vendor) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(home) = env::var("HOME") {
        let home = PathBuf::from(home);

        roots.push(home.join("Library/Application Support").join(vendor.mac_dir));
        roots.push(home.join(".config").join(vendor.linux_dir));
    }

    if let Ok(local) = env::var("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join(vendor.mac_dir).join("User Data"));
    }

    roots
}

/// The tool finds the profiles by the directories that contain a cookie store. The
/// directory of the browser contains many directories that are not profiles.
fn stores(root: &PathBuf) -> Vec<(PathBuf, String)> {
    let mut stores = Vec::new();

    let entries = match read_dir(root) {
        Ok(entries) => entries,
        _ => return stores,
    };

    for entry in entries.flatten() {
        let directory = entry.path();

        if !directory.is_dir() {
            continue;
        }

        let profile = entry.file_name().to_string_lossy().to_string();

        // Recent versions keep the store one level lower
        for candidate in [directory.join("Cookies"), directory.join("Network").join("Cookies")] {
            if candidate.exists() {
                stores.push((candidate, profile.clone()));
            }
        }
    }

    stores
}

pub fn find(browser: Option<&str>) -> Vec<FoundCookie> {
    let mut found = Vec::new();

    for vendor in VENDORS {
        if let Some(wanted) = browser {
            if !vendor.name.to_lowercase().contains(&wanted.to_lowercase()) {
                continue;
            }
        }

        // The tool gets this one time for each browser, and only after it finds a cookie
        // to decrypt. Thus no permission dialog appears if there is nothing to read.
        let mut password: Option<String> = None;

        for root in roots(vendor) {
            for (store, profile) in stores(&root) {
                let (encrypted, last_access) = match read_encrypted(&store) {
                    Some(row) => row,
                    _ => continue,
                };

                if password.is_none() {
                    password = match safe_storage_password(vendor) {
                        Ok(password) => Some(password),
                        Err(e) => {
                            eprintln!("Cannot decrypt the {} cookies: {}", vendor.name, e);
                            continue;
                        }
                    };
                }

                let password = match &password {
                    Some(password) => password,
                    _ => continue,
                };

                match decrypt(&encrypted, password) {
                    Ok(value) => found.push(FoundCookie {
                        value,
                        last_access: last_access - EPOCH_OFFSET_MICROS,
                        browser: vendor.name.to_string(),
                        profile,
                    }),
                    Err(e) => eprintln!("Cannot read the {} cookie in {}: {}", vendor.name, profile, e),
                }
            }
        }
    }

    found
}

fn read_encrypted(store: &PathBuf) -> Option<(Vec<u8>, i64)> {
    let copied = copy_store(store, "chromium").ok()?;

    let row = (|| {
        let connection = Connection::open(&copied).ok()?;

        let mut statement = connection
            .prepare("SELECT encrypted_value, last_access_utc FROM cookies WHERE host_key LIKE ?1 AND name = ?2")
            .ok()?;

        let mut rows = statement
            .query_map((format!("%{}", HOST_SUFFIX), COOKIE_NAME), |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
            })
            .ok()?;

        rows.next()?.ok()
    })();

    discard_store(&copied);

    row
}

/// Decrypt the AES-CBC that chromium applies to each cookie value
fn decrypt(encrypted: &[u8], password: &str) -> Result<String, Box<dyn Error>> {
    if encrypted.len() < 3 {
        return Err("the stored value is too short".into());
    }

    let (version, body) = encrypted.split_at(3);

    // v10 and v11 use a different password source. The tool gets the password before this.
    if version != b"v10" && version != b"v11" {
        return Err(format!("unknown encryption {}. This browser is too new.", String::from_utf8_lossy(version)).into());
    }

    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<Sha1>(password.as_bytes(), SALT, ITERATIONS, &mut key);

    let mut buffer = body.to_vec();

    let plain = Aes128CbcDec::new(&key.into(), &IV.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(|e| format!("cannot decrypt the value: {}", e))?;

    // Recent versions put a hash of the domain before the value. The identity token
    // contains printable characters only. Thus bytes that are not printable are the hash.
    let plain = match plain.len() > 32 && plain[..32].iter().any(|byte| !byte.is_ascii_graphic()) {
        true => &plain[32..],
        _ => plain,
    };

    Ok(String::from_utf8_lossy(plain).to_string())
}

/// The password that chromium encrypts with. The operating system keeps it.
#[cfg(target_os = "macos")]
fn safe_storage_password(vendor: &Vendor) -> Result<String, Box<dyn Error>> {
    use std::process::Command;

    // A read of the keychain causes the permission dialog
    let output = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-w", "-s", vendor.service, "-a", vendor.account])
        .output()?;

    if !output.status.success() {
        return Err("the keychain did not supply the key".into());
    }

    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

#[cfg(target_os = "linux")]
fn safe_storage_password(_vendor: &Vendor) -> Result<String, Box<dyn Error>> {
    // Chromium uses this fixed password if no keyring is available
    Ok("peanuts".to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn safe_storage_password(_vendor: &Vendor) -> Result<String, Box<dyn Error>> {
    Err("this system encrypts chromium cookies with DPAPI. The tool cannot read them.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_an_encryption_it_does_not_know() {
        let error = decrypt(b"v20somethingelse", "password").unwrap_err().to_string();

        assert!(error.contains("unknown encryption"), "{}", error);
    }

    #[test]
    fn refuses_a_value_too_short_to_be_encrypted() {
        assert!(decrypt(b"v1", "password").is_err());
    }
}
