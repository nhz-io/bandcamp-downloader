use crate::collection::metadata::ItemMetadata;
use crate::page::page::Page;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize, Debug)]
pub struct CollectionData {
    pub item_count: usize,
    pub last_token: String,
    pub redownload_urls: HashMap<String, String>,
}

/// The first page describes its items here and not with the download urls. It
/// describes only some of them. The other items have no description.
#[derive(Deserialize, Debug)]
pub struct ItemCache {
    pub collection: Option<HashMap<String, ItemMetadata>>,
}

#[derive(Deserialize, Debug)]
pub struct CollectionPageData {
    pub collection_data: CollectionData,
    pub item_cache: Option<ItemCache>,
}

pub type CollectionPage = Page<CollectionPageData>;