use reqwest::cookie::{CookieStore, Jar};
use sha2::{Digest, Sha256};
use reqwest::{Client, ClientBuilder, Url};
use std::sync::Arc;

fn bandcamp() -> Url {
    "https://bandcamp.com".parse::<Url>().unwrap()
}

/// Build the client, and keep the jar so the caller can read the cookie later.
///
/// The jar is shared with the client. If Bandcamp sends a new identity cookie, the
/// client stores it here, thus reading the jar shows what the tool is sending now.
pub fn init_client(identity: &str) -> (Client, Arc<Jar>) {
    let url = bandcamp();

    // The identity cookie applies to all bandcamp.com hosts
    let cookie = String::from("identity=") + identity + "; Domain=.bandcamp.com";

    let jar = Arc::new(Jar::default());
    jar.add_cookie_str(&cookie, &url);

    // Build the client
    let client = ClientBuilder::new()
        .cookie_provider(jar.clone())
        .build()
        .unwrap();

    (client, jar)
}

/// A short hash of the identity cookie that the jar holds now.
///
/// This is a hash and not the cookie. The value logs in to the account, thus no part
/// of it goes to the terminal, where it would stay in the scrollback and in the logs
/// that a server collects. A hash changes when the cookie changes, which is all that
/// the tool has to know.
pub fn identity_in(jar: &Jar) -> Option<String> {
    let header = jar.cookies(&bandcamp())?;

    let identity = header
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|pair| pair.strip_prefix("identity=").map(str::to_string))?;

    let mut hasher = Sha256::new();
    hasher.update(identity.as_bytes());

    Some(format!("{:x}", hasher.finalize()).chars().take(12).collect())
}
