mod gpod;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::prelude::Accessor;
use lofty::probe::Probe;
use walkdir::WalkDir;

use crate::gpod::{Database, Metadata, Track};

#[derive(Debug, Parser)]
#[command(
    name = "copyPod",
    version,
    about = "Mirror one or more local MP3 folders to a classic iPod",
    after_help = "WARNING: every iPod track not present in the supplied libraries is deleted.\nRun with --dry-run first if you are unsure."
)]
struct Cli {
    /// Local library folder to scan recursively; may be supplied multiple times
    #[arg(short = 'l', long = "library", required = true, value_name = "PATH")]
    libraries: Vec<PathBuf>,

    /// Mounted iPod filesystem (not /dev/sdX)
    #[arg(short = 'i', long = "ipod", value_name = "MOUNTPOINT")]
    ipod: PathBuf,

    /// Print the synchronization plan without changing the iPod
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug)]
struct Artwork {
    data: Vec<u8>,
    source: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TrackKey {
    artist: String,
    album: String,
    title: String,
    size: u64,
    duration_seconds: u32,
    track_number: u32,
    disc_number: u32,
}

#[derive(Debug)]
struct SourceTrack {
    path: PathBuf,
    metadata: Metadata,
    artwork: Option<Artwork>,
}

#[derive(Debug)]
struct ExistingTrack {
    track: Track,
}

#[derive(Debug)]
struct KeptTrack<'a> {
    existing: ExistingTrack,
    source: &'a SourceTrack,
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let libraries = validate_libraries(&cli.libraries)?;
    let ipod = validate_ipod(&cli.ipod)?;

    println!("Scanning {} source folder(s)…", libraries.len());
    let (sources, duplicate_sources) = scan_sources(&libraries)?;
    println!(
        "Found {} unique MP3 file(s){}.",
        sources.len(),
        if duplicate_sources == 0 {
            String::new()
        } else {
            format!(" ({duplicate_sources} duplicate(s) ignored)")
        }
    );

    println!("Reading iPod database at {}…", ipod.display());
    let mut database = Database::open(&ipod).with_context(|| {
        format!(
            "failed to open {}; ensure this is the mounted iPod root",
            ipod.display()
        )
    })?;
    println!("Device: {}", database.description());
    check_firewire_guid(&database, &ipod)?;

    let existing = read_existing_tracks(&database)?;
    let (kept, deleted, copied) = make_plan(&sources, existing);
    let artwork_updates: Vec<_> = kept
        .iter()
        .filter(|entry| !entry.existing.track.has_artwork && entry.source.artwork.is_some())
        .collect();

    println!(
        "Plan: keep {}, delete {}, copy {}, add artwork to {}.",
        kept.len(),
        deleted.len(),
        copied.len(),
        artwork_updates.len()
    );
    print_plan(&deleted, &copied, &artwork_updates);

    if cli.dry_run {
        println!("Dry run: no files or database entries were changed.");
        return Ok(());
    }

    let database_path = database.database_path()?;
    let backup_path = backup_database(&database_path)?;
    println!("Database backup: {}", backup_path.display());

    // Verify that libgpod can sign and write this device's database before
    // copyPod deletes any files.
    database
        .write()
        .map_err(with_nano_hint)
        .context("iPod preflight write failed; no music files have been changed")?;

    for entry in &deleted {
        println!("DELETE {}", describe_existing(&entry.track));
        database
            .remove_track(entry.track.handle)
            .with_context(|| format!("failed to delete {}", entry.track.path.display()))?;
    }

    // Commit deletion separately. If a subsequent copy fails, the database and
    // filesystem still agree and rerunning copyPod can finish the mirror.
    if !deleted.is_empty() {
        database
            .write()
            .map_err(with_nano_hint)
            .context("failed to save deletions to the iPod database")?;
    }

    for entry in &artwork_updates {
        let artwork = entry.source.artwork.as_ref().expect("filtered above");
        println!(
            "ART    {} — {} ({})",
            entry.source.metadata.artist, entry.source.metadata.title, artwork.source
        );
        database
            .set_artwork(entry.existing.track.handle, &artwork.data)
            .with_context(|| {
                format!("failed to add artwork for {}", entry.source.path.display())
            })?;
    }

    let mut newly_copied = Vec::new();
    for source in &copied {
        println!(
            "COPY   {} — {} ({})",
            source.metadata.artist,
            source.metadata.title,
            source.path.display()
        );
        let artwork = source
            .artwork
            .as_ref()
            .map(|artwork| artwork.data.as_slice());
        match database.add_track(&source.path, &source.metadata, artwork) {
            Ok(path) => newly_copied.push(path),
            Err(error) => {
                remove_uncommitted_files(&newly_copied);
                return Err(error).with_context(|| {
                    format!(
                        "failed to copy {}; rerun copyPod to retry",
                        source.path.display()
                    )
                });
            }
        }
    }

