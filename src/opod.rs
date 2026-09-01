use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use libopod::{
    BackendKind, ChecksumKind, Device, MediaDeletionPolicy, MediaKind, PersistentId, TrackToAdd,
};

#[derive(Clone, Debug)]
pub struct Metadata {
    pub title: String,
    pub album: String,
    pub artist: String,
    pub album_artist: String,
    pub genre: String,
    #[allow(dead_code)] // kept for API parity; not read by the mirror planner
    pub comment: String,
    pub size: u64,
    #[allow(dead_code)] // kept for API parity; not read by the mirror planner
    pub modified_at: i64,
    pub duration_ms: u32,
    pub bitrate_kbps: u32,
    pub sample_rate_hz: u32,
    pub year: u32,
    pub track_number: u32,
    pub track_total: u32,
    pub disc_number: u32,
    pub disc_total: u32,
    pub media_kind: MediaKind,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TrackHandle(PersistentId);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PlaylistHandle(PersistentId);

#[cfg(test)]
impl TrackHandle {
    pub(crate) fn from_test_bits(bits: u64) -> Self {
        Self(PersistentId::from_bits(bits))
    }
}

#[cfg(test)]
impl PlaylistHandle {
    pub(crate) fn from_test_bits(bits: u64) -> Self {
        Self(PersistentId::from_bits(bits))
    }
}

#[derive(Debug)]
pub struct Track {
    pub handle: TrackHandle,
    pub path: PathBuf,
    pub media_missing: bool,
    pub title: String,
    pub album: String,
    pub artist: String,
    pub album_artist: String,
    pub size: u64,
    pub duration_ms: u32,
    pub track_number: u32,
    pub disc_number: u32,
    pub has_artwork: bool,
    pub media_kind: MediaKind,
}

#[derive(Debug)]
pub struct Playlist {
    pub handle: PlaylistHandle,
    pub name: String,
    pub tracks: Vec<TrackHandle>,
    pub is_hidden: bool,
    pub is_smart: bool,
}

/// One queued library change, committed by [`Database::write`].
enum PendingChange {
    Remove(PersistentId),
    Add(Box<PendingAdd>),
    CreatePlaylist {
        name: String,
        tracks: Vec<PersistentId>,
    },
    UpdatePlaylist {
        id: PersistentId,
        name: String,
        tracks: Vec<PersistentId>,
    },
    DeletePlaylist(PersistentId),
}

struct PendingAdd {
    source: PathBuf,
    metadata: Metadata,
    artwork: Option<Vec<u8>>,
}

pub struct Database {
    mountpoint: PathBuf,
    device: Device,
    pending: Vec<PendingChange>,
    artwork_sequence: u64,
}

impl Database {
    pub fn open(mountpoint: &Path) -> Result<Self> {
        let device = Device::open(mountpoint).context("libopod could not inspect the iPod")?;
        if device.library().is_none() {
            bail!("libopod does not yet have a read adapter for this device profile");
        }
        Ok(Self {
            mountpoint: mountpoint.to_path_buf(),
            device,
            pending: Vec::new(),
            artwork_sequence: 0,
        })
    }

