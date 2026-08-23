use reqwest::cookie::Jar;
use reqwest::{Client, ClientBuilder, Url};

pub fn init_client(identity: &str) -> Client {
    // The jar needs a url to attach the cookie to
    let url = "https://bandcamp.com".parse::<Url>().unwrap();

    // The identity cookie applies to all bandcamp.com hosts
    let cookie = String::from("identity=") + identity + "; Domain=.bandcamp.com";

    let jar = Jar::default();
    jar.add_cookie_str(&cookie, &url);

    // Build the client
    ClientBuilder::new().cookie_provider(jar.into()).build().unwrap()
}
