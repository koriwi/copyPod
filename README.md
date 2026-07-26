# copyPod

copyPod mirrors one or more folders of MP3s to a mounted, non-Rockbox iPod.
It skips identical files and deletes every iPod track that is not in the given
folders, including tracks added by other programs.

It is Linux-only and uses `libgpod`.

## Build

On Arch Linux:

```bash
sudo pacman -S --needed base-devel rust libgpod
cargo build --release
```

## Use

Mount the iPod first, then run:

```bash
./target/release/copyPod \
  -l ~/Musik/asmr \
  -l ~/Musik/meditation \
  -i /run/media/$USER/IPOD \
  --dry-run
```

Remove `--dry-run` to sync. `-l` can be used more than once. `-i` must be the
mount path, not `/dev/sdX`. Eject the iPod before unplugging it.
