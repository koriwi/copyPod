mod opod;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};
use clap::Parser;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::prelude::Accessor;
use lofty::probe::Probe;
use walkdir::WalkDir;

use crate::opod::{Database, Metadata, Playlist, PlaylistHandle, Track, TrackHandle};

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
struct SourcePlaylist {
    name: String,
    path: PathBuf,
    tracks: Vec<TrackKey>,
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

#[derive(Debug)]
struct PlaylistUpdate<'a> {
    source: &'a SourcePlaylist,
    handle: PlaylistHandle,
}

#[derive(Debug, Default)]
struct PlaylistPlan<'a> {
    kept: Vec<&'a SourcePlaylist>,
    created: Vec<&'a SourcePlaylist>,
    updated: Vec<PlaylistUpdate<'a>>,
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
    let (sources, source_playlists, duplicate_sources) = scan_sources(&libraries)?;
    println!(
        "Found {} unique MP3 file(s){} and {} M3U playlist(s).",
        sources.len(),
        if duplicate_sources == 0 {
            String::new()
        } else {
            format!(" ({duplicate_sources} duplicate(s) ignored)")
        },
        source_playlists.len()
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
    if !source_playlists.is_empty() && !database.supports_playlists() {
        bail!(
            "{} M3U playlist(s) were found, but libopod does not support playlist writes for this iPod profile",
            source_playlists.len()
        );
    }

    let existing = read_existing_tracks(&database)?;
    let playlist_plan = if source_playlists.is_empty() {
        PlaylistPlan::default()
    } else {
        let existing_track_keys: HashMap<_, _> = existing
            .iter()
            .map(|entry| (entry.track.handle, existing_key(&entry.track)))
            .collect();
        make_playlist_plan(
            &source_playlists,
            database.playlists()?,
            &existing_track_keys,
        )
    };
    let (kept, deleted, copied) = make_plan(&sources, existing);
    // Devices without writable cover formats (Nano 1G/2G) cannot store
    // artwork at all; embedded source art is left out instead of failing.
    let artwork_capable = database.supports_artwork();
    let artwork_updates: Vec<_> = kept
        .iter()
        .filter(|entry| !entry.existing.track.has_artwork && entry.source.artwork.is_some())
        .filter(|_| artwork_capable)
        .collect();

    println!(
        "Plan: keep {}, delete {}, copy {}, add artwork to {}; keep {} playlist(s), create {}, update {}.",
        kept.len(),
        deleted.len(),
        copied.len(),
        artwork_updates.len(),
        playlist_plan.kept.len(),
        playlist_plan.created.len(),
        playlist_plan.updated.len()
    );
    if !artwork_capable
        && (kept.iter().any(|entry| entry.source.artwork.is_some())
            || copied.iter().any(|source| source.artwork.is_some()))
    {
        println!("       (embedded artwork ignored: this device cannot store cover art)");
    }
    print_plan(&deleted, &copied, &artwork_updates);
    print_playlist_plan(&playlist_plan);

    if cli.dry_run {
        println!("Dry run: no files or database entries were changed.");
        return Ok(());
    }

    for entry in &deleted {
        println!("DELETE {}", describe_existing(&entry.track));
        database
            .remove_track(entry.track.handle)
            .with_context(|| format!("failed to delete {}", entry.track.path.display()))?;
    }

    // Commit deletions separately. If a subsequent copy fails, the database and
    // filesystem still agree and rerunning copyPod can finish the mirror.
    if !deleted.is_empty() {
        database
            .write()
            .context("failed to save deletions to the iPod database")?;
    }

    // Tracks kept without artwork but with artwork in the source are replaced
    // by a fresh indexed entry carrying the artwork.
    for entry in &artwork_updates {
        let artwork = entry.source.artwork.as_ref().expect("filtered above");
        println!(
            "ART    {} — {} ({})",
            entry.source.metadata.artist, entry.source.metadata.title, artwork.source
        );
        database
            .remove_track(entry.existing.track.handle)
            .with_context(|| format!("failed to replace {}", entry.source.path.display()))?;
        database
            .add_track(
                &entry.source.path,
                &entry.source.metadata,
                Some(&artwork.data),
            )
            .with_context(|| {
                format!("failed to add artwork for {}", entry.source.path.display())
            })?;
    }

    for source in &copied {
        println!(
            "COPY   {} — {} ({})",
            source.metadata.artist,
            source.metadata.title,
            source.path.display()
        );
        let artwork = if artwork_capable {
            source
                .artwork
                .as_ref()
                .map(|artwork| artwork.data.as_slice())
        } else {
            None
        };
        database
            .add_track(&source.path, &source.metadata, artwork)
            .with_context(|| {
                format!(
                    "failed to copy {}; rerun copyPod to retry",
                    source.path.display()
                )
            })?;
    }

    database
        .write()
        .context("failed to commit copied tracks or artwork to the iPod database")?;

    // Additions and artwork replacements receive their persistent IDs during
    // the track commit, so rebuild the playlist plan against the refreshed
    // library before queueing playlist mutations.
    let playlist_plan = if source_playlists.is_empty() {
        PlaylistPlan::default()
    } else {
        let synced_tracks = database.tracks()?;
        let synced_track_keys: HashMap<_, _> = synced_tracks
            .iter()
            .map(|track| (track.handle, existing_key(track)))
            .collect();
        let plan = make_playlist_plan(&source_playlists, database.playlists()?, &synced_track_keys);
        let handles_by_key: HashMap<_, _> = synced_tracks
            .iter()
            .map(|track| (existing_key(track), track.handle))
            .collect();
        apply_playlist_plan(&mut database, &plan, &handles_by_key)?;
        database
            .write()
            .context("failed to commit M3U playlists to the iPod database")?;
        plan
    };

    println!(
        "Done: kept {}, deleted {}, copied {}, added artwork to {}; created {} playlist(s), updated {}. Unmount/eject the iPod before unplugging it.",
        kept.len(),
        deleted.len(),
        copied.len(),
        artwork_updates.len(),
        playlist_plan.created.len(),
        playlist_plan.updated.len()
    );
    Ok(())
}

