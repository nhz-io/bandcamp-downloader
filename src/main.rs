use crate::client::init_client;
use crate::collection::collection_iterator::CollectionIterator;
use crate::collection::traits::{CollectionItem as CollectionItemTrait, CollectionIterator as CollectionIteratorTrait};
use crate::collection::metadata::ItemMetadata;
use crate::cookies::find_all;
use crate::filter::Filters;
use crate::page::download_page::DownloadFormat;
use crate::manifest::{fingerprint, ItemRecord, looks_like_archive, sale_item_id, superseded_path, verify_archive, DownloadRecord, Fingerprinted, Manifest, MANIFEST_FILENAME, VERSIONS_DIRNAME};
use crate::pace::{set_interval, succeeded, DEFAULT_INTERVAL_MS};
use crate::retry::{is_terminal, needs_relink, with_retry};
use bytes::Bytes;
use chrono::Utc;
use clap::{Args, Parser, Subcommand};
use futures::{Stream, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use std::env;
use std::error::Error;
use std::fs::{create_dir_all, metadata, remove_file, rename, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;


mod page;
mod client;
mod collection;
mod cookies;
mod filter;
mod manifest;
mod pace;
mod retry;
mod stat_download;

#[derive(Parser, Debug)]
#[clap(
    version,
    about = "Download and keep a Bandcamp collection",
    long_about = "\
This tool downloads the music that you bought on Bandcamp. It keeps a record of the
music with the files. Subsequent runs read this record and know which files they have.

The tool does not replace a file that it downloaded before. If Bandcamp changes an
album, the tool moves your copy to .versions/, keeps it, and reports the change. The
tool checks each archive against the length that the server sent, before it gives the
file its correct name. If a download stops, the tool continues it. It does not start
the download again.",
    after_help = "\
EXAMPLES:
  bcdl                                 download all music, log in from your browser
  bcdl -f mp3-320 -o ~/Music/bandcamp  use a different format and directory
  bcdl list ott                        show the items that match 'ott'
  bcdl list --since 3m                 show the items that you bought in 3 months
  bcdl download --artist ott           download the music of one artist only
  bcdl verify                          check the files against the record, offline
  bcdl diff blumenkraft                show the changes between the kept versions

Bandcamp does not offer all albums in all formats. The tool reports an album that it
cannot get in the selected format, then continues."
)]
struct Cli {
    #[clap(subcommand)]
    command: Option<Command>,

    #[clap(flatten)]
    options: Options,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Download the music that you do not have (the default command)
    Download,

    /// Show the items in the collection. Do not download them.
    List {
        /// Select the items that contain this text in the artist or album name
        query: Option<String>,

        /// Print the output as json
        #[clap(long)]
        json: bool,
    },

    /// Compare the files with the record. Do not use the network.
    Verify,

    /// Show the changes between the kept versions of an album
    Diff {
        /// Select the items that contain this text in the artist or album name
        query: Option<String>,
    },
}

#[derive(Args, Debug)]
struct Options {
    /// The Bandcamp identity token
    #[clap(
        short, long, global = true, env = "BANDCAMP_IDENTITY", hide_env_values = true,
        long_help = "\
The Bandcamp identity token.

Usually you do not need this option. The tool reads the token from a browser that is
logged in. It reads Firefox first, then Chrome. It examines all profiles and selects the
login that you used last. If the tool selects the wrong login, use --browser or
--profile.

Give the token here only if the computer has no browser, for example a server. You can
also set BANDCAMP_IDENTITY, which keeps the token out of your shell history."
    )]
    identity: Option<String>,

    /// Read the token from this browser only (firefox, chrome, edge, brave, vivaldi)
    #[clap(long, global = true)]
    browser: Option<String>,

    /// Read the token from the profiles that contain this text in the name
    #[clap(long, global = true)]
    profile: Option<String>,

    /// The audio format to download
    #[clap(short, long, value_enum, default_value = "flac", global = true)]
    format: DownloadFormat,

    /// The directory for the music and the record (the current directory is the default)
    #[clap(short, long, global = true)]
    output: Option<PathBuf>,

    /// Download the items that you have, to find the changes on Bandcamp
    #[clap(long, global = true)]
    recheck: bool,

    /// The milliseconds between requests, so that a long run does not look automatic
    #[clap(long, default_value_t = DEFAULT_INTERVAL_MS, global = true)]
    delay: u64,

    /// Select the albums of the artists that contain this text in the name
    #[clap(long, global = true)]
    artist: Option<String>,

    /// Select the albums that contain this text in the title
    #[clap(long, global = true)]
    album: Option<String>,

    /// Select the items that you bought after this: 7d, 2w, 3m, 1y, or a date
    #[clap(long, global = true)]
    since: Option<String>,

    /// Select the items that you bought before this: 7d, 2w, 3m, 1y, or a date
    #[clap(long, global = true)]
    until: Option<String>,
}