    if !copied.is_empty() || !artwork_updates.is_empty() {
        if let Err(error) = database.write().map_err(with_nano_hint) {
            remove_uncommitted_files(&newly_copied);
            return Err(error).context(
                "failed to save copied tracks or artwork; uncommitted copies were removed",
            );
        }
    }

    println!(
        "Done: kept {}, deleted {}, copied {}, added artwork to {}. Unmount/eject the iPod before unplugging it.",
        kept.len(),
        deleted.len(),
        copied.len(),
        artwork_updates.len()
    );
    Ok(())
}

fn check_firewire_guid(database: &Database, ipod: &Path) -> Result<()> {
    if !database.requires_firewire_guid() {
        println!("FireWire GUID: not required");
        return Ok(());
    }

    let Some(raw_guid) = database.firewire_guid() else {
        bail!(
            "this iPod requires a FireWire GUID, but none was found in SysInfo or SysInfoExtended\n\
             Initialize it, then rerun copyPod:\n  sudo ipod-read-sysinfo-extended /dev/sdX {}",
            ipod.display()
        );
    };
    let guid = normalize_firewire_guid(&raw_guid).ok_or_else(|| {
        anyhow!(
            "this iPod requires a FireWire GUID, but `{raw_guid}` is invalid\n\
             Regenerate it, then rerun copyPod:\n  sudo ipod-read-sysinfo-extended /dev/sdX {}",
            ipod.display()
        )
    })?;

    println!("FireWire GUID: required, present ({guid})");
    Ok(())
}

fn normalize_firewire_guid(value: &str) -> Option<String> {
    let value = value.trim();
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    (value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_uppercase())
}

fn validate_libraries(libraries: &[PathBuf]) -> Result<Vec<PathBuf>> {
    libraries
        .iter()
        .map(|path| {
            let canonical = path
                .canonicalize()
                .with_context(|| format!("library does not exist: {}", path.display()))?;
            if !canonical.is_dir() {
                bail!("library is not a directory: {}", path.display());
            }
            Ok(canonical)
        })
        .collect()
}

fn validate_ipod(ipod: &Path) -> Result<PathBuf> {
    let canonical = ipod
        .canonicalize()
        .with_context(|| format!("iPod mount point does not exist: {}", ipod.display()))?;
    if !canonical.is_dir() {
        bail!("iPod mount point is not a directory: {}", ipod.display());
    }
    if !canonical.join("iPod_Control").is_dir() {
        bail!(
            "{} has no iPod_Control directory; pass the mounted iPod root, not /dev/sdX",
            canonical.display()
        );
    }
    Ok(canonical)
}

fn scan_sources(libraries: &[PathBuf]) -> Result<(Vec<SourceTrack>, usize)> {
    let mut paths = Vec::new();
    for library in libraries {
        for entry in WalkDir::new(library).follow_links(false) {
            let entry = entry.with_context(|| format!("could not scan {}", library.display()))?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.into_path();
            if is_mp3(&path) {
                paths.push(path);
            } else if is_unsupported_audio(&path) {
                eprintln!(
                    "warning: unsupported audio file ignored: {}",
                    path.display()
                );
            }
        }
    }
    paths.sort();

    let mut seen = HashSet::new();
    let mut tracks = Vec::new();
    let mut duplicates = 0;
    for path in paths {
        let track = read_source_track(path)?;
        if !seen.insert(source_key(&track)) {
            duplicates += 1;
            continue;
        }
        tracks.push(track);
    }
    Ok((tracks, duplicates))
}

fn read_existing_tracks(database: &Database) -> Result<Vec<ExistingTrack>> {
    let total = database.track_count();
    println!("Checking {total} existing iPod track(s) from iTunesDB…");
    Ok(database
        .tracks()?
        .into_iter()
        .map(|track| ExistingTrack { track })
        .collect())
}

fn make_plan<'a>(
    sources: &'a [SourceTrack],
    existing: Vec<ExistingTrack>,
) -> (Vec<KeptTrack<'a>>, Vec<ExistingTrack>, Vec<&'a SourceTrack>) {
    let wanted: HashMap<TrackKey, &SourceTrack> = sources
        .iter()
        .map(|source| (source_key(source), source))
        .collect();
    let mut matched = HashSet::new();
    let mut kept = Vec::new();
    let mut deleted = Vec::new();

    for entry in existing {
        let key = existing_key(&entry.track);
        if let Some(source) = wanted.get(&key) {
            if matched.insert(key) {
                kept.push(KeptTrack {
                    existing: entry,
                    source,
                });
                continue;
            }
        }
        deleted.push(entry);
    }

    let copied = sources
        .iter()
        .filter(|source| !matched.contains(&source_key(source)))
        .collect();
    (kept, deleted, copied)
}

