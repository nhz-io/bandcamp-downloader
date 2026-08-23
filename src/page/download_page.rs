use crate::page::page::Page;
use clap::ValueEnum;
use serde::Deserialize;
use std::collections::HashMap;

/// The formats in the `downloads` object of the download page data blob
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum DownloadFormat {
    Flac,
    #[value(name = "mp3-320")]
    Mp3320,
    #[value(name = "mp3-v0")]
    Mp3V0,
    #[value(name = "aac-hi")]
    AacHi,
    #[value(name = "aiff-lossless")]
    AiffLossless,
    Alac,
    Vorbis,
    Wav,
}

impl DownloadFormat {
    /// The key of this format in the `downloads` object
    pub fn as_key(&self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Mp3320 => "mp3-320",
            Self::Mp3V0 => "mp3-v0",
            Self::AacHi => "aac-hi",
            Self::AiffLossless => "aiff-lossless",
            Self::Alac => "alac",
            Self::Vorbis => "vorbis",
            Self::Wav => "wav",
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct DownloadItem {
    pub url: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct DownloadItems {
    pub downloads: Option<HashMap<String, DownloadItem>>,
}

#[derive(Deserialize, Debug)]
pub struct DownloadPageData {
    pub download_items: Option<Vec<DownloadItems>>,
}

pub type DownloadPage = Page<DownloadPageData>;
