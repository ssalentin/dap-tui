# dap-tui

A terminal UI for device sync of portable music players, rewritten from scratch
in Rust on top of [ratatui](https://ratatui.rs/).

It mirrors the StorageBox masters library into your local music directory, then
syncs a device profile — the same pipeline as the Python `dap-sync`, now as a
living dashboard instead of a batch CLI.

## Highlights

- **Tokyo Night** palette, rounded panels, gradient title rule, status & key
  bars.
- **Devices sidebar** — live mount status (green dot = mounted), firmware tags,
  last-sync age per device.
- **3 tabs** — Dashboard (per-player overview + settings, storage usage when
  mounted, library + sync pipeline), Sync (live step pipeline + progress
  gauge with speed/ETA, timeline of the last run), Logs (color-coded, follow
  mode, warning/error counters).
- **Responsive layout** — short terminals drop the decorative sync-path card,
  narrow cards switch to compact labels/values and ellipsize long paths
  instead of clipping them mid-word.
- **Live sync engine** — streams `rclone` and `rsync` output, parses progress
  (byte-weighted via `--info=progress2`), NFC-aware diffing for
  macOS→FAT32/Android devices, optional `fatsort` finalize for Mlove devices.
- **Abort-safe** — `a` kills the running child process group cleanly; quitting
  while a sync runs aborts it first instead of leaving it orphaned.
- **Mount-aware** — a path that exists but is not a real mount point is flagged
  and refused as a sync target, so a leftover mount directory can't silently
  receive a sync on the root filesystem.
- Reads the existing `~/.config/dap-sync/config.toml` (compatible with
  `dap-sync`), persists last-sync state to `~/.local/share/dap-sync/sync-state.toml`.

## Install / run

```bash
cargo build --release
./target/release/dap-tui            # uses ~/.config/dap-sync/config.toml
./target/release/dap-tui --config /path/to/config.toml
./target/release/dap-tui --skip-mirror   # skip the StorageBox → local mirror step

# install/update the `dap-tui` command (~/.cargo/bin) with the new build
cargo install --path . --force
dap-tui
```

## Keybindings

| Key          | Action                       |
| ------------ | ---------------------------- |
| `↑`/`↓` `j`/`k` | move selection             |
| `s` / `Enter`| sync selected device         |
| `a`          | abort running sync           |
| `m`          | mount / unmount device (udisks2, no sudo) |
| `Tab` / `1-3`| switch tab                   |
| `r`          | reload config + refresh mounts |
| `f`          | toggle log follow            |
| `u` / `d`    | scroll logs                  |
| `x`          | clear logs                   |
| `?` / `h`    | help overlay                 |
| `q` / `Esc`  | quit                         |

## Configuration

`dap-tui` reads the standard `dap-sync` config format:

```toml
[paths]
local_music_dir = "~/Music"
local_hoerspiele_dir = "~/hoerspiele"

[storagebox]
source = "storagebox:masters"
exclude = ["/Music/**", ".DS_Store", ".localized"]

[[devices]]
name = "iPod Rockbox"
firmware = "rockbox"
mount_point = "/media/sebastian/IPOD"
music_folder = "Music"
```

Firmware types: `apple` (podkit command), `rockbox`, `android`, `sony`
(rsync to a mounted volume), `mlove` (rsync + `fatsort`). Devices with
`content_type = "hoerspiele"` sync against the hoerspiele library.

## How the sync pipeline works

1. **Mirror** — `rclone sync` (or local `rsync`) pulls the StorageBox source
   into the local library.
2. **Diff** — the device is scanned and compared to the source with
   NFC-normalised paths, size, and 2-second-rounded mtimes (FAT32 friendly).
3. **Sync** — stale files are removed first, then `rsync --files-from` copies
   only what changed, with live per-file progress.
4. **Finalize** (mlove) — unmounts the volume and runs `fatsort` via
   `sudo -n` to keep FAT directory order stable. If passwordless sudo is not
   available the step is reported as a warning and the commands are printed.

## Mounting

Devices are mounted/unmounted without sudo via **udisks2**:
`m` in the TUI mounts/unmounts the selected device, and a sync automatically
mounts the target first (resolved by mount-point label via `lsblk`). Only the
mlove `fatsort` step needs root (raw block device).

Mount status and sync targets are validated against the kernel mount table
(`/proc/self/mountinfo`), not just directory existence: if the configured
`mount_point` exists but is not actually mounted, `dap-tui` refuses to sync to
it rather than writing the library onto the root filesystem.

## Development

```bash
cargo test      # parser + config tests
cargo run
```
