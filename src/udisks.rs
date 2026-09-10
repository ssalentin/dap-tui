use std::path::Path;
use std::process::Command;

/// Mount/unmount devices without sudo via udisks2 (available to the active
/// desktop user). The block device is resolved from the mount point: either
/// the currently-mounted source, or a partition whose label matches the mount
/// point's basename (e.g. `/media/sebastian/IPOD` → label `IPOD`).

fn run(cmd: &str, args: &[&str]) -> (bool, String) {
    match Command::new(cmd).args(args).output() {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let detail = if !err.is_empty() { err } else { out };
            (o.status.success(), detail)
        }
        Err(e) => (false, format!("cannot run {cmd}: {e}")),
    }
}

fn resolve_block_device(path: &Path) -> Option<String> {
    // If the volume is already mounted (possibly at a udisks-suffixed path
    // like IPOD1), ask findmnt for its source device.
    if let Some(mp) = crate::disk::mounted_path_for(path) {
        let (ok, out) = run("findmnt", &["-n", "-o", "SOURCE", "-T", mp.to_str()?]);
        if ok {
            let src = out.trim();
            if src.starts_with("/dev/") {
                return Some(src.to_string());
            }
        }
    }
    // Not mounted: match a partition label to the configured mount point's
    // basename (e.g. `/media/sebastian/IPOD` → label `IPOD`).
    let wanted = path.file_name()?.to_string_lossy().to_lowercase();
    let (ok, out) = run("lsblk", &["-o", "PATH,LABEL", "-n", "-P"]);
    if !ok {
        return None;
    }
    for line in out.lines() {
        let Some(dev) = field(line, "PATH") else {
            continue;
        };
        let Some(label) = field(line, "LABEL") else {
            continue;
        };
        if !label.is_empty() && label.to_lowercase() == wanted {
            return Some(dev);
        }
    }
    None
}

/// Current mount target of a block device, if any.
fn mounted_target(dev: &str) -> Option<std::path::PathBuf> {
    let (ok, out) = run("findmnt", &["-n", "-o", "TARGET", "-S", dev]);
    if ok && !out.trim().is_empty() {
        Some(std::path::PathBuf::from(out.trim()))
    } else {
        None
    }
}

/// Extract a `KEY="value"` field from `lsblk -P` output.
fn field(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

pub fn mount_device(path: &Path) -> Result<String, String> {
    // Already mounted, possibly at a udisks-suffixed path.
    if let Some(mp) = crate::disk::mounted_path_for(path) {
        return Ok(format!("already mounted at {}", mp.display()));
    }
    let Some(dev) = resolve_block_device(path) else {
        return Err(format!("no block device found for {}", path.display()));
    };
    if let Some(mp) = mounted_target(&dev) {
        return Ok(format!("already mounted at {}", mp.display()));
    }
    let (ok, detail) = run("udisksctl", &["mount", "-b", dev.as_str()]);
    if ok {
        Ok(detail)
    } else {
        // A concurrent/late mount makes AlreadyMounted spurious: report
        // success when the kernel now has the device mounted.
        if let Some(mp) = mounted_target(&dev) {
            return Ok(format!("mounted at {}", mp.display()));
        }
        Err(format!("udisksctl mount {dev} failed: {detail}"))
    }
}

pub fn unmount_device(path: &Path) -> Result<String, String> {
    let Some(dev) = resolve_block_device(path) else {
        return Err(format!("no block device found for {}", path.display()));
    };
    let (ok, detail) = run("udisksctl", &["unmount", "-b", dev.as_str()]);
    if ok {
        Ok(detail)
    } else {
        Err(format!("udisksctl unmount {dev} failed: {detail}"))
    }
}

#[cfg(test)]
mod tests {
    use super::field;

    #[test]
    fn parses_lsblk_pairs() {
        assert_eq!(
            field(r#"PATH="/dev/sdb2" LABEL="IPOD""#, "PATH").as_deref(),
            Some("/dev/sdb2")
        );
        assert_eq!(
            field(r#"PATH="/dev/sdb2" LABEL="IPOD""#, "LABEL").as_deref(),
            Some("IPOD")
        );
        assert_eq!(
            field(r#"PATH="/dev/sdb1" LABEL="""#, "LABEL").as_deref(),
            Some("")
        );
        assert_eq!(
            field(r#"LABEL="My Disk" PATH="/dev/sdc1""#, "LABEL").as_deref(),
            Some("My Disk")
        );
        assert_eq!(field(r#"PATH="/dev/sdc1""#, "LABEL"), None);
    }

}
