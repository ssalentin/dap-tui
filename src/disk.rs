use std::path::Path;

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
    use super::fmt_bytes;

    #[test]
    fn formats_bytes() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1024), "1.0 KiB");
        assert_eq!(fmt_bytes(137438953472), "128.0 GiB");
        assert_eq!(fmt_bytes(1_099_511_627_776), "1.0 TiB");
    }
}
