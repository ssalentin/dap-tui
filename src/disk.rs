use std::path::{Path, PathBuf};

/// Whether `path` is currently a mount point.
///
/// Scanning `/proc/self/mountinfo` is used instead of `path.exists()` so a
/// leftover mount-point directory (e.g. `/media/sebastian/IPOD` after an
/// unclean eject) is not mistaken for a mounted device. Writing a device sync
/// into an unmounted directory would silently fill the root filesystem.
pub fn is_mounted(path: &Path) -> bool {
    let Some(target) = path.to_str() else {
        return false;
    };
    let target = target.trim_end_matches('/');

    #[cfg(target_os = "linux")]
    if let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") {
        return text.lines().any(|line| {
            let Some(field) = line.split(' ').nth(4) else {
                return false;
            };
            unescape_octal(field).trim_end_matches('/') == target
        });
    }

    // Fallback for platforms without /proc: best effort only.
    path.exists()
}

/// Mount points currently listed in the kernel mount table.
fn mount_points() -> Vec<PathBuf> {
    #[cfg(target_os = "linux")]
    if let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") {
        return text
            .lines()
            .filter_map(|line| line.split(' ').nth(4))
            .map(|field| PathBuf::from(unescape_octal(field)))
            .collect();
    }
    Vec::new()
}

/// Where the volume configured at `path` is actually mounted.
///
/// udisks mounts a volume at `/media/$USER/$LABEL`; if that directory already
/// exists (e.g. a leftover mount dir after an unclean eject) it appends a
/// number, so `/media/sebastian/IPOD` becomes `/media/sebastian/IPOD1`. This
/// resolves the exact path or the lowest-numbered suffixed sibling, so the
/// configured path is treated as a hint rather than a hard requirement.
pub fn mounted_path_for(path: &Path) -> Option<PathBuf> {
    if is_mounted(path) {
        return Some(path.to_path_buf());
    }
    let parent = path.parent()?;
    let base = path.file_name()?.to_string_lossy().to_string();
    if let Some(found) = mount_points()
        .into_iter()
        .filter_map(|mp| mount_suffix_rank(parent, &base, &mp).map(|rank| (rank, mp)))
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, mp)| mp)
    {
        return Some(found);
    }
    // The volume may have been mounted under a different parent than the
    // configured hint. udisks mounts removable media at /media/$USER/$LABEL,
    // but this machine's automount puts them at /run/media/$USER/$LABEL, so a
    // config written for /media/... would otherwise report as "not mounted"
    // (and the sidebar would show 0 mounted devices). Fall back to matching the
    // basename as a volume label anywhere in the mount table, mirroring the
    // label lookup in `udisks::resolve_block_device`.
    mounted_by_label(&base)
}

/// Find a mount point whose basename equals `label` case-insensitively,
/// preferring the highest mount (shortest path) so nested mounts do not
/// shadow a real volume.
fn mounted_by_label(label: &str) -> Option<PathBuf> {
    pick_by_label(mount_points(), label)
}

/// ASCII case-insensitive (`eq_ignore_ascii_case`, not a full Unicode
/// case-fold — FAT32/vfat labels are effectively ASCII-only, so this is
/// cheaper than `udisks::resolve_block_device`'s `to_lowercase()` without
/// behaving differently for any label this codebase actually sees), so the
/// dashboard and the sync engine never disagree about whether a device is
/// mounted just because a udev/vfat label (commonly all-uppercase) doesn't
/// match a configured `mount_point` directory's case. Takes the mount list
/// as a parameter (rather than reading `/proc/self/mountinfo` itself) so
/// the matching logic is unit-testable without a real mount table.
fn pick_by_label(mounts: Vec<PathBuf>, label: &str) -> Option<PathBuf> {
    if label.is_empty() {
        return None;
    }
    let mut matches: Vec<PathBuf> = mounts
        .into_iter()
        .filter(|mp| {
            mp.file_name()
                .map(|n| n.to_string_lossy().eq_ignore_ascii_case(label))
                .unwrap_or(false)
        })
        .collect();
    matches.sort_by_key(|p| p.components().count());
    matches.into_iter().next()
}

/// Rank of a mount point relative to a configured path: 0 for an exact match,
/// `n` for `name{n}` (udisks' uniquifying suffix), `None` otherwise.
fn mount_suffix_rank(parent: &Path, base: &str, candidate: &Path) -> Option<u32> {
    if candidate.parent() != Some(parent) {
        return None;
    }
    let name = candidate.file_name()?.to_string_lossy();
    let rest = name.strip_prefix(base)?;
    if rest.is_empty() {
        return Some(0);
    }
    if rest.chars().all(|c| c.is_ascii_digit()) {
        return rest.parse().ok();
    }
    None
}

