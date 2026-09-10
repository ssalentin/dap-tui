use std::path::Path;

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
    use super::{fmt_bytes, unescape_octal};

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
