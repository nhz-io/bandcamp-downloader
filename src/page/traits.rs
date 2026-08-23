use crate::pace::pace;
use reqwest::{Client, Url};
use select::document::Document;
use select::predicate::{Attr, Name, Predicate};
use serde::de::DeserializeOwned;

pub trait Page<T>
where
    T: DeserializeOwned,
{
    fn get_url(&self) -> &Url;
    async fn get_page_data(&self, client: &Client) -> Result<T, Box<dyn std::error::Error>> {
        // Get the page
        pace().await;
        let response = client.get(self.get_url().clone()).send().await?;

        // Read the response as text
        let text = response.text().await?;

        // Make an HTML document from the text
        let document = Document::from(text.as_str());

        // Find the pagedata div
        let pagedata_div = match document.find(Name("div").and(Attr("id", "pagedata"))).next() {
            Some(value) => Ok(value),
            _ => Err("The pagedata div was not found"),
        }?;

        // Get the data-blob from the pagedata div
        let data_blob = match pagedata_div.attr("data-blob") {
            Some(value) => Ok(value),
            _ => Err("The pagedata div does not contain a data-blob")
        }?;

        match serde_json::from_str::<T>(data_blob) {
            Ok(page_data) => Ok(page_data.into()),
            Err(e) => Err(e.into()),
        }
    }
}