/// Undo the octal escaping (`\040` etc.) used by /proc mount tables.
fn unescape_octal(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1..=i + 3].iter().all(|b| (b'0'..=b'7').contains(b));
        if octal {
            let v = (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
            out.push(v as char);
            i += 4;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Total and available bytes on a mounted filesystem (statvfs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskStats {
    pub total: u64,
    pub available: u64,
}

pub fn disk_stats(path: &Path) -> Option<DiskStats> {
    let c = std::ffi::CString::new(path.to_str()?).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let frsize = (s.f_frsize.max(1)) as u64;
    Some(DiskStats {
        total: s.f_blocks as u64 * frsize,
        available: s.f_bavail as u64 * frsize,
    })
}

pub fn fmt_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < 4 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::{fmt_bytes, mounted_path_for, mount_suffix_rank, pick_by_label, unescape_octal};

    #[test]
    fn matches_mount_labels_case_insensitively() {
        // A udev/vfat label is commonly all-uppercase (e.g. "IPOD") even when
        // the configured mount_point directory is not — this must still
        // resolve, mirroring udisks::resolve_block_device's own
        // case-insensitive label lookup so the dashboard and the sync engine
        // never disagree about whether a device is mounted.
        let mounts = vec![
            std::path::PathBuf::from("/media/sebastian/IPOD"),
            std::path::PathBuf::from("/media/sebastian/OTHER"),
        ];
        assert_eq!(
            pick_by_label(mounts.clone(), "ipod"),
            Some(std::path::PathBuf::from("/media/sebastian/IPOD"))
        );
        assert_eq!(
            pick_by_label(mounts.clone(), "IPOD"),
            Some(std::path::PathBuf::from("/media/sebastian/IPOD"))
        );
        assert_eq!(pick_by_label(mounts, "nope"), None);
    }

    #[test]
    fn ranks_udisks_suffixed_mounts() {
        let parent = std::path::Path::new("/media/sebastian");
        let rank = |c: &str| mount_suffix_rank(parent, "IPOD", std::path::Path::new(c));
        assert_eq!(rank("/media/sebastian/IPOD"), Some(0));
        assert_eq!(rank("/media/sebastian/IPOD1"), Some(1));
        assert_eq!(rank("/media/sebastian/IPOD12"), Some(12));
        // non-numeric suffixes and other directories must not match
        assert_eq!(rank("/media/sebastian/IPODX"), None);
        assert_eq!(rank("/media/sebastian/IPOD-2"), None);
        assert_eq!(rank("/media/other/IPOD1"), None);
        assert_eq!(rank("/media/sebastian/IPOD1/extra"), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn resolves_exact_mounts() {
        // `/` is always mounted; mounted_path_for must return it unchanged.
        assert_eq!(
            mounted_path_for(std::path::Path::new("/")),
            Some(std::path::PathBuf::from("/"))
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn resolves_mount_under_a_different_parent_by_label() {
        // udisks uses /media/$USER/$LABEL while this host's automount uses
        // /run/media/$USER/$LABEL. A config path under /media must still
        // resolve to the real mount so the sidebar shows it as mounted.
        let real = std::path::Path::new("/run/media/sebastian/SD_CARD");
        if !real.is_dir() {
            return; // card not attached in this environment
        }
        assert_eq!(
            mounted_path_for(std::path::Path::new("/media/sebastian/SD_CARD")),
            Some(std::path::PathBuf::from("/run/media/sebastian/SD_CARD"))
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn unknown_label_does_not_resolve() {
        assert_eq!(mounted_path_for(std::path::Path::new("/media/sebastian/NOPE_NOT_HERE")), None);
    }

    #[test]
    fn unescapes_mountinfo_paths() {
        assert_eq!(unescape_octal("/media/sebastian/IPOD"), "/media/sebastian/IPOD");
        assert_eq!(unescape_octal("/media/My\\040Disk"), "/media/My Disk");
        assert_eq!(unescape_octal("/media/back\\134slash"), "/media/back\\slash");
    }

    #[test]
    fn formats_bytes() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1024), "1.0 KiB");
        assert_eq!(fmt_bytes(137438953472), "128.0 GiB");
        assert_eq!(fmt_bytes(1_099_511_627_776), "1.0 TiB");
    }
}
