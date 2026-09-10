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
    // Only trust findmnt when the path really is a mount point; otherwise
    // findmnt happily reports the containing filesystem (e.g. /), which would
    // make us "mount" the root device.
    if crate::disk::is_mounted(path) {
        let (ok, out) = run("findmnt", &["-n", "-o", "SOURCE", "-T", path.to_str()?]);
        if ok {
            let src = out.trim();
            if src.starts_with("/dev/") {
                return Some(src.to_string());
            }
        }
    }
    let wanted = path.file_name()?.to_string_lossy().to_lowercase();
    let (ok, out) = run("lsblk", &["-o", "PATH,LABEL", "-n", "-l", "-P"]);
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

/// Extract a `KEY="value"` field from `lsblk -P` output.
fn field(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

pub fn mount_device(path: &Path) -> Result<String, String> {
    let Some(dev) = resolve_block_device(path) else {
        return Err(format!("no block device found for {}", path.display()));
    };
    let (ok, detail) = run("udisksctl", &["mount", "-b", dev.as_str()]);
    if ok {
        Ok(detail)
    } else {
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
