use crate::pace::pace;
use reqwest::Client;
use serde::Deserialize;
use std::error::Error;

const COLLECTION_SUMMARY_URL: &str = "https://bandcamp.com/api/fan/2/collection_summary";

#[derive(Deserialize, Debug)]
pub struct CollectionSummary {
    pub fan_id: usize,
    pub url: String,
}

#[derive(Deserialize, Debug)]
struct CollectionSummaryResponse {
    // Absent when the identity cookie is missing or expired
    collection_summary: Option<CollectionSummary>,
}

/// Resolve the logged in fan.
///
/// The home page used to carry this in its `pagedata` blob, but Bandcamp no
/// longer emits `identities` there, so ask the API instead.
pub async fn get_collection_summary(client: &Client) -> Result<CollectionSummary, Box<dyn Error>> {
    pace().await;

    let response = client
        .get(COLLECTION_SUMMARY_URL)
        .send()
        .await?
        .json::<CollectionSummaryResponse>()
        .await?;

    match response.collection_summary {
        Some(collection_summary) => Ok(collection_summary),
        _ => Err("Not logged in. The identity cookie is absent or not valid.".into()),
    }
}