/// The results of the run. The tool prints this one time at the end, thus you do not
/// have to read all the output.
#[derive(Default)]
struct Summary {
    downloaded: usize,
    skipped: usize,
    adopted: usize,
    changed: Vec<String>,
    failed: Vec<String>,
    relink: Vec<String>,
    unavailable: Vec<String>,
    stopped_early: Option<String>,
}

impl Summary {
    fn record_failure(&mut self, label: &str, url: &str, error: &Box<dyn Error>) {
        // Show a link that is not valid together with the page that supplies a new one.
        // The operator can then do this.
        if needs_relink(error) {
            self.relink.push(format!("{}\n      {}", label, url));
            return;
        }

        let entry = format!("{}: {}", label, error);

        // The items that Bandcamp refuses are the important ones to record
        match is_terminal(error) {
            true => self.unavailable.push(entry),
            _ => self.failed.push(entry),
        }
    }

    fn render(&self) -> String {
        let rule = "-".repeat(60);

        let mut report = format!("\n{}\nDownloaded: {}\nAlready on the disk: {}\n", rule, self.downloaded, self.skipped);

        if self.adopted > 0 {
            report += &format!("Files kept from the disk: {}\n", self.adopted);
        }

        if !self.changed.is_empty() {
            report += &format!("\nCHANGED ON BANDCAMP ({}). Previous copies are in {}/:\n", self.changed.len(), VERSIONS_DIRNAME);
            for entry in &self.changed {
                report += &format!("  {}\n", entry);
            }
        }

        if !self.failed.is_empty() {
            report += &format!("\nFailed after the retries ({}). Run the tool again for these:\n", self.failed.len());
            for entry in &self.failed {
                report += &format!("  {}\n", entry);
            }
        }

        if !self.relink.is_empty() {
            report += &format!("\nNEED A NEW LINK ({}). Open each page and request a link by email,\nthen run the tool again:\n", self.relink.len());
            for entry in &self.relink {
                report += &format!("  {}\n", entry);
            }
        }

        if !self.unavailable.is_empty() {
            report += &format!("\nNot available on Bandcamp ({}). A subsequent run cannot get these:\n", self.unavailable.len());
            for entry in &self.unavailable {
                report += &format!("  {}\n", entry);
            }
        }

        if let Some(reason) = &self.stopped_early {
            report += &format!("\nStopped before the end of the collection: {}\n", reason);
            report += "The tool did not examine all items.\n";
        }

        report + &rule
    }

    fn print(&self) {
        println!("{}", self.render());
    }

    /// A problem that a subsequent run can correct is an error for the caller. The tool
    /// reports the items that Bandcamp never supplies, but they are not an error, because
    /// no run can correct them.
    fn exit_code(&self) -> i32 {
        // A link that is not valid is not a failed run. It is a task for the operator.
        match self.stopped_early.is_some() || !self.failed.is_empty() {
            true => 1,
            _ => 0,
        }
    }
}

/// The token to use. If you do not give one, the tool reads it from a browser.
///
/// More than one profile can be logged in, and to different accounts. Thus the tool
/// always prints its selection.
fn resolve_identity(options: &Options) -> Result<String, Box<dyn Error>> {
    if let Some(identity) = &options.identity {
        return Ok(identity.clone());
    }

    let found = find_all(options.browser.as_deref(), options.profile.as_deref());

    let using = match found.first() {
        Some(using) => using,
        _ => return Err("No Bandcamp login found in a browser. Log in at bandcamp.com, or give the token.".into()),
    };

    match found.len() {
        1 => println!("Use the Bandcamp login from {}", using.describe()),
        more => {
            println!("Found a Bandcamp login in {} profiles:", more);

            for cookie in &found {
                println!("  {}", cookie.describe());
            }

            println!("Use {}, the most recent one. To select a different one, use --browser or --profile.", using.describe());
        }
    }

    Ok(using.value.clone())
}

fn check_if_exists(path_buf: &PathBuf) -> bool {
    match metadata(path_buf) {
        Ok(_) => true,
        _ => false
    }
}

/// The change that Bandcamp made to an album between two downloads
enum Change {
    /// The same music in a new archive. Nothing is lost.
    Repacked,
    /// Bandcamp added, removed, or replaced tracks. This is the important change.
    TracksChanged { gone: Vec<String>, added: Vec<String> },
}

