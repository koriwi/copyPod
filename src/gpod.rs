use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};

use anyhow::{anyhow, Context, Result};

#[repr(C)]
struct CpDb {
    _private: [u8; 0],
}

type CpTrack = c_void;
type CpTrackNode = c_void;

#[repr(C)]
struct CpMetadata {
    title: *const c_char,
    album: *const c_char,
    artist: *const c_char,
    album_artist: *const c_char,
    genre: *const c_char,
    comment: *const c_char,
    size: u64,
    modified_at: i64,
    duration_ms: u32,
    bitrate_kbps: u32,
    sample_rate_hz: u32,
    year: u32,
    track_number: u32,
    track_total: u32,
    disc_number: u32,
    disc_total: u32,
}

extern "C" {
    fn cp_db_open(mountpoint: *const c_char, error: *mut *mut c_char) -> *mut CpDb;
    fn cp_db_free(db: *mut CpDb);
    fn cp_db_description(db: *const CpDb) -> *mut c_char;
    fn cp_db_requires_firewire_guid(db: *const CpDb) -> c_int;
    fn cp_db_firewire_guid(db: *const CpDb) -> *mut c_char;
    fn cp_db_database_path(db: *const CpDb) -> *mut c_char;
    fn cp_db_track_count(db: *const CpDb) -> usize;
    fn cp_db_tracks(db: *const CpDb) -> *mut CpTrackNode;
    fn cp_track_node_next(node: *const CpTrackNode) -> *mut CpTrackNode;
    fn cp_track_node_track(node: *const CpTrackNode) -> *mut CpTrack;
    fn cp_track_path(track: *const CpTrack) -> *mut c_char;
    fn cp_track_title(track: *const CpTrack) -> *mut c_char;
    fn cp_track_artist(track: *const CpTrack) -> *mut c_char;
    fn cp_db_remove_track(db: *mut CpDb, track: *mut CpTrack, error: *mut *mut c_char) -> c_int;
    fn cp_db_add_track(
        db: *mut CpDb,
        source_path: *const c_char,
        metadata: *const CpMetadata,
        copied_path: *mut *mut c_char,
        error: *mut *mut c_char,
    ) -> c_int;
    fn cp_db_write(db: *mut CpDb, error: *mut *mut c_char) -> c_int;
    fn cp_string_free(value: *mut c_char);
}

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
pub struct TrackHandle(NonNull<CpTrack>);

#[derive(Debug)]
pub struct Track {
    pub handle: TrackHandle,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
}

pub struct Database {
    raw: NonNull<CpDb>,
}

impl Database {
    pub fn open(mountpoint: &Path) -> Result<Self> {
        let mountpoint = path_to_cstring(mountpoint)?;
        let mut error = ptr::null_mut();
        // SAFETY: mountpoint is a valid, NUL-terminated string and error is writable.
        let raw = unsafe { cp_db_open(mountpoint.as_ptr(), &mut error) };
        NonNull::new(raw)
            .map(|raw| Self { raw })
            .ok_or_else(|| take_error(error))
    }

    pub fn description(&self) -> String {
        // SAFETY: self owns a live CpDb.
        let value = unsafe { cp_db_description(self.raw.as_ptr()) };
        take_string(value).unwrap_or_else(|| "unknown iPod".to_owned())
    }

    pub fn requires_firewire_guid(&self) -> bool {
        // SAFETY: self owns a live CpDb.
        unsafe { cp_db_requires_firewire_guid(self.raw.as_ptr()) != 0 }
    }

    pub fn firewire_guid(&self) -> Option<String> {
        // SAFETY: self owns a live CpDb.
        let value = unsafe { cp_db_firewire_guid(self.raw.as_ptr()) };
        take_string(value)
    }

