use serde::Deserialize;

/// What Bandcamp already tells us about a purchase while listing the collection.
///
/// Fetched anyway as part of enumerating the collection, so keeping it costs nothing
/// and is the difference between reporting a problem against a name and against a url.
#[derive(Deserialize, Debug, Clone)]
pub struct ItemMetadata {
    pub sale_item_id: Option<u64>,
    /// The letter before the id in the key. Bandcamp uses p, r, and c.
    pub sale_item_type: Option<String>,
    pub band_name: Option<String>,
    pub item_title: Option<String>,
    pub purchased: Option<String>,
    pub item_url: Option<String>,
}

#[cfg(test)]
impl ItemMetadata {
    pub fn for_test(sale_item_id: Option<u64>, band_name: Option<&str>, item_title: Option<&str>) -> Self {
        Self {
            sale_item_id,
            sale_item_type: None,
            band_name: band_name.map(str::to_string),
            item_title: item_title.map(str::to_string),
            purchased: None,
            item_url: None,
        }
    }
}

impl ItemMetadata {
    /// The key this item's download url is filed under.
    ///
    /// The key is the type and the id together. Bandcamp uses more than one type, and
    /// an item of another type does not match a key that always starts with p.
    pub fn key(&self) -> Option<String> {
        let item_type = self.sale_item_type.clone().unwrap_or_else(|| "p".to_string());

        self.sale_item_id.map(|id| format!("{}{}", item_type, id))
    }

    /// How the item should be named to a person
    pub fn label(&self) -> Option<String> {
        match (&self.band_name, &self.item_title) {
            (Some(band_name), Some(item_title)) => Some(format!("{} - {}", band_name, item_title)),
            (_, Some(item_title)) => Some(item_title.clone()),
            _ => None,
        }
    }
}