fn classify(previous: &[String], current: &[String]) -> Change {
    let gone: Vec<String> = previous.iter().filter(|t| !current.contains(t)).cloned().collect();
    let added: Vec<String> = current.iter().filter(|t| !previous.contains(t)).cloned().collect();

    match gone.is_empty() && added.is_empty() {
        true => Change::Repacked,
        _ => Change::TracksChanged { gone, added },
    }
}

fn report(change: &Change, filename: &str) {
    match change {
        Change::Repacked => println!(
            "Bandcamp built {} again. The tracks are the same. Keep both copies.", filename),

        Change::TracksChanged { gone, added } => {
            println!("\n!! {} CHANGED ON BANDCAMP", filename);

            for track in gone {
                println!("   gone:  {}", track);
            }

            for track in added {
                println!("   added: {}", track);
            }

            println!("   Your copy stays in {}/\n", VERSIONS_DIRNAME);
        }
    }
}

/// True if the tool can use a file that is already here as a complete download.
///
/// An interrupted run can leave a file that looks correct but is not. If the tool used
/// that file, it would record a damaged copy as the correct one and never read it again.
fn is_trustworthy(path: &PathBuf, fingerprinted: &Fingerprinted, expected_size: usize) -> bool {
    match looks_like_archive(path) {
        // A truncated archive loses the index at its end, so opening it is the test
        true => fingerprinted.is_archive,
        // Nothing to look inside, so the promised size is all there is to go on
        _ => metadata(path).map(|m| m.len()).unwrap_or(0) == expected_size as u64,
    }
}

/// Move the copy that is already here into the version history. Nothing is ever
/// deleted, so a replaced album can always be recovered.
fn set_aside(destination: &PathBuf, root: &Path, record: &DownloadRecord) -> Result<(), Box<dyn Error>> {
    let archived = superseded_path(root, record);

    if let Some(parent) = archived.parent() {
        create_dir_all(parent)?;
    }

    println!("Keep the previous copy at: {}", archived.to_str().unwrap());

    Ok(rename(destination, &archived)?)
}

/// Path the download is streamed to before it is known to be complete
fn part_path(path_buf: &PathBuf) -> PathBuf {
    let filename = path_buf.file_name().unwrap().to_string_lossy().to_string();

    let mut part_path_buf = path_buf.clone();
    part_path_buf.set_file_name(filename + ".part");

    part_path_buf
}

/// Stream to the partial file, then verify it is whole before claiming the real name
async fn download_to_part<S>(part: &PathBuf, destination: &PathBuf, filesize: usize, content_length: Option<u64>, resumed_from: u64, stream: &mut S) -> Result<(), Box<dyn Error>>
where
    S: Stream<Item=Result<Bytes, reqwest::Error>> + Unpin,
{
    // Appending is the whole point of resuming, and truncating here would silently
    // throw away everything the previous attempt managed to fetch
    let mut file = match resumed_from > 0 {
        true => OpenOptions::new().append(true).open(part)?,
        _ => File::create(part)?,
    };

    let pb = ProgressBar::new(content_length.unwrap_or(filesize as u64));

    match resumed_from > 0 {
        true => println!("Continue at {} bytes: {}", resumed_from, destination.to_str().unwrap()),
        _ => println!("Download: {}", destination.to_str().unwrap()),
    }

    pb.set_style(
        ProgressStyle::default_bar()
            .template("{msg} [{bar:40}] {bytes}/{total_bytes} ({percent}%) ETA: {eta}")?
            .progress_chars("##-"));

    // Counts the whole file, not just this response, so it is comparable to the total
    let mut downloaded: u64 = resumed_from;

    while let Some(item) = stream.next().await {
        let chunk = item?;
        file.write_all(&chunk)?;
        downloaded += chunk.len() as u64;
        pb.set_position(downloaded);
    }

    // Get the bytes onto the disk before the size is trusted
    file.flush()?;
    drop(file);

    // A truncated stream is the usual way a download goes wrong, so refuse to publish
    // anything short. The length is taken from this very response rather than an earlier
    // probe, so a rebuilt archive cannot make a complete download look incomplete
    match content_length {
        Some(expected) if downloaded != expected =>
            return Err(format!("Incomplete download: {} bytes received, {} bytes expected", downloaded, expected).into()),
        None => eprintln!("Warning: no Content-Length for {}. Cannot check that it is complete.", destination.to_str().unwrap()),
        _ => ()
    }

    // Joined in the middle, so the contents are checked rather than assumed. A poisoned
    // partial file is removed outright, since resuming it again would repeat the fault.
    if resumed_from > 0 && looks_like_archive(destination) {
        if let Err(e) = verify_archive(part) {
            let _ = remove_file(part);
            return Err(format!("The continued file failed the check. Start again: {}", e).into());
        }
    }

    pb.finish_with_message("Download complete");

    Ok(())
}

