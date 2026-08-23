use crate::pace::{pace, throttled};
use crate::retry::ThrottledError;
use bytes::Bytes;
use futures::Stream;
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_RANGE, CONTENT_TYPE, RANGE, RETRY_AFTER};
use reqwest::{Client, Response, StatusCode, Url};
use std::error::Error;
use std::str::from_utf8;
use std::time::Duration;

/// Say what a page that arrived in place of a file is.
///
/// Bandcamp answers a request that is not ready, a request that it refuses, and a
/// request that is not logged in with a page each time. The status and the headers
/// are the same, thus only the words in the page tell them apart.
fn describe_page(body: &str) -> String {
    let text = body.to_lowercase();

    let known = [
        ("log in", "it wants you to log in, thus the cookie is not accepted"),
        ("logged in", "it wants you to log in, thus the cookie is not accepted"),
        ("preparing", "it is still building the archive"),
        ("try again", "it asks you to try again"),
        ("too many", "it says there are too many requests"),
        ("rate limit", "it says there are too many requests"),
        ("expired", "it says the link is no longer valid"),
        ("not found", "it says there is nothing there"),
    ];

    for (phrase, meaning) in known {
        if text.contains(phrase) {
            return meaning.to_string();
        }
    }

    // Nothing recognised, thus report the size and the title to identify it later
    let title = body
        .split_once("<title>")
        .and_then(|(_, rest)| rest.split_once("</title>"))
        .map(|(title, _)| title.trim().to_string())
        .unwrap_or_else(|| "no title".to_string());

    format!("{} bytes, titled \"{}\"", body.len(), title)
}

/// The time that the server asks the tool to wait, if the server supplies it
fn retry_after(response: &Response) -> Option<Duration> {
    response
        .headers()
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// The full length of the file, from the `bytes 100-999/1000` form of Content-Range
fn total_from_content_range(response: &Response) -> Option<u64> {
    response
        .headers()
        .get(CONTENT_RANGE)?
        .to_str()
        .ok()?
        .rsplit('/')
        .next()?
        .trim()
        .parse()
        .ok()
}

pub trait CollectionItemInfo {
    fn new(filename: String, filesize: usize) -> Self;
}

pub trait CollectionItem {
    type Item: CollectionItemInfo;
    fn get_url(&self) -> &Url;

    async fn get_download_url(&self, client: &Client) -> Result<Url, Box<dyn Error>>;

    /// This function and the download use the same prepared url. Thus the tool reads
    /// and downloads one archive, and not two different ones.
    async fn get_item_info(&self, client: &Client, download_url: &Url) -> Result<Self::Item, Box<dyn Error>> {
        pace().await;

        let response = client
            .get(download_url.clone())
            .header("Range", "bytes=0-0")
            .send()
            .await?;

        let headers = response.headers();

        let content_disposition = match headers.get(CONTENT_DISPOSITION) {
            Some(content_disposition) => content_disposition,
            None => return Err("No Content-Disposition header".into()),
        };

        let content_disposition = from_utf8(content_disposition.as_bytes())?;

        let filename = content_disposition.split(';').find_map(|part| {
            let part = part.trim();
            if part.starts_with("filename=") {
                part[9..].trim_matches('"').to_string().into()
            } else {
                None
            }
        });

        let filename = match filename {
            Some(filename) => filename,
            None => return Err("Filename not found in Content-Disposition header".into()),
        };

        let content_range = match headers.get(CONTENT_RANGE) {
            Some(content_range) => content_range,
            None => return Err("No Content-Range header".into()),
        };

        let filesize = content_range.to_str()?.replace("bytes 0-0/", "").parse::<usize>()?;

        Ok(Self::Item::new(filename, filesize))
    }

    /// Get the file from byte `from`. Return the full length of the file and the number
    /// of bytes that the server agreed to skip. The server can ignore the range. Thus the
    /// function returns the offset and the caller must not assume it.
    async fn get_download_stream(&self, client: &Client, download_url: &Url, from: u64)
        -> Result<(Option<u64>, u64, impl Stream<Item=Result<Bytes, reqwest::Error>>), Box<dyn Error>> {
        let mut request = client.get(download_url.clone());

        if from > 0 {
            request = request.header(RANGE, format!("bytes={}-", from));
        }

        pace().await;

        let response = request.send().await?;

        // The server says this directly, but not frequently
        if matches!(response.status(), StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE) {
            throttled(retry_after(&response));

            return Err(ThrottledError(format!("Bandcamp answered {}", response.status())).into());
        }

        // If Bandcamp limits the requests, or the archive is not ready, it sends an html
        // page. This page has the correct length. Thus a count of the bytes cannot find
        // the difference. But only a true download has an attachment.
        if response.headers().get(CONTENT_DISPOSITION).is_none() {
            // This is how Bandcamp refuses a request. Decrease the speed.
            throttled(retry_after(&response));

            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("unknown")
                .to_string();

            // Read what the page says. A page that asks you to log in, a page that
            // says the archive is being built, and a page that refuses the request
            // all arrive the same way, and only the words tell them apart.
            let body = response.text().await.unwrap_or_default();
            let said = describe_page(&body);

            return Err(ThrottledError(
                format!("Bandcamp sent {} and not a file: {}", content_type, said)).into());
        }

        // Only a partial response continues the download. A 200 response means that the
        // server ignored the range and sends the full file. Keep no bytes.
        let resumed_from = match response.status() == StatusCode::PARTIAL_CONTENT {
            true => from,
            _ => 0,
        };

        let total = match resumed_from > 0 {
            // A partial response gives the remaining length. Thus the full length comes
            // from Content-Range and not from Content-Length.
            true => total_from_content_range(&response).or(response.content_length().map(|left| left + resumed_from)),
            _ => response.content_length(),
        };

        Ok((total, resumed_from, response.bytes_stream()))
    }
}

pub trait CollectionIterator<'a, T: CollectionItem> {
    async fn get_next_batch(&mut self) -> Result<Option<()>, Box<dyn Error>>;
    async fn get_next_item(&mut self) -> Result<Option<T>, Box<dyn Error>>;
    async fn next(&mut self) -> Result<Option<T>, Box<dyn Error>> {
        let next_batch_ok = match self.get_next_item().await? {
            Some(item) => return Ok(Some(item)),
            _ => self.get_next_batch().await?
        };

        match next_batch_ok {
            Some(_) => self.get_next_item().await,
            _ => Ok(None)
        }
    }
}