fn print_plan(
    deleted: &[ExistingTrack],
    copied: &[&SourceTrack],
    artwork_updates: &[&KeptTrack<'_>],
) {
    for entry in deleted {
        println!("- {}", describe_existing(&entry.track));
    }
    for source in copied {
        println!(
            "+ {} — {} ({})",
            source.metadata.artist,
            source.metadata.title,
            source.path.display()
        );
    }
    for entry in artwork_updates {
        let artwork = entry.source.artwork.as_ref().expect("filtered above");
        println!(
            "~ artwork: {} — {} ({})",
            entry.source.metadata.artist, entry.source.metadata.title, artwork.source
        );
    }
}

fn describe_existing(track: &Track) -> String {
    let artist = if track.artist.is_empty() {
        "Unknown Artist"
    } else {
        &track.artist
    };
    let title = if track.title.is_empty() {
        track
            .path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Unknown Title")
    } else {
        &track.title
    };
    format!("{artist} — {title} ({})", track.path.display())
}

fn read_source_track(path: PathBuf) -> Result<SourceTrack> {
    let tagged_file = Probe::open(&path)
        .with_context(|| format!("could not open MP3 metadata: {}", path.display()))?
        .read()
        .with_context(|| format!("could not parse MP3 metadata: {}", path.display()))?;
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());
    let properties = tagged_file.properties();
    let file_metadata = fs::metadata(&path)?;

    let fallback_title = path
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Unknown Title")
        .to_owned();

    let title = tag
        .and_then(|tag| tag.title())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_title);
    let artist = tag
        .and_then(|tag| tag.artist())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Unknown Artist".to_owned());
    let album = tag
        .and_then(|tag| tag.album())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Unknown Album".to_owned());
    let album_artist = tag
        .and_then(|tag| tag.get_string(&lofty::tag::ItemKey::AlbumArtist))
        .unwrap_or("")
        .trim()
        .to_owned();
    let genre = tag
        .and_then(|tag| tag.genre())
        .map(|value| value.trim().to_owned())
        .unwrap_or_default();
    let comment = tag
        .and_then(|tag| tag.comment())
        .map(|value| value.trim().to_owned())
        .unwrap_or_default();

    let duration_ms = properties.duration().as_millis().min(u128::from(u32::MAX)) as u32;
    let modified_at = file_metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0);
    let metadata = Metadata {
        title,
        album,
        artist,
        album_artist,
        genre,
        comment,
        size: file_metadata.len(),
        modified_at,
        duration_ms,
        bitrate_kbps: properties.audio_bitrate().unwrap_or(0),
        sample_rate_hz: properties.sample_rate().unwrap_or(0),
        year: tag.and_then(|tag| tag.year()).unwrap_or(0),
        track_number: tag.and_then(|tag| tag.track()).unwrap_or(0),
        track_total: tag.and_then(|tag| tag.track_total()).unwrap_or(0),
        disc_number: tag.and_then(|tag| tag.disk()).unwrap_or(0),
        disc_total: tag.and_then(|tag| tag.disk_total()).unwrap_or(0),
    };

    let embedded = tag.and_then(|tag| {
        tag.pictures()
            .iter()
            .find(|picture| picture.pic_type() == PictureType::CoverFront)
            .or_else(|| tag.pictures().first())
    });
    let artwork = if let Some(picture) = embedded.filter(|picture| !picture.data().is_empty()) {
        Some(Artwork {
            data: picture.data().to_vec(),
            source: format!("embedded in {}", path.display()),
        })
    } else {
        read_external_artwork(&path)?
    };

    Ok(SourceTrack {
        path,
        metadata,
        artwork,
    })
}

fn read_external_artwork(track_path: &Path) -> Result<Option<Artwork>> {
    const COVER_NAMES: &[&str] = &[
        "cover.jpg",
        "cover.jpeg",
        "cover.png",
        "folder.jpg",
        "folder.jpeg",
        "folder.png",
        "front.jpg",
        "front.jpeg",
        "front.png",
    ];
    let Some(directory) = track_path.parent() else {
        return Ok(None);
    };

    let mut files = HashMap::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("could not scan for artwork in {}", directory.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            files.insert(
                entry.file_name().to_string_lossy().to_lowercase(),
                entry.path(),
            );
        }
    }

    for name in COVER_NAMES {
        if let Some(path) = files.get(*name) {
            return Ok(Some(Artwork {
                data: fs::read(path)
                    .with_context(|| format!("could not read artwork {}", path.display()))?,
                source: path.display().to_string(),
            }));
        }
    }
    Ok(None)
}

