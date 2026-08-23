use crate::collection::metadata::ItemMetadata;
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use std::error::Error;

/// How Bandcamp writes purchase dates
const PURCHASED_FORMAT: &str = "%d %b %Y %H:%M:%S GMT";

/// Which parts of a collection a command should act on
#[derive(Debug, Default)]
pub struct Filters {
    pub query: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

/// Read a purchase date as Bandcamp writes it
pub fn parse_purchased(purchased: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(purchased, PURCHASED_FORMAT)
        .ok()
        .map(|naive| naive.and_utc())
}

/// Read either a plain date or a length of time back from now.
///
/// `m` means months rather than minutes: this filters purchase history, where
/// anything finer than a day is meaningless, so the usual confusion is not worth
/// keeping just to be consistent with other tools.
pub fn parse_when(text: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, Box<dyn Error>> {
    let text = text.trim();

    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Ok(date.and_hms_opt(0, 0, 0).unwrap().and_utc());
    }

    let (count, unit) = text.split_at(
        text.find(|c: char| !c.is_ascii_digit())
            .ok_or_else(|| format!("'{}' has no unit, try 7d, 2w, 3m or 1y", text))?);

    let count: i64 = count.parse().map_err(|_| format!("'{}' does not start with a number", text))?;

    let ago = match unit.to_ascii_lowercase().as_str() {
        "d" => Duration::days(count),
        "w" => Duration::weeks(count),
        "m" => Duration::days(count * 30),
        "y" => Duration::days(count * 365),
        _ => return Err(format!("'{}' is not a length of time, try 7d, 2w, 3m or 1y", unit).into()),
    };

    Ok(now - ago)
}

impl Filters {
    pub fn build(
        query: Option<String>,
        artist: Option<String>,
        album: Option<String>,
        since: Option<String>,
        until: Option<String>,
    ) -> Result<Self, Box<dyn Error>> {
        let now = Utc::now();

        Ok(Self {
            query,
            artist,
            album,
            since: since.map(|since| parse_when(&since, now)).transpose()?,
            until: until.map(|until| parse_when(&until, now)).transpose()?,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.query.is_none()
            && self.artist.is_none()
            && self.album.is_none()
            && self.since.is_none()
            && self.until.is_none()
    }

    /// Whether an item is one the command was asked to act on.
    ///
    /// An item without a description cannot match a filter. Thus the tool keeps it if
    /// there is no filter, and removes it if there is one.
    pub fn matches(&self, metadata: Option<&ItemMetadata>) -> bool {
        if self.is_empty() {
            return true;
        }

        let metadata = match metadata {
            Some(metadata) => metadata,
            _ => return false,
        };

        let band = metadata.band_name.clone().unwrap_or_default().to_lowercase();
        let title = metadata.item_title.clone().unwrap_or_default().to_lowercase();

        // A query without a field searches the artist and the title
        if let Some(query) = &self.query {
            let query = query.to_lowercase();

            if !band.contains(&query) && !title.contains(&query) {
                return false;
            }
        }

        if let Some(artist) = &self.artist {
            if !band.contains(&artist.to_lowercase()) {
                return false;
            }
        }

        if let Some(album) = &self.album {
            if !title.contains(&album.to_lowercase()) {
                return false;
            }
        }

        if self.since.is_some() || self.until.is_some() {
            let purchased = match metadata.purchased.as_deref().and_then(parse_purchased) {
                Some(purchased) => purchased,
                // There is no date, thus the item cannot match the range
                _ => return false,
            };

            if self.since.map(|since| purchased < since).unwrap_or(false) {
                return false;
            }

            if self.until.map(|until| purchased > until).unwrap_or(false) {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(band: &str, title: &str, purchased: Option<&str>) -> ItemMetadata {
        let mut metadata = ItemMetadata::for_test(Some(1), Some(band), Some(title));
        metadata.purchased = purchased.map(str::to_string);
        metadata
    }

    #[test]
    fn matches_part_of_either_name() {
        let filters = Filters { query: Some("blumen".into()), ..Default::default() };

        assert!(filters.matches(Some(&item("Ott", "Blumenkraft", None))));
        assert!(!filters.matches(Some(&item("Ott", "Skylon", None))));

        // The same query, but for the artist
        let filters = Filters { query: Some("ott".into()), ..Default::default() };
        assert!(filters.matches(Some(&item("Ott", "Skylon", None))));
    }

    #[test]
    fn narrows_to_one_field_when_asked() {
        let by_artist = Filters { artist: Some("ott".into()), ..Default::default() };

        assert!(by_artist.matches(Some(&item("Ott", "Skylon", None))));
        // The title contains the word, but the filter selects by artist
        assert!(!by_artist.matches(Some(&item("Someone", "Ott Remixes", None))));
    }

    #[test]
    fn reads_bandcamps_purchase_dates() {
        let purchased = parse_purchased("03 Jan 2024 10:58:30 GMT").unwrap();

        assert_eq!(purchased.format("%Y-%m-%d").to_string(), "2024-01-03");
        assert!(parse_purchased("sometime last year").is_none());
    }

    #[test]
    fn reads_both_plain_dates_and_lengths_of_time() {
        let now = parse_purchased("01 Jun 2026 00:00:00 GMT").unwrap();

        assert_eq!(parse_when("2024-01-03", now).unwrap().format("%Y-%m-%d").to_string(), "2024-01-03");
        assert_eq!(parse_when("7d", now).unwrap().format("%Y-%m-%d").to_string(), "2026-05-25");
        // The letter m is months, not minutes
        assert_eq!(parse_when("3m", now).unwrap().format("%Y-%m").to_string(), "2026-03");
        assert_eq!(parse_when("3M", now).unwrap().format("%Y-%m").to_string(), "2026-03");
        assert_eq!(parse_when("1y", now).unwrap().format("%Y").to_string(), "2025");

        assert!(parse_when("soon", now).is_err());
        assert!(parse_when("12", now).is_err());
    }

    #[test]
    fn keeps_only_what_falls_in_the_range() {
        let now = parse_purchased("01 Jun 2026 00:00:00 GMT").unwrap();
        let filters = Filters { since: Some(parse_when("1y", now).unwrap()), ..Default::default() };

        assert!(filters.matches(Some(&item("Ott", "Skylon", Some("08 Jun 2025 08:44:22 GMT")))));
        assert!(!filters.matches(Some(&item("Ott", "Skylon", Some("03 Jan 2024 10:58:30 GMT")))));
        // There is no date
        assert!(!filters.matches(Some(&item("Ott", "Skylon", None))));
    }

    #[test]
    fn an_undescribed_item_survives_only_an_empty_filter() {
        assert!(Filters::default().matches(None));
        assert!(!Filters { artist: Some("ott".into()), ..Default::default() }.matches(None));
    }
}