fn check_firewire_guid(database: &Database, ipod: &Path) -> Result<()> {
    if !database.requires_firewire_guid() {
        println!("FireWire GUID: not required");
        return Ok(());
    }
    if !database.has_firewire_guid() {
        bail!(
            "this iPod requires a FireWire GUID, but none was found in SysInfo or SysInfoExtended\n\
             Initialize it, then rerun copyPod:\n  sudo ipod-read-sysinfo-extended /dev/sdX {}",
            ipod.display()
        );
    }

    println!("FireWire GUID: required and present (value redacted)");
    Ok(())
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

fn scan_sources(libraries: &[PathBuf]) -> Result<(Vec<SourceTrack>, Vec<SourcePlaylist>, usize)> {
    let mut track_paths = Vec::new();
    let mut playlist_paths = Vec::new();
    for library in libraries {
        for entry in WalkDir::new(library).follow_links(false) {
            let entry = entry.with_context(|| format!("could not scan {}", library.display()))?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.into_path();
            if is_mp3(&path) {
                track_paths.push(path);
            } else if is_m3u(&path) {
                playlist_paths.push(path);
            } else if is_unsupported_audio(&path) {
                eprintln!(
                    "warning: unsupported audio file ignored: {}",
                    path.display()
                );
            }
        }
    }
    track_paths.sort();
    playlist_paths.sort();

    let mut seen = HashSet::new();
    let mut tracks = Vec::new();
    let mut keys_by_path = HashMap::new();
    let mut duplicates = 0;
    for path in track_paths {
        let track = read_source_track(path)?;
        let key = source_key(&track);
        keys_by_path.insert(track.path.clone(), key.clone());
        if !seen.insert(key) {
            duplicates += 1;
            continue;
        }
        tracks.push(track);
    }
    let playlists = read_source_playlists(&playlist_paths, &keys_by_path)?;
    Ok((tracks, playlists, duplicates))
}

fn read_source_playlists(
    paths: &[PathBuf],
    keys_by_path: &HashMap<PathBuf, TrackKey>,
) -> Result<Vec<SourcePlaylist>> {
    let mut seen_names = HashMap::<String, PathBuf>::new();
    let mut playlists = Vec::new();
    for path in paths {
        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .with_context(|| format!("playlist has no valid UTF-8 name: {}", path.display()))?
            .to_owned();
        if name.encode_utf16().count() > 255 {
            bail!(
                "playlist name exceeds 255 UTF-16 code units: {}",
                path.display()
            );
        }
        let normalized_name = normalize_playlist_name(&name);
        if let Some(previous) = seen_names.insert(normalized_name, path.clone()) {
            bail!(
                "M3U playlists must have unique names (case-insensitive): {} and {}",
                previous.display(),
                path.display()
            );
        }

        let bytes = fs::read(path)
            .with_context(|| format!("could not read M3U playlist: {}", path.display()))?;
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        let contents = std::str::from_utf8(bytes)
            .with_context(|| format!("M3U playlist is not UTF-8: {}", path.display()))?;
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        let mut tracks = Vec::new();
        for (line_index, line) in contents.lines().enumerate() {
            let entry = line.trim();
            if entry.is_empty() || entry.starts_with('#') {
                continue;
            }
            let referenced = Path::new(entry);
            let referenced = if referenced.is_absolute() {
                referenced.to_path_buf()
            } else {
                directory.join(referenced)
            };
            let referenced = referenced.canonicalize().with_context(|| {
                format!(
                    "{}:{} references a missing track: {}",
                    path.display(),
                    line_index + 1,
                    entry
                )
            })?;
            let key = keys_by_path.get(&referenced).with_context(|| {
                format!(
                    "{}:{} references a track outside the supplied MP3 libraries: {}",
                    path.display(),
                    line_index + 1,
                    referenced.display()
                )
            })?;
            tracks.push(key.clone());
        }
        playlists.push(SourcePlaylist {
            name,
            path: path.clone(),
            tracks,
        });
    }
    Ok(playlists)
}

fn read_existing_tracks(database: &Database) -> Result<Vec<ExistingTrack>> {
    let total = database.track_count();
    println!("Checking {total} existing iPod track(s) from the authoritative library…");
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

fn make_playlist_plan<'a>(
    sources: &'a [SourcePlaylist],
    existing: Vec<Playlist>,
    track_keys: &HashMap<TrackHandle, TrackKey>,
) -> PlaylistPlan<'a> {
    let mut kept = Vec::new();
    let mut created = Vec::new();
    let mut updated = Vec::new();
    let mut matched = HashSet::new();

    for source in sources {
        let existing_playlist = existing.iter().find(|playlist| {
            !playlist.is_hidden
                && !playlist.is_smart
                && !matched.contains(&playlist.handle)
                && normalize_playlist_name(&playlist.name) == normalize_playlist_name(&source.name)
        });
        let Some(existing_playlist) = existing_playlist else {
            created.push(source);
            continue;
        };
        matched.insert(existing_playlist.handle);
        let existing_keys: Option<Vec<_>> = existing_playlist
            .tracks
            .iter()
            .map(|handle| track_keys.get(handle).cloned())
            .collect();
        if existing_playlist.name == source.name
            && existing_keys.as_deref() == Some(source.tracks.as_slice())
        {
            kept.push(source);
        } else {
            updated.push(PlaylistUpdate {
                source,
                handle: existing_playlist.handle,
            });
        }
    }

    PlaylistPlan {
        kept,
        created,
        updated,
    }
}

