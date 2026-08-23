use crate::collection::metadata::ItemMetadata;
use chrono::{DateTime, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::error::Error;
use std::fs::{rename, File};
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};

/// The record stays with the music. Thus a directory can describe its own contents.
pub const MANIFEST_FILENAME: &str = ".bandcamp-manifest.json";

const MANIFEST_VERSION: u32 = 1;

/// Replaced versions stay here, with the music that they came from
pub const VERSIONS_DIRNAME: &str = ".versions";

/// The location of a version after Bandcamp replaced it. The name contains the date
/// and the fingerprint. Thus two replacements of one album cannot use the same name.
pub fn superseded_path(root: &Path, record: &DownloadRecord) -> PathBuf {
    let stamp = record.downloaded_at.format("%Y%m%d");
    let short_fingerprint = record.fingerprint.chars().take(8).collect::<String>();

    root.join(VERSIONS_DIRNAME)
        .join(format!("{}_{}", stamp, short_fingerprint))
        .join(&record.filename)
}

/// One archive of one item in one format
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DownloadRecord {
    pub filename: String,
    pub filesize: u64,
    /// This identifies the audio and not the archive. Thus a new archive of the same
    /// music is not a change, but a track that is removed or replaced is a change.
    pub fingerprint: String,
    pub tracks: Vec<String>,
    /// True if the track list comes from the archive. False if it is only the file name.
    #[serde(default)]
    pub is_archive: bool,
    pub downloaded_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct ItemRecord {
    /// The key is the format. Thus you can keep one album as flac and as mp3.
    pub downloads: HashMap<String, DownloadRecord>,
    /// The versions that Bandcamp replaced, oldest first. The tool keeps them because
    /// an album that loses a track must not cause the loss of your complete copy.
    #[serde(default)]
    pub superseded: HashMap<String, Vec<DownloadRecord>>,
    /// This makes the record readable. It also stays here after Bandcamp removes
    /// the item.
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub purchased: Option<String>,
    #[serde(default)]
    pub item_url: Option<String>,
    /// The last time that Bandcamp offered this item. Thus you can find the items that it removed.
    pub last_seen: Option<DateTime<Utc>>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Manifest {
    pub version: u32,
    /// The key is the Bandcamp sale item id. File names and sizes can change. This id does not.
    pub items: HashMap<String, ItemRecord>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: MANIFEST_VERSION,
            items: HashMap::new(),
        }
    }
}

impl Manifest {
    /// An absent record is an empty record. Thus a new directory operates correctly.
    pub fn load(path: &Path) -> Result<Self, Box<dyn Error>> {
        let file = match File::open(path) {
            Ok(file) => file,
            _ => return Ok(Self::default()),
        };

        Ok(serde_json::from_reader(BufReader::new(file))?)
    }

    /// The tool writes a temporary file and then renames it. Thus an interrupted write
    /// cannot destroy the record of the music that you have.
    pub fn save(&self, path: &Path) -> Result<(), Box<dyn Error>> {
        let mut writing_path = PathBuf::from(path);
        writing_path.set_extension("writing");

        let file = File::create(&writing_path)?;
        serde_json::to_writer_pretty(BufWriter::new(file), self)?;

        Ok(rename(&writing_path, path)?)
    }

    pub fn get(&self, sale_item_id: &str, format: &str) -> Option<&DownloadRecord> {
        self.items.get(sale_item_id)?.downloads.get(format)
    }

    pub fn record(&mut self, sale_item_id: &str, format: &str, download: DownloadRecord) {
        let item = self.items.entry(sale_item_id.to_string()).or_default();

        item.downloads.insert(format.to_string(), download);
        item.last_seen = Some(Utc::now());
    }

    /// Move a version into the history
    pub fn supersede(&mut self, sale_item_id: &str, format: &str, download: DownloadRecord) {
        self.items
            .entry(sale_item_id.to_string())
            .or_default()
            .superseded
            .entry(format.to_string())
            .or_default()
            .push(download);
    }

