# copyPod

> [!CAUTION]
> **This project is vibecoded. Back up your iPod and use it at your own risk.**

copyPod is a small Linux command-line tool for putting MP3s on an iPod running
Apple's original firmware. Give it one or more music folders and the iPod's
mount point; copyPod scans the folders recursively and makes the iPod's music
library match them.

> [!WARNING]
> copyPod is a full-mirror tool. Tracks that do not match anything in the given
> folders are removed from the iPod, even if they were added with Rhythmbox,
> iTunes, or another program. Run with `--dry-run` first.

## Features

- Recursively syncs any number of folders
- Skips existing tracks without reading and hashing every file on the iPod
- Matches tracks using tags, exact file size, duration, and track/disc numbers
- Reads cover art embedded in MP3s
- Falls back to `cover`, `folder`, or `front` JPEG/PNG files next to the MP3
- Adds missing artwork to existing tracks without copying the audio again
- Checks for the FireWire GUID required by affected Nano and Classic models
- Uses the authoritative SQLite library on supported modern Nanos

This migration branch uses the Rust `libopod` crate and has no libgpod, GLib,
project C shim, or pkg-config integration. Dry-run planning works on the Nano
7G profile. Real synchronization currently stops at a read-only preflight until
libopod's staged multi-file writer and recovery support are complete.

## Requirements

- Linux
- Rust toolchain
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

## Build

```bash
# Place the libopod and copyPod checkouts next to each other:
# parent/libopod and parent/copyPod
git clone https://github.com/koriwi/copyPod.git
cd copyPod
cargo build --release
```

The binary is written to `target/release/copyPod`. To install it for your user:

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

`-l/--library` may be supplied more than once. Each folder is scanned
recursively; its directory layout is not copied to the iPod.

`-i/--ipod` must be the mounted filesystem path, **not** `/dev/sdX`.

## Track matching

Instead of hashing the audio on the iPod, copyPod compares information already
stored in the iPod's authoritative library with the local MP3:

- artist or album artist
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

Names are matched case-insensitively. Artwork writes will resume when libopod's
profile-aware ArtworkDB/ithmb writer is enabled.

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

This migration branch currently refuses every non-dry-run operation before
changing media or database files. This is intentional: modern Nanos require a
recoverable commit across the SQLite library, CBK signature, binary companion,
and artwork files. Always eject or unmount the iPod before unplugging it.

## Current limitations

- MP3 only; no transcoding
- No playlist creation
- No video or podcast handling
- Full-mirror synchronization only
