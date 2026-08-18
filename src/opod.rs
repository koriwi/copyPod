use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use libopod::{ChecksumKind, Device, PersistentId};

#[derive(Debug)]
pub struct Metadata {
    pub title: String,
    pub album: String,
    pub artist: String,
    pub album_artist: String,
    pub genre: String,
    pub comment: String,
    pub size: u64,
    pub modified_at: i64,
    pub duration_ms: u32,
    pub bitrate_kbps: u32,
    pub sample_rate_hz: u32,
    pub year: u32,
    pub track_number: u32,
    pub track_total: u32,
    pub disc_number: u32,
    pub disc_total: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct TrackHandle(PersistentId);

#[derive(Debug)]
pub struct Track {
    pub handle: TrackHandle,
    pub path: PathBuf,
    pub title: String,
    pub album: String,
    pub artist: String,
    pub album_artist: String,
    pub size: u64,
    pub duration_ms: u32,
    pub track_number: u32,
    pub disc_number: u32,
    pub has_artwork: bool,
}

pub struct Database {
    device: Device,
}

impl Database {
    pub fn open(mountpoint: &Path) -> Result<Self> {
        let device = Device::open(mountpoint).context("libopod could not inspect the iPod")?;
        if device.library().is_none() {
            bail!("libopod does not yet have a read adapter for this device profile");
        }
        Ok(Self { device })
    }

    pub fn description(&self) -> String {
        self.device.profile().map_or_else(
            || "unknown iPod".to_owned(),
            |profile| profile.display_name().to_owned(),
        )
    }

    pub fn requires_firewire_guid(&self) -> bool {
        self.device.profile().is_some_and(|profile| {
            matches!(
                profile.capabilities().checksum,
                ChecksumKind::Hash58 | ChecksumKind::HashAb
            )
        })
    }

    pub fn has_firewire_guid(&self) -> bool {
        self.device.evidence().has_firewire_guid()
    }

    pub fn track_count(&self) -> usize {
        self.device
            .library()
            .map_or(0, libopod::Library::track_count)
    }

    pub fn tracks(&self) -> Result<Vec<Track>> {
        let library = self
            .device
            .library()
            .context("libopod has no readable library for this device")?;
        library
            .tracks()
            .iter()
            .map(|track| {
                Ok(Track {
                    handle: TrackHandle(track.id),
                    path: self.device.track_path(track)?,
                    title: track.title.clone(),
                    album: track.album.clone(),
                    artist: track.artist.clone(),
                    album_artist: track.album_artist.clone(),
                    size: track.size,
                    duration_ms: track.duration_ms,
                    track_number: track.track_number,
                    disc_number: track.disc_number,
                    has_artwork: track.has_artwork,
                })
            })
            .collect()
    }

    pub fn remove_track(&mut self, track: TrackHandle) -> Result<()> {
        let _persistent_id = track.0;
        bail!("libopod track removal is not implemented; no device files were changed")
    }

    pub fn set_artwork(&mut self, track: TrackHandle, artwork: &[u8]) -> Result<()> {
        let (_persistent_id, _artwork) = (track.0, artwork);
        bail!("libopod artwork updates are not implemented; no device files were changed")
    }

    pub fn add_track(
        &mut self,
        source: &Path,
        metadata: &Metadata,
        artwork: Option<&[u8]>,
    ) -> Result<PathBuf> {
        let _future_write_input = (
            source,
            &metadata.title,
            &metadata.album,
            &metadata.artist,
            &metadata.album_artist,
            &metadata.genre,
            &metadata.comment,
            metadata.size,
            metadata.modified_at,
            metadata.duration_ms,
            metadata.bitrate_kbps,
            metadata.sample_rate_hz,
            metadata.year,
            metadata.track_number,
            metadata.track_total,
            metadata.disc_number,
            metadata.disc_total,
            artwork,
        );
        bail!("libopod track addition is not implemented; no device files were changed")
    }

    pub fn write(&mut self) -> Result<()> {
        bail!("libopod commits are not implemented; no device files were changed")
    }
}