/// Downloads and verifies, leaving the result under its partial name. The caller
/// decides what happens to whatever already holds the real name, so a finished
/// download can never silently replace a copy that is already there.
async fn download_file<S>(destination: &PathBuf, filesize: usize, content_length: Option<u64>, resumed_from: u64, stream: &mut S) -> Result<PathBuf, Box<dyn Error>>
where
    S: Stream<Item=Result<Bytes, reqwest::Error>> + Unpin,
{
    let part = part_path(destination);

    // What was fetched is deliberately left in place on failure, so the next attempt
    // carries on from it rather than starting a large download over again
    download_to_part(&part, destination, filesize, content_length, resumed_from, stream).await?;

    Ok(part)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retry::TerminalError;

    fn fingerprinted(is_archive: bool) -> Fingerprinted {
        Fingerprinted { digest: "abc".into(), tracks: vec!["01 One.flac".into()], is_archive }
    }

    #[test]
    fn an_unreadable_archive_is_never_taken_at_face_value() {
        let archive = PathBuf::from("/tmp/Artist - Album.zip");

        // Opened cleanly, so what is here can be believed
        assert!(is_trustworthy(&archive, &fingerprinted(true), 0));
        // Present but unreadable, which is what a half finished run leaves behind
        assert!(!is_trustworthy(&archive, &fingerprinted(false), 0));
    }

    #[test]
    fn a_bare_track_is_judged_on_its_size() {
        let track = std::env::temp_dir().join("bcdl-trust-test.flac");
        std::fs::write(&track, b"0123456789").unwrap();

        assert!(is_trustworthy(&track, &fingerprinted(false), 10));
        // Short of what the server promised, so it is still a partial download
        assert!(!is_trustworthy(&track, &fingerprinted(false), 999));

        let _ = std::fs::remove_file(&track);
    }

    #[test]
    fn repacking_the_same_tracks_is_not_a_change() {
        let before = vec!["01 One.flac".to_string(), "02 Two.flac".to_string()];
        // Same tracks, listed the other way round
        let after = vec!["02 Two.flac".to_string(), "01 One.flac".to_string()];

        assert!(matches!(classify(&before, &after), Change::Repacked));
    }

    #[test]
    fn a_dropped_track_is_reported_as_gone() {
        let before = vec!["01 One.flac".to_string(), "02 Two.flac".to_string()];
        let after = vec!["01 One.flac".to_string()];

        match classify(&before, &after) {
            Change::TracksChanged { gone, added } => {
                assert_eq!(gone, vec!["02 Two.flac"]);
                assert!(added.is_empty());
            }
            _ => panic!("a missing track must not be treated as a repack"),
        }
    }

    #[test]
    fn a_replaced_track_shows_both_sides() {
        let before = vec!["01 One.flac".to_string(), "02 Original.flac".to_string()];
        let after = vec!["01 One.flac".to_string(), "02 Replacement.flac".to_string()];

        match classify(&before, &after) {
            Change::TracksChanged { gone, added } => {
                assert_eq!(gone, vec!["02 Original.flac"]);
                assert_eq!(added, vec!["02 Replacement.flac"]);
            }
            _ => panic!("a replaced track must be reported"),
        }
    }

    #[test]
    fn changed_albums_are_called_out_in_the_summary() {
        let summary = Summary { changed: vec!["Ott - Blumenkraft.zip".into()], ..Default::default() };

        let report = summary.render();

        assert!(report.contains("CHANGED ON BANDCAMP (1)"));
        assert!(report.contains("Ott - Blumenkraft.zip"));
        assert!(report.contains(VERSIONS_DIRNAME));
    }

    #[test]
    fn a_stale_link_is_listed_with_the_page_that_reissues_it() {
        let mut summary = Summary::default();

        let expired: Box<dyn Error> = TerminalError::needs_relink("link expired (ExpirationError)").into();
        summary.record_failure("DubPuffin - 3 Years at Sea", "https://bandcamp.com/download?sitem_id=9", &expired);

        // It is neither a lost cause nor a failed run, so it belongs in neither list
        assert!(summary.unavailable.is_empty());
        assert!(summary.failed.is_empty());
        assert_eq!(summary.exit_code(), 0);

        let report = summary.render();

        assert!(report.contains("NEED A NEW LINK (1)"));
        assert!(report.contains("DubPuffin - 3 Years at Sea"));
        // The page has to be there, since asking for the new link is done by hand
        assert!(report.contains("https://bandcamp.com/download?sitem_id=9"));
    }

    #[test]
    fn separates_what_is_gone_from_what_merely_failed() {
        let mut summary = Summary::default();

        let transient: Box<dyn Error> = "connection reset".into();
        let terminal: Box<dyn Error> = TerminalError::new("no longer available on Bandcamp (DeletedError)").into();

        summary.record_failure("Some Album.zip", "https://bandcamp.com/download?sitem_id=1", &transient);
        summary.record_failure("Gone Album.zip", "https://bandcamp.com/download?sitem_id=2", &terminal);

        assert_eq!(summary.failed, vec!["Some Album.zip: connection reset"]);
        assert_eq!(summary.unavailable, vec!["Gone Album.zip: no longer available on Bandcamp (DeletedError)"]);
    }

    #[test]
    fn reports_counts_and_both_kinds_of_problem() {
        let mut summary = Summary { downloaded: 3, skipped: 7, ..Default::default() };

        let terminal: Box<dyn Error> = TerminalError::new("no longer available on Bandcamp (DeletedError)").into();
        summary.record_failure("Gone Album.zip", "https://bandcamp.com/download?sitem_id=2", &terminal);

        let report = summary.render();

        assert!(report.contains("Downloaded: 3"));
        assert!(report.contains("Already on the disk: 7"));
        assert!(report.contains("Not available on Bandcamp (1)"));
        assert!(report.contains("Gone Album.zip: no longer available on Bandcamp (DeletedError)"));
        // Nothing failed transiently, so that section stays out of the way
        assert!(!report.contains("Failed after the retries"));
    }

    #[test]
    fn fails_the_exit_code_only_for_problems_a_rerun_could_fix() {
        let clean = Summary { downloaded: 3, ..Default::default() };
        assert_eq!(clean.exit_code(), 0);

        let mut gone = Summary { downloaded: 3, ..Default::default() };
        let terminal: Box<dyn Error> = TerminalError::new("no longer available").into();
        gone.record_failure("Gone.zip", "https://bandcamp.com/download?sitem_id=3", &terminal);
        // Reported, but no future run can change it
        assert_eq!(gone.exit_code(), 0);

        let mut retryable = Summary::default();
        let transient: Box<dyn Error> = "connection reset".into();
        retryable.record_failure("Some.zip", "https://bandcamp.com/download?sitem_id=4", &transient);
        assert_eq!(retryable.exit_code(), 1);

        let stopped = Summary { stopped_early: Some("connection refused".into()), ..Default::default() };
        assert_eq!(stopped.exit_code(), 1);
    }

    #[test]
    fn says_so_when_the_collection_ran_out_early() {
        let summary = Summary { stopped_early: Some("connection refused".into()), ..Default::default() };

        let report = summary.render();

        assert!(report.contains("Stopped before the end of the collection: connection refused"));
        assert!(report.contains("The tool did not examine all items."));
    }
}

