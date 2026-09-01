# copyPod

> [!CAUTION]
> **This project is vibecoded. Back up your iPod and use it at your own risk.**

copyPod is a small command-line tool for putting MP3s on an iPod running
Apple's original firmware. Give it music folders, M3U playlists, or both plus
the iPod's mount point; copyPod makes the iPod's music library match the union
of those sources.

> [!WARNING]
> copyPod is a full-mirror tool. Tracks that do not match anything in the given
> sources are removed from the iPod, even if they were added with Rhythmbox,
> iTunes, or another program. Run with `--dry-run` first.

## Features

- Recursively syncs any number of folders
- Syncs only the tracks referenced by selected `.m3u` and `.m3u8` files
- Accepts individual playlists or recursively scanned playlist folders
- Creates and updates standard iPod playlists
- Skips existing tracks without reading and hashing every file on the iPod
- Matches tracks using tags, exact file size, duration, and track/disc numbers
- Reads cover art embedded in MP3s
- Falls back to `cover`, `folder`, or `front` JPEG/PNG files next to the MP3
- Adds missing artwork to existing tracks without copying the audio again
- Checks for the FireWire GUID required by affected Nano and Classic models
- Uses the authoritative SQLite library on supported modern Nanos

copyPod uses the Rust `libopod` crate and has no libgpod, GLib, project C shim,
or pkg-config integration. libopod stages and installs database changes as a
recoverable transaction on supported iPod profiles.

## Requirements

- Linux, macOS, or Windows
- Rust toolchain (when building from source)
- A sibling checkout of the in-development `libopod` crate
- A mounted, non-Rockbox iPod with a libopod read adapter
- MP3 files; other audio formats are not supported yet

### Arch Linux

```bash
sudo pacman -S --needed base-devel rust
```

### Debian or Ubuntu

```bash
sudo apt install build-essential cargo
```

## Downloads

Each pushed revision publishes release archives for Linux x86_64, Windows
x86_64, macOS Apple Silicon, and macOS Intel on the GitHub Releases page. Each
archive has a matching SHA-256 checksum file.

## Build

```bash
# Place the libopod and copyPod checkouts next to each other:
# parent/libopod and parent/copyPod
git clone https://github.com/koriwi/libopod.git
git clone https://github.com/koriwi/copyPod.git
cd copyPod
cargo build --release
```

The binary is written to `target/release/copyPod` (`copyPod.exe` on Windows).
To install it for your user on Linux or macOS:

```bash
install -Dm755 target/release/copyPod ~/.local/bin/copyPod
```

## Usage

First mount the iPod through your file manager. Then preview the sync:

```bash
copyPod \
  -l ~/Musik/asmr \
  -l ~/Musik/meditation \
  -i /run/media/$USER/IPOD \
  --dry-run
```

If the plan looks right, run the same command without `--dry-run`:

```bash
copyPod \
  -l ~/Musik/asmr \
  -l ~/Musik/meditation \
  -i /run/media/$USER/IPOD
```

At least one `-l/--library` or `-p/--playlist` source is required. Both options
may be supplied more than once and may be combined:

```bash
copyPod \
  -l ~/Musik/albums \
  -p ~/Musik/playlists/favorites.m3u \
  -p ~/Musik/playlists/portable \
  -i /run/media/$USER/IPOD \
  --dry-run
```

Each `-l` folder is scanned recursively and contributes every MP3 it contains;
its directory layout is not copied to the iPod. Each `-p` path can be an M3U or
M3U8 file, or a directory scanned recursively for playlists. A selected
playlist contributes only its referenced MP3s. When both options are used,
copyPod mirrors the union of their tracks, with duplicate tracks copied once.

M3U and M3U8 files become standard iPod playlists. The playlist filename
(without its extension) becomes the playlist name; copyPod removes the leading
`000 ` sorting prefix used by rocksonic-rs and cleans up obsolete prefixed
copies already on the iPod. Files must use UTF-8. Entries may use absolute paths
or paths relative to the M3U file; blank lines, comments, and extended-M3U
metadata lines are ignored. Tracks in explicitly selected `-p` playlists may be
anywhere on disk. Missing, unreadable, and non-MP3 entries are errors. Existing
standard playlists with the same name are updated, while unrelated iPod
playlists are preserved. Playlist writes work on the Nano 7G and classic
binary-iTunesDB models supported by libopod.

`-i/--ipod` must be the mounted filesystem path, **not** `/dev/sdX`.

## Track matching

Instead of hashing the audio on the iPod, copyPod compares information already
stored in the iPod's authoritative library with the local MP3:

- track artist (falling back to album artist when track artist is missing)
- album and title
- exact file size
- duration, rounded to a second
- track and disc number

Tag text is compared case-insensitively and extra whitespace is ignored. A
missing title falls back to the filename; missing artist and album tags become
`Unknown Artist` and `Unknown Album`.

## Cover art

For each MP3, copyPod looks for artwork in this order:

1. Embedded front cover, or the first embedded picture
2. `cover.jpg`, `cover.jpeg`, or `cover.png`
3. `folder.jpg`, `folder.jpeg`, or `folder.png`
4. `front.jpg`, `front.jpeg`, or `front.png`

Names are matched case-insensitively. Artwork is written only when the detected
device profile supports it.

## FireWire GUID

Some models, including the iPod Nano 3G, require a device-specific FireWire
GUID to sign `iTunesDB`. The name is historical; it is also required when the
iPod is connected over USB.

copyPod checks for the GUID before syncing and stops with instructions if it is
missing or malformed. To initialize it, identify the iPod carefully:

```bash
lsblk -o NAME,SIZE,MODEL,SERIAL,FSTYPE,MOUNTPOINTS
```

Until libopod gains its own identity collection helper, use the existing helper
with the verified **whole device** and mount point:

```bash
sudo ipod-read-sysinfo-extended /dev/sdX /run/media/$USER/IPOD
```

The `/dev/sdX` path is only used by this initialization helper. Never pass it
to copyPod.

## Safety

libopod stages database and media changes and installs them as a recoverable
transaction. If copyPod finds an interrupted transaction, it asks before
recovery; a dry run never performs recovery. Do not rename or manually modify
`.libopod-transaction-v1`.

Before planning a mirror, copyPod audits database media references. A missing
file is removed from the database and recopied when it is still present in the
source library; obsolete dangling references are removed without a recopy.
These repairs appear explicitly in normal and dry-run output.

Even so, keep a backup and run with `--dry-run` before the first sync. Always
eject or unmount the iPod before unplugging it.

## Current limitations

- MP3 only; no transcoding
- Playlist writes require a Nano 7G or classic binary-iTunesDB model supported by libopod
- No video or podcast handling
- Full-mirror synchronization only
