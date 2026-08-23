use crate::pace::pace;
use crate::retry::TerminalError;
use reqwest::{Client, Url};
use serde::Deserialize;
use std::error::Error;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

/// The maximum number of new links to follow for one item
const MAX_RETRY_HOPS: usize = 3;

#[derive(Deserialize, Debug)]
struct StatResult {
    result: String,
    download_url: Option<String>,
    errortype: Option<String>,
    retry_url: Option<String>,
}

/// The url that gives the status of the archive. It comes from the url that requests it.
fn stat_url(url: &Url) -> Result<Url, Box<dyn Error>> {
    let mut stat_url = Url::from_str(&url.as_str().replacen("/download/", "/statdownload/", 1))?;

    // The Bandcamp client marks each request. Thus it can ignore old answers.
    let rand = SystemTime::now().duration_since(UNIX_EPOCH)?.subsec_nanos();

    stat_url
        .query_pairs_mut()
        .append_pair(".rand", &rand.to_string())
        .append_pair(".vrs", "1");

    Ok(stat_url)
}

/// The response is JSONP. Read the first json object from the callback.
fn parse_stat_result(body: &str) -> Result<StatResult, Box<dyn Error>> {
    let callback = body.find("statResult").ok_or("Unknown statdownload response")?;

    let start = body[callback..]
        .find('{')
        .map(|offset| callback + offset)
        .ok_or("No json object in the statdownload response")?;

    // The stream deserializer stops at the end of the object and ignores the rest
    let mut results = serde_json::Deserializer::from_str(&body[start..]).into_iter::<StatResult>();

    match results.next() {
        Some(result) => Ok(result?),
        _ => Err("Empty statdownload response".into()),
    }
}

/// Tell Bandcamp to build the archive and get the url of the file.
///
/// The url on the download page only requests the archive. If you get that url directly
/// while Bandcamp builds the archive, the response contains no file.
pub async fn resolve_download_url(client: &Client, packaging_url: &Url) -> Result<Url, Box<dyn Error>> {
    let mut url = packaging_url.clone();

    for _ in 0..MAX_RETRY_HOPS {
        pace().await;
        let body = client.get(stat_url(&url)?).send().await?.text().await?;

        let stat_result = parse_stat_result(&body)?;

        if stat_result.result == "ok" {
            return match stat_result.download_url {
                Some(download_url) => Ok(Url::from_str(&download_url)?),
                // Bandcamp sends no url if the request url is the file
                _ => Ok(url),
            };
        }

        let errortype = stat_result.errortype.unwrap_or_else(|| "unknown".to_string());

        match errortype.as_str() {
            // Bandcamp removed the music. A subsequent attempt cannot get it.
            "DeletedError" => return Err(TerminalError::new(
                format!("not available on Bandcamp ({})", errortype)).into()),

            "ExceedsFreeDownloadsError" => return Err(TerminalError::new(
                format!("you made the maximum number of downloads ({})", errortype)).into()),

            // The link is not valid. Each attempt makes the same link from the same
            // collection entry. The download page can supply a new link, but it asks for
            // an email address first. Only a person can do this.
            "ExpirationError" => return Err(TerminalError::needs_relink(
                format!("the link is not valid. Request a new one from the download page ({})", errortype)).into()),

            // Bandcamp supplied a new link. Use it.
            "ExpiredFreeDownloadError" => match stat_result.retry_url {
                Some(retry_url) => url = Url::from_str(&retry_url)?,
                _ => return Err(TerminalError::needs_relink(
                    format!("the link is not valid and Bandcamp did not offer a new one ({})", errortype)).into()),
            },

            // Bandcamp builds the archive, or the signature is old. Try again.
            _ => return Err(format!("the archive is not ready ({})", errortype).into()),
        }
    }

    Err(format!("Too many new links for: {}", packaging_url).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_download_url_out_of_the_jsonp_wrapper() {
        let body = r#"if ( window.Downloads ) { Downloads.statResult ( {"result":"ok","url":"popplers5.bandcamp.com/statdownload/album?enc=flac","download_url":"https://p4.bcbits.com/download/album/abc/flac/1?token=1_x"} ) };"#;

        let stat_result = parse_stat_result(body).unwrap();

        assert_eq!(stat_result.result, "ok");
        assert_eq!(stat_result.download_url.unwrap(), "https://p4.bcbits.com/download/album/abc/flac/1?token=1_x");
    }

    #[test]
    fn reads_the_error_type_out_of_a_failure() {
        let body = r#"if ( window.Downloads ) { Downloads.statResult ( {"result":"err","errortype":"DeletedError"} ) };"#;

        let stat_result = parse_stat_result(body).unwrap();

        assert_eq!(stat_result.result, "err");
        assert_eq!(stat_result.errortype.unwrap(), "DeletedError");
    }

    #[test]
    fn builds_the_stat_url_from_the_packaging_url() {
        let url = Url::from_str("https://popplers5.bandcamp.com/download/album?enc=flac&id=1").unwrap();

        let stat_url = stat_url(&url).unwrap();

        assert!(stat_url.as_str().starts_with("https://popplers5.bandcamp.com/statdownload/album?enc=flac&id=1"));
        assert!(stat_url.as_str().contains(".vrs=1"));
        assert!(stat_url.as_str().contains(".rand="));
    }
}