    /// Record the description from Bandcamp while Bandcamp still supplies it
    pub fn describe(&mut self, sale_item_id: &str, metadata: &ItemMetadata) {
        let item = self.items.entry(sale_item_id.to_string()).or_default();

        item.artist = metadata.band_name.clone();
        item.title = metadata.item_title.clone();
        item.purchased = metadata.purchased.clone();
        item.item_url = metadata.item_url.clone();
        item.last_seen = Some(Utc::now());
    }

    pub fn mark_seen(&mut self, sale_item_id: &str) {
        self.items.entry(sale_item_id.to_string()).or_default().last_seen = Some(Utc::now());
    }
}

/// The Bandcamp id for a purchase. The download page url contains it as `sitem_id`.
pub fn sale_item_id(url: &Url) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == "sitem_id")
        .map(|(_, value)| value.to_string())
}

#[derive(Debug, Clone)]
pub struct Fingerprinted {
    pub digest: String,
    pub tracks: Vec<String>,
    /// True if the tool read the contents as an archive. A file that must be an archive
    /// but is not is damaged. This field stops the tool from using it as a new version.
    pub is_archive: bool,
}

/// True if the tool must be able to open this file as an archive
pub fn looks_like_archive(path: &Path) -> bool {
    path.extension().map(|e| e.eq_ignore_ascii_case("zip")).unwrap_or(false)
}

/// Calculate a fingerprint from the contents of an archive, not from its bytes.
///
/// Bandcamp builds an archive again for each request. Thus the bytes change but the
/// music stays the same. Each entry has a CRC of its uncompressed contents. A hash of
/// these CRCs ignores the new archive but finds a track that is removed or replaced.
///
/// If the file is not an archive, the tool uses `name` as the track list. Thus an
/// incomplete file never enters the record with its temporary name.
pub fn fingerprint(path: &Path, name: &str) -> Result<Fingerprinted, Box<dyn Error>> {
    match fingerprint_archive(path) {
        Ok((digest, tracks)) => Ok(Fingerprinted { digest, tracks, is_archive: true }),
        // A single track is an audio file and not an archive
        _ => {
            let digest = fingerprint_bytes(path)?;

            Ok(Fingerprinted { digest, tracks: vec![name.to_string()], is_archive: false })
        }
    }
}

/// Read all entries. Thus the tool compares the stored CRCs with the bytes.
///
/// A continued download is two responses that join in the middle. If Bandcamp built the
/// archive again between the two responses, the index can agree but the contents cannot.
/// Only a full read finds this.
pub fn verify_archive(path: &Path) -> Result<(), Box<dyn Error>> {
    let mut archive = zip::ZipArchive::new(BufReader::new(File::open(path)?))?;
    let mut discard = vec![0u8; 64 * 1024];

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;

        // The zip crate compares the CRC only if the tool reads to the end
        while entry.read(&mut discard)? > 0 {}
    }

    Ok(())
}

fn fingerprint_archive(path: &Path) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut archive = zip::ZipArchive::new(BufReader::new(File::open(path)?))?;

    let mut entries = Vec::new();

    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        entries.push((entry.name().to_string(), entry.size(), entry.crc32()));
    }

    // The order in the archive can change. Sort the entries first.
    entries.sort();

    let mut hasher = Sha256::new();

    for (name, size, crc) in &entries {
        hasher.update(format!("{}\u{0}{}\u{0}{:08x}\n", name, size, crc));
    }

    let tracks = entries.into_iter().map(|(name, _, _)| name).collect();

    Ok((format!("{:x}", hasher.finalize()), tracks))
}

