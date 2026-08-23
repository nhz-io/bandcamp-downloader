use crate::collection::collection_item::CollectionItem;
use crate::collection::collection_summary::get_collection_summary;
use crate::collection::metadata::ItemMetadata;
use crate::collection::traits;
use crate::page::collection_page::CollectionPage;
use crate::page::download_page::DownloadFormat;
use crate::pace::pace;
use crate::page::traits::Page;
use crate::retry::with_retry;
use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::error::Error;
use std::str::FromStr;

#[derive(Deserialize, Debug)]
pub struct CollectionItemsResponse {
    pub redownload_urls: HashMap<String, String>,
    pub last_token: Option<String>,
    /// Later batches describe their items here, unlike the first page
    pub items: Option<Vec<ItemMetadata>>,
}

/// A download url paired with whatever Bandcamp said about it while listing it
type Queued = (String, Option<ItemMetadata>);

/// Pair each download url with its description, where one was given. The urls are
/// filed under the item's key, which is what makes the two halves line up.
fn pair(urls: HashMap<String, String>, described: Vec<ItemMetadata>) -> Vec<Queued> {
    let mut by_key: HashMap<String, ItemMetadata> = described
        .into_iter()
        .filter_map(|item| item.key().map(|key| (key, item)))
        .collect();

    urls.into_iter().map(|(key, url)| (url, by_key.remove(&key))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_each_url_with_its_own_description() {
        let urls = HashMap::from([
            ("p111".to_string(), "https://bandcamp.com/download?sitem_id=111".to_string()),
            ("p222".to_string(), "https://bandcamp.com/download?sitem_id=222".to_string()),
        ]);

        let described = vec![
            ItemMetadata::for_test(Some(222), Some("Ott"), Some("Blumenkraft")),
            ItemMetadata::for_test(Some(111), Some("Ajja"), Some("Trilon")),
        ];

        let paired = pair(urls, described);

        for (url, metadata) in paired {
            let label = metadata.expect("every url here was described").label().unwrap();

            // Order is not guaranteed, so each pairing is checked on its own terms
            match url.ends_with("111") {
                true => assert_eq!(label, "Ajja - Trilon"),
                _ => assert_eq!(label, "Ott - Blumenkraft"),
            }
        }
    }

    #[test]
    fn pairs_items_whose_type_is_not_p() {
        // Bandcamp files an url under its item type and its id together, and the type
        // is not always p. An item of another type has a description, and a key that
        // always started with p did not find it.
        let urls = HashMap::from([
            ("r555".to_string(), "https://bandcamp.com/download?sitem_id=555".to_string()),
            ("c999".to_string(), "https://bandcamp.com/download?sitem_id=999".to_string()),
        ]);

        let mut compilation = ItemMetadata::for_test(Some(555), Some("Basilisk"), Some("Greatest Trips"));
        compilation.sale_item_type = Some("r".to_string());

        let mut other = ItemMetadata::for_test(Some(999), Some("Someone"), Some("Something"));
        other.sale_item_type = Some("c".to_string());

        let paired = pair(urls, vec![compilation, other]);

        assert_eq!(paired.len(), 2);
        assert!(paired.iter().all(|(_, described)| described.is_some()),
                "every url here was described, whatever its type");
    }

    #[test]
    fn an_undescribed_url_is_still_queued() {
        let urls = HashMap::from([
            ("p111".to_string(), "https://bandcamp.com/download?sitem_id=111".to_string()),
        ]);

        // The first page describes only some of its items
        let paired = pair(urls, vec![ItemMetadata::for_test(Some(999), Some("Other"), Some("Thing"))]);

        assert_eq!(paired.len(), 1);
        assert!(paired[0].1.is_none());
    }
}

pub struct CollectionIterator<'a> {
    client: &'a Client,
    queued: Option<Vec<Queued>>,
    last_token: Option<String>,
    index: usize,
    fan_id: usize,
    format: DownloadFormat,
    /// The total number of items. Only the first page supplies this.
    total: Option<usize>,
}

impl<'a> CollectionIterator<'a> {
    pub fn new(client: &'a Client, format: DownloadFormat) -> Self {
        Self {
            client,
            index: 0,
            fan_id: 0,
            last_token: None,
            queued: None,
            format,
            total: None,
        }
    }

    /// The number of items in the collection, after the first page supplies it
    pub fn total(&self) -> Option<usize> {
        self.total
    }
}

impl<'a> traits::CollectionIterator<'a, CollectionItem> for CollectionIterator<'a> {
    async fn get_next_batch(&mut self) -> Result<Option<()>, Box<dyn Error>> {
        let client = self.client;
        let fan_id = self.fan_id;
        let last_token = self.last_token.clone();

        let response = with_retry("Collection batch", move || {
            let last_token = last_token.clone();

            async move {
                pace().await;

                Ok(client
                    .post("https://bandcamp.com/api/fancollection/1/collection_items")
                    .json(&json!({
                        "older_than_token": last_token,
                        "fan_id": fan_id,
                    }))
                    .send()
                    .await?
                    .json::<CollectionItemsResponse>()
                    .await?)
            }
        }).await?;

        // Stop if there are no download urls
        if response.redownload_urls.len() < 1 {
            return Ok(None);
        }

        // Reset the index
        self.index = 0;

        // Keep the download urls with their descriptions
        self.queued = Some(pair(response.redownload_urls, response.items.unwrap_or_default()));

        // Update last token
        self.last_token = response.last_token;

        Ok(Some(()))
    }

    async fn get_next_item(&mut self) -> Result<Option<CollectionItem>, Box<dyn Error>> {
        if self.queued.is_none() {
            let collection_summary = get_collection_summary(self.client).await?;

            // Set the fan id
            self.fan_id = collection_summary.fan_id;

            // Get collection page data
            let page_data = CollectionPage::new(Url::from_str(&collection_summary.url)?)
                .get_page_data(self.client)
                .await?;

            // The first page keeps the descriptions in a different object. It describes
            // only some of the urls. The other urls have no description.
            let described = page_data
                .item_cache
                .and_then(|cache| cache.collection)
                .map(|collection| collection.into_values().collect())
                .unwrap_or_default();

            let collection_data = page_data.collection_data;

            self.total = Some(collection_data.item_count);

            // Set initial last token
            self.last_token = Some(collection_data.last_token);

            // Update the download urls
            self.queued = Some(pair(collection_data.redownload_urls, described));

            // Reset the index
            self.index = 0;
        }

        let queued = self.queued.as_ref().unwrap();

        if self.index >= queued.len() {
            return Ok(None);
        }

        let (url, metadata) = &queued[self.index];
        self.index += 1;

        Ok(CollectionItem::new(Url::from_str(url)?, self.format, metadata.clone()).into())
    }
}