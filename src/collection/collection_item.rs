use crate::collection::metadata::ItemMetadata;
use crate::collection::traits;
use crate::page::download_page::{DownloadFormat, DownloadPage};
use crate::stat_download::resolve_download_url;
use crate::page::traits::Page;
use reqwest::{Client, Url};
use std::error::Error;
use std::str::FromStr;

#[derive(Debug)]
pub struct CollectionItemInfo {
    pub filename: String,
    pub filesize: usize,
}

impl traits::CollectionItemInfo for CollectionItemInfo {
    fn new(filename: String, filesize: usize) -> Self {
        Self {
            filename,
            filesize,
        }
    }
}

#[derive(Debug)]
pub struct CollectionItem {
    url: Url,
    format: DownloadFormat,
    metadata: Option<ItemMetadata>,
}

impl CollectionItem {
    pub fn new(url: impl Into<Url>, format: DownloadFormat, metadata: Option<ItemMetadata>) -> Self {
        Self { url: url.into(), format, metadata }
    }

    pub fn metadata(&self) -> Option<&ItemMetadata> {
        self.metadata.as_ref()
    }

    /// How this item should be named to a person, falling back to the url when
    /// Bandcamp did not describe it
    pub fn label(&self) -> String {
        self.metadata
            .as_ref()
            .and_then(|metadata| metadata.label())
            .unwrap_or_else(|| self.url.as_str().to_string())
    }
}

impl traits::CollectionItem for CollectionItem {
    type Item = CollectionItemInfo;

    fn get_url(&self) -> &Url {
        &self.url
    }

    async fn get_download_url(&self, client: &Client) -> Result<Url, Box<dyn Error>> {
        let download_page_data = DownloadPage::new(self.get_url().clone()).get_page_data(&client).await?;

        let download_items = match &download_page_data.download_items {
            Some(download_items) => match download_items.len() {
                0 => return Err(format!("Empty download_items for: {}", self.get_url()).into()),
                _ => download_items.iter().next().unwrap()
            },
            _ => return Err(format!("Missing download_items for: {}", self.get_url()).into())
        };

        let downloads = match &download_items.downloads {
            Some(downloads) => downloads,
            _ => return Err(format!("Missing download_items[0].downloads for: {}", self.get_url()).into()),
        };

        let format = self.format.as_key();

        let packaging_url = match downloads.get(format).and_then(|download_item| download_item.url.as_ref()) {
            Some(url) => Url::from_str(url)?,
            _ => return Err(format!("No {} download available for: {}", format, self.get_url()).into()),
        };

        // That url only asks Bandcamp to build the archive, so wait for the real one
        resolve_download_url(client, &packaging_url).await
    }
}