/// Fetch everything the filters allow that is not already held
async fn download(options: &Options, filters: &Filters, root: &PathBuf) -> Result<i32, Box<dyn Error>> {
    let client = init_client(&resolve_identity(options)?);
    let mut collection_iterator = CollectionIterator::new(&client, options.format);
    let mut summary = Summary::default();

    // Kept with the downloads, so a library remembers what it holds between runs
    let manifest_path = root.join(MANIFEST_FILENAME);
    let mut manifest = Manifest::load(&manifest_path)?;
    let format = options.format.as_key();
    let mut seen = 0;

    loop {
        // A collection fetch that will not recover leaves nothing left to enumerate,
        // so stop walking but still report on everything already done
        let item = match collection_iterator.next().await {
            Ok(Some(item)) => item,
            Ok(None) => break,
            Err(e) => {
                eprintln!("Cannot read more of the collection: {}", e);
                summary.stopped_early = Some(e.to_string());
                break;
            }
        };

        seen += 1;

        // Each album shows its own progress, but not how far through the collection it is
        let position = match collection_iterator.total() {
            Some(total) => format!("[{}/{}]", seen, total),
            _ => format!("[{}]", seen),
        };

        let sale_item_id = sale_item_id(item.get_url());
        let label = item.label();

        // Enumerating the collection is cheap, so narrowing happens here rather than
        // by asking Bandcamp for anything about an item that was not asked for
        if !filters.matches(item.metadata()) {
            continue;
        }

        // Written down while Bandcamp still describes it, so a purchase it later
        // refuses to serve is still recognisable in the manifest
        if let (Some(id), Some(metadata)) = (&sale_item_id, item.metadata()) {
            manifest.describe(id, metadata);
        }

        // Answered from what is already on disk, so a held item costs no requests
        // and Bandcamp is not asked to package an archive nobody will download.
        // A recheck is exactly the request to look anyway, so it skips this.
        if let Some(id) = &sale_item_id.clone().filter(|_| !options.recheck) {
            if let Some(record) = manifest.get(id, format) {
                if check_if_exists(&root.join(&record.filename)) {
                    println!("{} you have this file: {}", position, record.filename);
                    summary.skipped += 1;
                    manifest.mark_seen(id);
                    continue;
                }
            }
        }

        let item = &item;
        let client = &client;

        // Resolved once so the probe below and the download itself describe the same archive
        let download_url = match with_retry("Preparing archive", move || async move {
            item.get_download_url(client).await
        }).await {
            Ok(download_url) => download_url,
            Err(e) => {
                eprintln!("{} cannot get {}: {}", position, label, e);
                summary.record_failure(&label, item.get_url().as_str(), &e);
                continue;
            }
        };

        let download_url = &download_url;

        let item_info = match with_retry("Download info", move || async move {
            item.get_item_info(client, download_url).await
        }).await {
            Ok(item_info) => item_info,
            Err(e) => {
                eprintln!("{} cannot get {}: {}", position, label, e);
                summary.record_failure(&label, item.get_url().as_str(), &e);
                continue;
            }
        };

        let destination = root.join(&item_info.filename);

        let known = sale_item_id.as_ref().and_then(|id| manifest.get(id, format)).is_some();

        // A file the manifest has never seen is taken at face value rather than fetched
        // again, so a library downloaded before any of this still costs nothing
        if !options.recheck && !known && check_if_exists(&destination) {
            match fingerprint(&destination, &item_info.filename) {
                Ok(existing) if is_trustworthy(&destination, &existing, item_info.filesize) => {
                    println!("{} the file is here already: {}", position, &item_info.filename);

                    if let Some(id) = &sale_item_id {
                        manifest.record(id, format, DownloadRecord {
                            filename: item_info.filename.clone(),
                            filesize: metadata(&destination).map(|m| m.len()).unwrap_or(0),
                            fingerprint: existing.digest,
                            tracks: existing.tracks,
                            is_archive: existing.is_archive,
                            downloaded_at: Utc::now(),
                        });
                    }

                    summary.adopted += 1;
                    continue;
                }
                // Left over from an interrupted run, so fetch it properly rather than
                // recording damage as the copy of record
                Ok(_) => println!("{} the file {} is not complete. Download it again.", position, &item_info.filename),
                Err(e) => eprintln!("Warning: cannot read the file {}: {}", &item_info.filename, e),
            }
        }

        let destination = &destination;
        let filesize = item_info.filesize;

        // The download lands under its partial name, so nothing here is at risk yet
        let part = match with_retry("Download", move || async move {
            // Whatever a previous attempt left behind is carried on from
            let resume_from = metadata(part_path(destination)).map(|m| m.len()).unwrap_or(0);

            let (content_length, resumed_from, stream) = item.get_download_stream(client, download_url, resume_from).await?;
            let mut stream = stream;

            download_file(destination, filesize, content_length, resumed_from, &mut stream).await
        }).await {
            Ok(part) => part,
            Err(e) => {
                eprintln!("Cannot get {}: {}", &item_info.filename, e);
                summary.record_failure(&item_info.filename, item.get_url().as_str(), &e);
                continue;
            }
        };

        let arrived = fingerprint(&part, &item_info.filename);

        // Something that should be an archive but cannot be opened is a damaged
        // download, not a new version, and must never displace a good copy
        if arrived.as_ref().map(|a| looks_like_archive(destination) && !a.is_archive).unwrap_or(false) {
            let e: Box<dyn Error> = format!("the tool cannot open the file as an archive").into();
            eprintln!("Delete {}: {}", &item_info.filename, e);
            let _ = remove_file(&part);
            summary.record_failure(&item_info.filename, item.get_url().as_str(), &e);
            continue;
        }

        // Whatever already holds this name is described and moved into the version
        // history first. The new file never takes the name until the old one is safe.
        if check_if_exists(destination) {
            let previous = match sale_item_id.as_ref().and_then(|id| manifest.get(id, format)).cloned() {
                Some(record) => Some(record),
                _ => fingerprint(destination, &item_info.filename).ok().map(|existing| DownloadRecord {
                    filename: item_info.filename.clone(),
                    filesize: metadata(destination).map(|m| m.len()).unwrap_or(0),
                    fingerprint: existing.digest,
                    tracks: existing.tracks,
                    is_archive: existing.is_archive,
                    downloaded_at: Utc::now(),
                }),
            };

            let previous = match previous {
                Some(previous) => previous,
                _ => {
                    eprintln!("Cannot read the file {}. Do not change it.", &item_info.filename);
                    let _ = remove_file(&part);
                    continue;
                }
            };

            // Identical content, so discard the copy just fetched rather than the one held
            if arrived.as_ref().map(|a| a.digest == previous.fingerprint).unwrap_or(false) {
                println!("Unchanged on Bandcamp, keeping what is here: {}", &item_info.filename);
                let _ = remove_file(&part);

                if let Some(id) = &sale_item_id {
                    manifest.record(id, format, previous);
                }

                summary.skipped += 1;
                continue;
            }

            if let Ok(arrived) = &arrived {
                let change = classify(&previous.tracks, &arrived.tracks);

                report(&change, &item_info.filename);

                if let Change::TracksChanged { .. } = change {
                    summary.changed.push(item_info.filename.clone());
                }
            }

            if let Err(e) = set_aside(destination, root, &previous) {
                eprintln!("Cannot keep the copy of {}: {}", &item_info.filename, e);
                summary.record_failure(&item_info.filename, item.get_url().as_str(), &e);
                continue;
            }

            if let Some(id) = &sale_item_id {
                manifest.supersede(id, format, previous);
            }
        }

        if let Err(e) = rename(&part, destination) {
            let e: Box<dyn Error> = e.into();
            eprintln!("Cannot give {} its correct name: {}", &item_info.filename, e);
            summary.record_failure(&item_info.filename, item.get_url().as_str(), &e);
            continue;
        }

        summary.downloaded += 1;

        // A whole file arrived, so let the pace ease back off if it had been slowed
        succeeded();

        // Recorded against what the archive holds, so the next run knows this is the
        // same music even if Bandcamp packs it differently
        match (&sale_item_id, arrived) {
            (Some(id), Ok(arrived)) => {
                manifest.record(id, format, DownloadRecord {
                    filename: item_info.filename.clone(),
                    filesize: metadata(destination).map(|m| m.len()).unwrap_or(item_info.filesize as u64),
                    fingerprint: arrived.digest,
                    tracks: arrived.tracks,
                    is_archive: arrived.is_archive,
                    downloaded_at: Utc::now(),
                });

                // Written per item so an interrupted run keeps everything it earned
                if let Err(e) = manifest.save(&manifest_path) {
                    eprintln!("Warning: cannot write the record: {}", e);
                }
            }
            (_, Err(e)) => eprintln!("Warning: cannot calculate the fingerprint of {}: {}", &item_info.filename, e),
            _ => ()
        }
    };

    if let Err(e) = manifest.save(&manifest_path) {
        eprintln!("Warning: cannot write the record: {}", e);
    }

    summary.print();

    Ok(summary.exit_code())
}