fn source_key(source: &SourceTrack) -> TrackKey {
    metadata_key(
        &source.metadata.album_artist,
        &source.metadata.artist,
        &source.metadata.album,
        &source.metadata.title,
        source.metadata.size,
        source.metadata.duration_ms,
        source.metadata.track_number,
        source.metadata.disc_number,
    )
}

fn existing_key(track: &Track) -> TrackKey {
    metadata_key(
        &track.album_artist,
        &track.artist,
        &track.album,
        &track.title,
        track.size,
        track.duration_ms,
        track.track_number,
        track.disc_number,
    )
}

#[allow(clippy::too_many_arguments)]
fn metadata_key(
    album_artist: &str,
    artist: &str,
    album: &str,
    title: &str,
    size: u64,
    duration_ms: u32,
    track_number: u32,
    disc_number: u32,
) -> TrackKey {
    let effective_artist = if album_artist.trim().is_empty() {
        artist
    } else {
        album_artist
    };
    TrackKey {
        artist: normalize_tag(effective_artist),
        album: normalize_tag(album),
        title: normalize_tag(title),
        size,
        duration_seconds: duration_ms.saturating_add(500) / 1_000,
        track_number,
        disc_number,
    }
}

fn normalize_tag(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_mp3(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp3"))
}

fn is_unsupported_audio(path: &Path) -> bool {
    const AUDIO_EXTENSIONS: &[&str] = &[
        "aac", "aif", "aiff", "alac", "flac", "m4a", "ogg", "opus", "wav", "wma",
    ];
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            AUDIO_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

fn backup_database(database_path: &Path) -> Result<PathBuf> {
    let filename = database_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid iTunesDB path: {}", database_path.display()))?;
    let backup = database_path.with_file_name(format!("{filename}.copyPod-backup"));
    fs::copy(database_path, &backup).with_context(|| {
        format!(
            "could not back up {} to {}",
            database_path.display(),
            backup.display()
        )
    })?;
    Ok(backup)
}

fn remove_uncommitted_files(paths: &[PathBuf]) {
    for path in paths {
        if let Err(error) = fs::remove_file(path) {
            eprintln!(
                "warning: could not remove uncommitted copy {}: {error}",
                path.display()
            );
        }
    }
}

fn with_nano_hint(error: anyhow::Error) -> anyhow::Error {
    anyhow!(
        "{error:#}\nFor a Nano 3G, make sure iPod_Control/Device/SysInfoExtended contains its FireWire GUID. The one-time setup is:\n  sudo ipod-read-sysinfo-extended /dev/sdX /path/to/mounted/ipod"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_mp3_case_insensitively() {
        assert!(is_mp3(Path::new("song.mp3")));
        assert!(is_mp3(Path::new("song.MP3")));
        assert!(!is_mp3(Path::new("song.flac")));
    }

    #[test]
    fn recognizes_unsupported_audio_without_flagging_images() {
        assert!(is_unsupported_audio(Path::new("song.flac")));
        assert!(is_unsupported_audio(Path::new("song.M4A")));
        assert!(!is_unsupported_audio(Path::new("cover.jpg")));
    }

    #[test]
    fn validates_and_normalizes_firewire_guids() {
        assert_eq!(
            normalize_firewire_guid("0x000a27001aae9513"),
            Some("000A27001AAE9513".to_owned())
        );
        assert_eq!(
            normalize_firewire_guid("000A27001AAE9513"),
            Some("000A27001AAE9513".to_owned())
        );
        assert_eq!(normalize_firewire_guid("not-a-guid"), None);
        assert_eq!(normalize_firewire_guid("000A27001AAE951"), None);
    }

    #[test]
    fn track_keys_normalize_tags_but_require_the_same_size() {
        let first = metadata_key(
            "",
            "  Some   Artist ",
            "ALBUM",
            "Song",
            12_345,
            61_200,
            2,
            1,
        );
        let same = metadata_key("", "some artist", "album", " song ", 12_345, 61_499, 2, 1);
        let different_size = metadata_key("", "some artist", "album", "song", 12_346, 61_200, 2, 1);

        assert_eq!(first, same);
        assert_ne!(first, different_size);
    }

    #[test]
    fn finds_external_cover_art_case_insensitively() {
        let directory =
            std::env::temp_dir().join(format!("copypod-artwork-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir(&directory).unwrap();
        let track = directory.join("song.mp3");
        fs::write(&track, []).unwrap();
        fs::write(directory.join("Cover.JPEG"), [1, 2, 3]).unwrap();

        let artwork = read_external_artwork(&track).unwrap().unwrap();
        assert_eq!(artwork.data, [1, 2, 3]);

        fs::remove_dir_all(directory).unwrap();
    }
}