fn fingerprint_bytes(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut file = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];

    loop {
        let read = file.read(&mut buffer)?;

        if read == 0 {
            break;
        }

        hasher.update(&buffer[..read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::str::FromStr;
    use zip::write::SimpleFileOptions;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("bcdl-test-{}", name));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn write_archive(path: &Path, entries: &[(&str, &[u8])]) {
        let mut writer = zip::ZipWriter::new(File::create(path).unwrap());

        for (name, body) in entries {
            writer.start_file(*name, SimpleFileOptions::default()).unwrap();
            writer.write_all(body).unwrap();
        }

        writer.finish().unwrap();
    }

    #[test]
    fn reads_the_sale_item_id_out_of_a_download_url() {
        let url = Url::from_str("https://bandcamp.com/download?from=collection&payment_id=1&sig=abc&sitem_id=123").unwrap();

        assert_eq!(sale_item_id(&url).unwrap(), "123");
    }

    #[test]
    fn has_no_sale_item_id_when_the_url_carries_none() {
        let url = Url::from_str("https://bandcamp.com/download?from=collection").unwrap();

        assert_eq!(sale_item_id(&url), None);
    }

    #[test]
    fn ignores_repackaging_but_notices_a_replaced_track() {
        let original = scratch("original.zip");
        let repacked = scratch("repacked.zip");
        let altered = scratch("altered.zip");

        let tracks: &[(&str, &[u8])] = &[("01 One.mp3", b"first"), ("02 Two.mp3", b"second")];

        write_archive(&original, tracks);
        // The same music in a new archive and in a different order
        write_archive(&repacked, &[tracks[1], tracks[0]]);
        // A replaced track. The tool must find this change.
        write_archive(&altered, &[tracks[0], ("02 Two.mp3", b"replaced")]);

        let original_print = fingerprint(&original, "ignored").unwrap();
        let repacked_print = fingerprint(&repacked, "ignored").unwrap();
        let altered_print = fingerprint(&altered, "ignored").unwrap();

        assert_eq!(original_print.digest, repacked_print.digest);
        assert_ne!(original_print.digest, altered_print.digest);
        assert_eq!(original_print.tracks, vec!["01 One.mp3", "02 Two.mp3"]);
        assert!(original_print.is_archive);
    }

    #[test]
    fn fingerprints_a_bare_track_under_the_name_it_will_be_saved_as() {
        let path = scratch("track.mp3.part");
        File::create(&path).unwrap().write_all(b"not a zip").unwrap();

        let fingerprinted = fingerprint(&path, "Artist - Track.mp3").unwrap();

        assert_eq!(fingerprinted.digest.len(), 64);
        assert!(!fingerprinted.is_archive);
        // The temporary name must not enter the record
        assert_eq!(fingerprinted.tracks, vec!["Artist - Track.mp3"]);
    }

    #[test]
    fn a_damaged_archive_is_reported_as_not_being_one() {
        let path = scratch("truncated.zip");
        File::create(&path).unwrap().write_all(b"PK\x03\x04 truncated rubbish").unwrap();

        let fingerprinted = fingerprint(&path, "Artist - Album.zip").unwrap();

        // The tool can hash the file, but it did not read it as an archive
        assert!(!fingerprinted.is_archive);
        assert!(looks_like_archive(&path));
    }

    #[test]
    fn remembers_downloads_across_a_save_and_load() {
        let path = scratch("manifest.json");
        let mut manifest = Manifest::default();

        manifest.record("123", "flac", DownloadRecord {
            filename: "Artist - Album.zip".into(),
            filesize: 1234,
            fingerprint: "abc".into(),
            tracks: vec!["01 One.flac".into()],
            is_archive: true,
            downloaded_at: Utc::now(),
        });

        manifest.save(&path).unwrap();

        let reloaded = Manifest::load(&path).unwrap();
        let record = reloaded.get("123", "flac").unwrap();

        assert_eq!(record.filename, "Artist - Album.zip");
        assert_eq!(record.filesize, 1234);
        // The record does not contain a format that you did not download
        assert!(reloaded.get("123", "mp3-320").is_none());
    }

    #[test]
    fn a_missing_manifest_loads_as_an_empty_one() {
        let manifest = Manifest::load(&scratch("absent.json")).unwrap();

        assert_eq!(manifest.version, MANIFEST_VERSION);
        assert!(manifest.items.is_empty());
    }
}