fn apply_playlist_plan(
    database: &mut Database,
    plan: &PlaylistPlan<'_>,
    handles_by_key: &HashMap<TrackKey, TrackHandle>,
) -> Result<()> {
    for source in &plan.created {
        let tracks = resolve_playlist_handles(source, handles_by_key)?;
        println!("PLAYLIST + {} ({})", source.name, source.path.display());
        database
            .create_playlist(&source.name, &tracks)
            .with_context(|| format!("failed to create playlist {}", source.name))?;
    }
    for update in &plan.updated {
        let tracks = resolve_playlist_handles(update.source, handles_by_key)?;
        println!(
            "PLAYLIST ~ {} ({})",
            update.source.name,
            update.source.path.display()
        );
        database
            .update_playlist(update.handle, &update.source.name, &tracks)
            .with_context(|| format!("failed to update playlist {}", update.source.name))?;
    }
    Ok(())
}

fn resolve_playlist_handles(
    source: &SourcePlaylist,
    handles_by_key: &HashMap<TrackKey, TrackHandle>,
) -> Result<Vec<TrackHandle>> {
    source
        .tracks
        .iter()
        .map(|key| {
            handles_by_key.get(key).copied().with_context(|| {
                format!(
                    "playlist {} contains a track that was not synchronized",
                    source.path.display()
                )
            })
        })
        .collect()
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

fn print_playlist_plan(plan: &PlaylistPlan<'_>) {
    for source in &plan.created {
        println!("+ playlist: {} ({})", source.name, source.path.display());
    }
    for update in &plan.updated {
        println!(
            "~ playlist: {} ({})",
            update.source.name,
            update.source.path.display()
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
    // The track artist identifies the recording; album artist is grouping
    // metadata and can legitimately differ on compilations. Some classic iPod
    // databases also omit album artist when libopod rewrites a track, so using
    // it as the primary identity causes a perpetual delete/copy cycle. Only
    // use album artist as a fallback when the track artist is absent.
    let effective_artist = if artist.trim().is_empty() {
        album_artist
    } else {
        artist
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

fn normalize_playlist_name(value: &str) -> String {
    value.trim().to_lowercase()
}

fn is_mp3(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp3"))
}

fn is_m3u(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("m3u") || extension.eq_ignore_ascii_case("m3u8")
        })
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
    fn recognizes_m3u_case_insensitively() {
        assert!(is_m3u(Path::new("mix.m3u")));
        assert!(is_m3u(Path::new("mix.M3U8")));
        assert!(!is_m3u(Path::new("mix.txt")));
    }

    #[test]
    fn recognizes_unsupported_audio_without_flagging_images() {
        assert!(is_unsupported_audio(Path::new("song.flac")));
        assert!(is_unsupported_audio(Path::new("song.M4A")));
        assert!(!is_unsupported_audio(Path::new("cover.jpg")));
    }

    #[test]
    fn track_keys_normalize_tags_but_require_the_same_size() {
        let first = metadata_key(
            "Compilation Artist",
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
    fn track_key_falls_back_to_album_artist_when_artist_is_missing() {
        let album_artist = metadata_key("Artist", "", "Album", "Song", 1, 1, 0, 0);
        let track_artist = metadata_key("", "Artist", "Album", "Song", 1, 1, 0, 0);

        assert_eq!(album_artist, track_artist);
    }

    #[test]
    fn reads_relative_extended_m3u_entries_in_order() {
        let directory =
            std::env::temp_dir().join(format!("copypod-m3u-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(directory.join("music")).unwrap();
        let first = directory.join("music/one.mp3");
        let second = directory.join("music/two.mp3");
        fs::write(&first, []).unwrap();
        fs::write(&second, []).unwrap();
        let playlist_path = directory.join("Road Trip.m3u8");
        fs::write(
            &playlist_path,
            b"\xef\xbb\xbf#EXTM3U\n#EXTINF:1,One\nmusic/one.mp3\n\n music/two.mp3 \nmusic/one.mp3\n",
        )
        .unwrap();

        let first_key = metadata_key("", "a", "b", "one", 1, 1, 0, 0);
        let second_key = metadata_key("", "a", "b", "two", 1, 1, 0, 0);
        let keys = HashMap::from([
            (first.canonicalize().unwrap(), first_key.clone()),
            (second.canonicalize().unwrap(), second_key.clone()),
        ]);
        let playlists = read_source_playlists(&[playlist_path], &keys).unwrap();

        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].name, "Road Trip");
        assert_eq!(
            playlists[0].tracks,
            vec![first_key.clone(), second_key, first_key]
        );
        fs::remove_dir_all(directory).unwrap();
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
