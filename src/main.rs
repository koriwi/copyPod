mod gpod;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{anyhow, bail, Context, Result};
use blake3::Hash;
use clap::Parser;
use lofty::file::{AudioFile, TaggedFileExt};
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
struct SourceTrack {
    path: PathBuf,
    hash: Hash,
    metadata: Metadata,
}

#[derive(Debug)]
struct ExistingTrack {
    track: Track,
    hash: Option<Hash>,
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

    println!(
        "Plan: keep {}, delete {}, copy {}.",
        kept.len(),
        deleted.len(),
        copied.len()
    );
    print_plan(&deleted, &copied);

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

    let mut newly_copied = Vec::new();
    for source in &copied {
        println!(
            "COPY   {} — {} ({})",
            source.metadata.artist,
            source.metadata.title,
            source.path.display()
        );
        match database.add_track(&source.path, &source.metadata) {
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

    if !copied.is_empty() {
        if let Err(error) = database.write().map_err(with_nano_hint) {
            remove_uncommitted_files(&newly_copied);
            return Err(error)
                .context("failed to save copied tracks; uncommitted copies were removed");
        }
    }

    println!(
        "Done: kept {}, deleted {}, copied {}. Unmount/eject the iPod before unplugging it.",
        kept.len(),
        deleted.len(),
        copied.len()
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
        let hash = hash_file(&path)
            .with_context(|| format!("could not hash source file {}", path.display()))?;
        if !seen.insert(hash) {
            duplicates += 1;
            continue;
        }
        let metadata = read_metadata(&path)?;
        tracks.push(SourceTrack {
            path,
            hash,
            metadata,
        });
    }
    Ok((tracks, duplicates))
}

fn read_existing_tracks(database: &Database) -> Result<Vec<ExistingTrack>> {
    let total = database.track_count();
    println!("Checking {total} existing iPod track(s)…");
    let tracks = database.tracks()?;
    let mut result = Vec::with_capacity(tracks.len());
    for (index, track) in tracks.into_iter().enumerate() {
        print!("\rHashing existing iPod tracks: {}/{total}", index + 1);
        std::io::stdout().flush()?;
        let hash = match hash_file(&track.path) {
            Ok(hash) => Some(hash),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                None
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("could not read iPod track {}", track.path.display())
                });
            }
        };
        result.push(ExistingTrack { track, hash });
    }
    if total > 0 {
        println!();
    }
    Ok(result)
}

fn make_plan(
    sources: &[SourceTrack],
    existing: Vec<ExistingTrack>,
) -> (Vec<ExistingTrack>, Vec<ExistingTrack>, Vec<&SourceTrack>) {
    let wanted: HashMap<Hash, &SourceTrack> =
        sources.iter().map(|source| (source.hash, source)).collect();
    let mut matched = HashSet::new();
    let mut kept = Vec::new();
    let mut deleted = Vec::new();

    for entry in existing {
        if let Some(hash) = entry.hash {
            if wanted.contains_key(&hash) && matched.insert(hash) {
                kept.push(entry);
                continue;
            }
        }
        deleted.push(entry);
    }

    let copied = sources
        .iter()
        .filter(|source| !matched.contains(&source.hash))
        .collect();
    (kept, deleted, copied)
}

fn print_plan(deleted: &[ExistingTrack], copied: &[&SourceTrack]) {
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

fn read_metadata(path: &Path) -> Result<Metadata> {
    let tagged_file = Probe::open(path)
        .with_context(|| format!("could not open MP3 metadata: {}", path.display()))?
        .read()
        .with_context(|| format!("could not parse MP3 metadata: {}", path.display()))?;
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());
    let properties = tagged_file.properties();
    let file_metadata = fs::metadata(path)?;

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

    Ok(Metadata {
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
    })
}

fn hash_file(path: &Path) -> Result<Hash> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize())
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
}