/// How an item reads once Bandcamp is no longer around to describe it
fn describe(id: &str, item: &ItemRecord) -> String {
    match (&item.artist, &item.title) {
        (Some(artist), Some(title)) => format!("{} - {}", artist, title),
        (_, Some(title)) => title.clone(),
        _ => id.to_string(),
    }
}

/// What a record says, in the shape the filters understand
fn as_metadata(item: &ItemRecord) -> ItemMetadata {
    ItemMetadata {
        sale_item_id: None,
        sale_item_type: None,
        band_name: item.artist.clone(),
        item_title: item.title.clone(),
        purchased: item.purchased.clone(),
        item_url: item.item_url.clone(),
    }
}

/// Show the collection without fetching any of it
async fn list(options: &Options, filters: &Filters, json: bool) -> Result<i32, Box<dyn Error>> {
    let client = init_client(&resolve_identity(options)?);
    let mut collection_iterator = CollectionIterator::new(&client, options.format);

    let mut entries: Vec<String> = Vec::new();
    let mut shown = 0;
    let mut total = 0;

    loop {
        let item = match collection_iterator.next().await {
            Ok(Some(item)) => item,
            Ok(None) => break,
            Err(e) => {
                eprintln!("Cannot read more of the collection: {}", e);
                break;
            }
        };

        total += 1;

        if !filters.matches(item.metadata()) {
            continue;
        }

        shown += 1;

        let purchased = item.metadata().and_then(|m| m.purchased.clone()).unwrap_or_default();

        match json {
            true => entries.push(serde_json::json!({
                "artist": item.metadata().and_then(|m| m.band_name.clone()),
                "title": item.metadata().and_then(|m| m.item_title.clone()),
                "purchased": item.metadata().and_then(|m| m.purchased.clone()),
                "url": item.metadata().and_then(|m| m.item_url.clone()),
            }).to_string()),

            // Just the date, the time it was bought at is noise
            _ => println!("{:<12} {}", purchased.split_whitespace().take(3).collect::<Vec<_>>().join(" "), item.label()),
        }
    }

    match json {
        true => println!("[{}]", entries.join(",")),
        _ => println!("\n{} of {} items", shown, total),
    }

    Ok(0)
}