    pub fn database_path(&self) -> Result<PathBuf> {
        // SAFETY: self owns a live CpDb.
        let value = unsafe { cp_db_database_path(self.raw.as_ptr()) };
        take_string(value)
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("libgpod could not locate iTunesDB"))
    }

    pub fn track_count(&self) -> usize {
        // SAFETY: self owns a live CpDb.
        unsafe { cp_db_track_count(self.raw.as_ptr()) }
    }

    pub fn tracks(&self) -> Result<Vec<Track>> {
        // SAFETY: no database mutations occur while traversing this GList.
        let mut node = unsafe { cp_db_tracks(self.raw.as_ptr()) };
        let expected = self.track_count();
        let mut tracks = Vec::with_capacity(expected);

        while !node.is_null() {
            // SAFETY: node points to a live GList entry owned by the database.
            let raw_track = unsafe { cp_track_node_track(node) };
            let handle = NonNull::new(raw_track)
                .map(TrackHandle)
                .ok_or_else(|| anyhow!("libgpod returned an empty track entry"))?;
            // SAFETY: handle refers to a live track while self is alive and unmodified.
            let path = take_string(unsafe { cp_track_path(handle.0.as_ptr()) })
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("track has no file path"))?;
            let title =
                take_string(unsafe { cp_track_title(handle.0.as_ptr()) }).unwrap_or_default();
            let artist =
                take_string(unsafe { cp_track_artist(handle.0.as_ptr()) }).unwrap_or_default();
            tracks.push(Track {
                handle,
                path,
                title,
                artist,
            });
            // SAFETY: node remains valid because the database has not been mutated.
            node = unsafe { cp_track_node_next(node) };
        }

        Ok(tracks)
    }

    pub fn remove_track(&mut self, track: TrackHandle) -> Result<()> {
        let mut error = ptr::null_mut();
        // SAFETY: both pointers belong to this database and error is writable.
        let ok = unsafe { cp_db_remove_track(self.raw.as_ptr(), track.0.as_ptr(), &mut error) };
        call_result(ok, error)
    }

    pub fn add_track(&mut self, source: &Path, metadata: &Metadata) -> Result<PathBuf> {
        let source_c = path_to_cstring(source)?;
        let title = to_cstring(&metadata.title, "title")?;
        let album = to_cstring(&metadata.album, "album")?;
        let artist = to_cstring(&metadata.artist, "artist")?;
        let album_artist = to_cstring(&metadata.album_artist, "album artist")?;
        let genre = to_cstring(&metadata.genre, "genre")?;
        let comment = to_cstring(&metadata.comment, "comment")?;
        let raw_metadata = CpMetadata {
            title: title.as_ptr(),
            album: album.as_ptr(),
            artist: artist.as_ptr(),
            album_artist: album_artist.as_ptr(),
            genre: genre.as_ptr(),
            comment: comment.as_ptr(),
            size: metadata.size,
            modified_at: metadata.modified_at,
            duration_ms: metadata.duration_ms,
            bitrate_kbps: metadata.bitrate_kbps,
            sample_rate_hz: metadata.sample_rate_hz,
            year: metadata.year,
            track_number: metadata.track_number,
            track_total: metadata.track_total,
            disc_number: metadata.disc_number,
            disc_total: metadata.disc_total,
        };
        let mut copied_path = ptr::null_mut();
        let mut error = ptr::null_mut();
        // SAFETY: all strings and metadata remain alive for the duration of the call.
        let ok = unsafe {
            cp_db_add_track(
                self.raw.as_ptr(),
                source_c.as_ptr(),
                &raw_metadata,
                &mut copied_path,
                &mut error,
            )
        };
        call_result(ok, error)?;
        take_string(copied_path)
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("libgpod copied a track but did not return its path"))
    }

    pub fn write(&mut self) -> Result<()> {
        let mut error = ptr::null_mut();
        // SAFETY: self owns a live CpDb and error is writable.
        let ok = unsafe { cp_db_write(self.raw.as_ptr(), &mut error) };
        call_result(ok, error)
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        // SAFETY: raw was returned by cp_db_open and is freed exactly once here.
        unsafe { cp_db_free(self.raw.as_ptr()) };
    }
}

fn path_to_cstring(path: &Path) -> Result<CString> {
    let value = path
        .to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))?;
    to_cstring(value, "path")
}

fn to_cstring(value: &str, field: &str) -> Result<CString> {
    CString::new(value).with_context(|| format!("{field} contains a NUL byte"))
}

fn call_result(ok: c_int, error: *mut c_char) -> Result<()> {
    if ok != 0 {
        if !error.is_null() {
            // Do not leak an unexpected warning allocated by the C side.
            let _ = take_string(error);
        }
        Ok(())
    } else {
        Err(take_error(error))
    }
}

fn take_error(error: *mut c_char) -> anyhow::Error {
    take_string(error)
        .map(anyhow::Error::msg)
        .unwrap_or_else(|| anyhow!("unknown libgpod error"))
}

fn take_string(value: *mut c_char) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // SAFETY: C returns a NUL-terminated GLib allocation, which we free below.
    let string = unsafe { CStr::from_ptr(value) }
        .to_string_lossy()
        .into_owned();
    unsafe { cp_string_free(value) };
    Some(string)
}