    /// Recovers an interrupted libopod transaction at `mountpoint`.
    pub fn recover_interrupted_transaction(mountpoint: &Path) -> Result<bool> {
        libopod::recover_interrupted_transaction(mountpoint)
            .context("libopod could not recover the interrupted transaction")
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

    /// Whether this device can store cover artwork. Nano 1G/2G cannot, so
    /// the mirror planner skips artwork instead of failing the commit.
    pub fn supports_artwork(&self) -> bool {
        self.device
            .profile()
            .is_some_and(|profile| profile.capabilities().supports_artwork())
    }

    /// Whether this device has qualified podcast write support.
    pub fn supports_podcasts(&self) -> bool {
        self.device.profile().map(libopod::DeviceProfile::key) == Some("nano-7g")
    }

    /// Whether libopod supports playlist mutations for this device profile.
    pub fn supports_playlists(&self) -> bool {
        self.device.profile().is_some_and(|profile| {
            profile_supports_playlists(profile.key(), profile.capabilities().backend)
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
        let missing: HashSet<_> = self
            .device
            .missing_media_track_ids()
            .context("audit iPod media references")?
            .into_iter()
            .collect();
        let library = self
            .device
            .library()
            .context("libopod has no readable library for this device")?;
        library
            .tracks()
            .iter()
            .map(|track| {
                let media_missing = missing.contains(&track.id);
                let path = if media_missing {
                    self.device.mount().as_path().join(track.location.as_str())
                } else {
                    self.device.track_path(track)?
                };
                Ok(Track {
                    handle: TrackHandle(track.id),
                    path,
                    media_missing,
                    title: track.title.clone(),
                    album: track.album.clone(),
                    artist: track.artist.clone(),
                    album_artist: track.album_artist.clone(),
                    size: track.size,
                    duration_ms: track.duration_ms,
                    track_number: track.track_number,
                    disc_number: track.disc_number,
                    has_artwork: track.has_artwork,
                    media_kind: track.media_kind,
                })
            })
            .collect()
    }

    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        let library = self
            .device
            .library()
            .context("libopod has no readable library for this device")?;
        Ok(library
            .playlists()
            .iter()
            .map(|playlist| Playlist {
                handle: PlaylistHandle(playlist.id),
                name: playlist.name.clone(),
                tracks: playlist
                    .track_ids()
                    .iter()
                    .copied()
                    .map(TrackHandle)
                    .collect(),
                is_hidden: playlist.is_hidden,
                is_smart: playlist.is_smart,
            })
            .collect())
    }

    /// Queues a track removal. The media file is deleted as part of the
    /// commit (libopod backs it up and restores it on rollback).
    pub fn remove_track(&mut self, track: TrackHandle) -> Result<()> {
        let present = self.device.library().is_some_and(|library| {
            library
                .tracks()
                .iter()
                .any(|existing| existing.id == track.0)
        });
        if !present {
            bail!("track is not present in the opened library");
        }
        self.pending.push(PendingChange::Remove(track.0));
        Ok(())
    }

    /// Queues a track addition. When `artwork` is present it is encoded into
    /// the device's cover formats as part of the commit.
    pub fn add_track(
        &mut self,
        source: &Path,
        metadata: &Metadata,
        artwork: Option<&[u8]>,
    ) -> Result<()> {
        if !source.is_file() {
            bail!("track source is not a regular file: {}", source.display());
        }
        self.pending.push(PendingChange::Add(Box::new(PendingAdd {
            source: source.to_path_buf(),
            metadata: metadata.clone(),
            artwork: artwork.map(<[u8]>::to_vec),
        })));
        Ok(())
    }

    /// Queues a standard playlist creation.
    pub fn create_playlist(&mut self, name: &str, tracks: &[TrackHandle]) -> Result<()> {
        validate_playlist_name(name)?;
        self.pending.push(PendingChange::CreatePlaylist {
            name: name.to_owned(),
            tracks: tracks.iter().map(|track| track.0).collect(),
        });
        Ok(())
    }

    /// Queues a standard playlist name and membership update.
    pub fn update_playlist(
        &mut self,
        playlist: PlaylistHandle,
        name: &str,
        tracks: &[TrackHandle],
    ) -> Result<()> {
        validate_playlist_name(name)?;
        let editable = self.device.library().is_some_and(|library| {
            library.playlists().iter().any(|existing| {
                existing.id == playlist.0 && !existing.is_hidden && !existing.is_smart
            })
        });
        if !editable {
            bail!("playlist is absent, hidden, or smart and cannot be updated");
        }
        self.pending.push(PendingChange::UpdatePlaylist {
            id: playlist.0,
            name: name.to_owned(),
            tracks: tracks.iter().map(|track| track.0).collect(),
        });
        Ok(())
    }

    /// Queues deletion of a standard playlist without deleting its tracks.
    pub fn delete_playlist(&mut self, playlist: PlaylistHandle) -> Result<()> {
        let editable = self.device.library().is_some_and(|library| {
            library.playlists().iter().any(|existing| {
                existing.id == playlist.0 && !existing.is_hidden && !existing.is_smart
            })
        });
        if !editable {
            bail!("playlist is absent, hidden, or smart and cannot be deleted");
        }
        self.pending.push(PendingChange::DeletePlaylist(playlist.0));
        Ok(())
    }

    /// Commits every queued change as one staged, signed, recoverable
    /// transaction. A no-op when nothing is queued.
    pub fn write(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut edit = self
            .device
            .edit()
            .context("libopod could not start an edit session")?;
        edit.set_media_policy(MediaDeletionPolicy::Delete);
        for change in std::mem::take(&mut self.pending) {
            match change {
                PendingChange::Remove(id) => {
                    edit.remove_track(id).context("queue track removal")?;
                }
                PendingChange::Add(change) => {
                    let artwork_source = match change.artwork {
                        Some(bytes) => {
                            self.artwork_sequence = self.artwork_sequence.wrapping_add(1);
                            let artwork_file = std::env::temp_dir().join(format!(
                                "copyPod-{}-{}.art",
                                std::process::id(),
                                self.artwork_sequence
                            ));
                            std::fs::write(&artwork_file, &bytes).with_context(|| {
                                format!("write temporary artwork for {}", change.source.display())
                            })?;
                            Some(artwork_file)
                        }
                        None => None,
                    };
                    let addition = TrackToAdd {
                        source_path: change.source.clone(),
                        title: change.metadata.title.clone(),
                        artist: non_empty(&change.metadata.artist),
                        album: non_empty(&change.metadata.album),
                        album_artist: non_empty(&change.metadata.album_artist),
                        genre: non_empty(&change.metadata.genre),
                        composer: None,
                        year: change.metadata.year,
                        track_number: change.metadata.track_number,
                        total_tracks: change.metadata.track_total,
                        disc_number: change.metadata.disc_number,
                        total_discs: change.metadata.disc_total,
                        bitrate: change.metadata.bitrate_kbps,
                        sample_rate: change.metadata.sample_rate_hz,
                        length_ms: change.metadata.duration_ms,
                        compilation: false,
                        media_kind: change.metadata.media_kind,
                        reuse_album_art: false,
                        artwork_source,
                    };
                    edit.add_track(addition).context("queue track addition")?;
                }
                PendingChange::CreatePlaylist { name, tracks } => {
                    edit.create_playlist(name, &tracks)
                        .context("queue playlist creation")?;
                }
                PendingChange::UpdatePlaylist { id, name, tracks } => {
                    edit.rename_playlist(id, name)
                        .context("queue playlist rename")?;
                    edit.set_playlist_tracks(id, &tracks)
                        .context("queue playlist membership update")?;
                }
                PendingChange::DeletePlaylist(id) => {
                    edit.delete_playlist(id)
                        .context("queue playlist deletion")?;
                }
            }
        }
        let staging = tempfile::tempdir().context("create staging directory")?;
        let staged = edit
            .stage_sqlite_preview(staging.path())
            .context("stage database changes")?;
        staged
            .install(&self.device)
            .context("install staged changes; rerun copyPod to retry")?;
        // The commit rewrote the device databases; refresh the cached device
        // (generation fingerprint, library) for the next write cycle.
        self.device = Device::open(&self.mountpoint).context("reopen the iPod after the commit")?;
        Ok(())
    }
}

fn profile_supports_playlists(profile_key: &str, backend: BackendKind) -> bool {
    backend == BackendKind::Binary || profile_key == "nano-7g"
}

fn validate_playlist_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("playlist name must not be empty");
    }
    Ok(())
}

fn non_empty(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::profile_supports_playlists;
    use libopod::BackendKind;

    #[test]
    fn enables_qualified_playlist_backends() {
        assert!(profile_supports_playlists("nano-3g", BackendKind::Binary));
        assert!(profile_supports_playlists(
            "nano-7g",
            BackendKind::SqliteWithBinaryCompanion
        ));
        assert!(!profile_supports_playlists(
            "future-sqlite-device",
            BackendKind::SqliteWithBinaryCompanion
        ));
    }
}