/// Check the library against its own record, without asking Bandcamp anything.
///
/// This is what catches a file quietly rotting on disk, which no amount of care at
/// download time can prevent.
fn verify(root: &PathBuf, filters: &Filters) -> Result<i32, Box<dyn Error>> {
    let manifest = Manifest::load(&root.join(MANIFEST_FILENAME))?;

    let mut checked = 0;
    let mut missing = Vec::new();
    let mut altered = Vec::new();

    for (id, item) in &manifest.items {
        if !filters.matches(Some(&as_metadata(item))) {
            continue;
        }

        for (format, record) in &item.downloads {
            let label = format!("{} [{}]", describe(id, item), format);
            let path = root.join(&record.filename);

            if !check_if_exists(&path) {
                missing.push(label);
                continue;
            }

            checked += 1;

            match fingerprint(&path, &record.filename) {
                Ok(found) if found.digest == record.fingerprint => (),
                Ok(_) => altered.push(format!("{}: the contents do not agree with the record", label)),
                Err(e) => altered.push(format!("{}: {}", label, e)),
            }
        }
    }

    println!("\n{}", "-".repeat(60));
    println!("Checked: {}", checked);

    if !missing.is_empty() {
        println!("\nIn the record but not on the disk ({}):", missing.len());
        for entry in &missing {
            println!("  {}", entry);
        }
    }

    if !altered.is_empty() {
        println!("\nDIFFERENT FROM THE RECORD ({}):", altered.len());
        for entry in &altered {
            println!("  {}", entry);
        }
    }

    if missing.is_empty() && altered.is_empty() {
        println!("All files agree with the record.");
    }

    println!("{}", "-".repeat(60));

    Ok(match altered.is_empty() && missing.is_empty() {
        true => 0,
        _ => 1,
    })
}

/// Show what Bandcamp changed between the versions that were kept
fn diff(root: &PathBuf, filters: &Filters) -> Result<i32, Box<dyn Error>> {
    let manifest = Manifest::load(&root.join(MANIFEST_FILENAME))?;

    let mut found = 0;

    for (id, item) in &manifest.items {
        if !filters.matches(Some(&as_metadata(item))) {
            continue;
        }

        for (format, older) in &item.superseded {
            let current = match item.downloads.get(format) {
                Some(current) => current,
                _ => continue,
            };

            // Oldest first, each compared with what replaced it
            for previous in older {
                found += 1;

                println!("\n{} [{}]", describe(id, item), format);
                println!("  kept    {} ({})", previous.filename, previous.downloaded_at.format("%Y-%m-%d"));
                println!("  current {} ({})", current.filename, current.downloaded_at.format("%Y-%m-%d"));

                match classify(&previous.tracks, &current.tracks) {
                    Change::Repacked => println!("  the same tracks in a new archive"),
                    Change::TracksChanged { gone, added } => {
                        for track in gone {
                            println!("  gone:  {}", track);
                        }

                        for track in added {
                            println!("  added: {}", track);
                        }
                    }
                }
            }
        }
    }

    match found {
        0 => println!("No version was replaced. There is nothing to compare."),
        _ => println!("\n{} replaced version(s).", found),
    }

    Ok(0)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let options = &cli.options;

    set_interval(options.delay);

    let root = match &options.output {
        Some(output) => output.clone(),
        _ => env::current_dir()?,
    };

    create_dir_all(&root)?;

    // The bare query only belongs to the commands that take one
    let query = match &cli.command {
        Some(Command::List { query, .. }) => query.clone(),
        Some(Command::Diff { query }) => query.clone(),
        _ => None,
    };

    let filters = Filters::build(
        query,
        options.artist.clone(),
        options.album.clone(),
        options.since.clone(),
        options.until.clone(),
    )?;

    let code = match &cli.command {
        Some(Command::List { json, .. }) => list(options, &filters, *json).await?,
        Some(Command::Verify) => verify(&root, &filters)?,
        Some(Command::Diff { .. }) => diff(&root, &filters)?,
        // Downloading is the point, so it is what happens when nothing else is asked for
        _ => download(options, &filters, &root).await?,
    };

    exit(code)
}
