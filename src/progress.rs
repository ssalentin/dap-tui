/// Parsers for rclone and rsync progress output. Hand-rolled, no regex dep.

const AUDIO_EXTS: &[&str] = &[".m4a", ".mp3", ".aac", ".ogg", ".wav", ".wv", ".ape", ".flac", ".m4b"];

#[derive(Debug, Clone, PartialEq)]
pub enum RsyncLine {
    /// Total file count announced by rsync ("Transfer starting: N files")
    Total(u64),
    /// A per-file completion update ("… (xfr#N, to-check=M/T)")
    Progress(RsyncProgress),
    /// Live byte progress of the current file ("bytes pct% speed ETA")
    ByteProgress(RsyncProgress),
    /// A transferred audio file path (current file being handled)
    File(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RsyncProgress {
    pub done: u64,
    pub total: u64,
    pub pct: u16,
    pub speed: String,
    pub eta: String,
}

fn ends_with_audio_ext(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    AUDIO_EXTS.iter().any(|e| lower.ends_with(e))
}

fn parse_u64(s: &str) -> Option<u64> {
    s.chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

fn find_pct(s: &str) -> Option<u16> {
    for tok in s.split_whitespace() {
        if tok.ends_with('%') && tok.len() > 1 {
            if let Some(p) = tok[..tok.len() - 1].parse::<u16>().ok() {
                return Some(p.min(100));
            }
        }
    }
    None
}

fn find_speed(s: &str) -> String {
    for tok in s.split_whitespace() {
        if tok.ends_with("/s") && tok.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            return tok.to_string();
        }
    }
    String::new()
}

fn find_eta(s: &str) -> String {
    if let Some(idx) = s.find("ETA") {
        let rest = s[idx + 3..].trim();
        if !rest.is_empty() {
            return rest.to_string();
        }
    }
    String::new()
}

/// Parse a single rsync progress line.
pub fn parse_rsync_line(line: &str) -> Option<RsyncLine> {
    let line = line.trim();

    if let Some(rest) = line.strip_prefix("Transfer starting:") {
        if let Some(n) = parse_u64(rest) {
            return Some(RsyncLine::Total(n));
        }
    }

    // Per-file progress: "  512 100%  1.23MB/s  0:00:01 (xfer#12, to-check=38/200)"
    if line.contains("xfer#") || line.contains("xfr#") || line.contains("to-check=") {
        let mut done = 0;
        let mut total = 0;
        if let Some(idx) = line.find('#') {
            // take the integer immediately after '#'
            let rest: String = line[idx + 1..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(n) = rest.parse::<u64>() {
                done = n;
            }
        }
        if let Some(idx) = line.find("to-check=") {
            let rest = &line[idx + 9..];
            let mut it = rest.split(|c: char| c == '/' || c == ' ' || c == ')');
            let _remaining = it.next().and_then(parse_u64);
            if let Some(b) = it.next().and_then(parse_u64) {
                total = b; // "to-check=remaining/total" → total is second value
            }
        } else if let Some(idx) = line.find("to-chk=") {
            let rest = &line[idx + 7..];
            let mut it = rest.split(|c: char| c == '/' || c == ' ' || c == ')');
            let _remaining = it.next().and_then(parse_u64);
            if let Some(b) = it.next().and_then(parse_u64) {
                total = b;
            }
        }
        let pct = find_pct(line).unwrap_or(0);
        return Some(RsyncLine::Progress(RsyncProgress {
            done,
            total,
            pct,
            speed: find_speed(line),
            eta: find_eta(line),
        }));
    }

    // Live byte progress of the current file: " 32.768 0% 999,51MB/s 0:00:00 "
    // (no xfr# yet — rsync emits these with \r as the file transfers).
    if line.contains('%') {
        let starts_with_digit = line
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false);
        let speed = find_speed(line);
        if starts_with_digit || !speed.is_empty() {
            return Some(RsyncLine::ByteProgress(RsyncProgress {
                done: 0,
                total: 0,
                pct: find_pct(line).unwrap_or(0),
                speed,
                eta: find_eta(line),
            }));
        }
    }

    // Plain audio file path line (transferred file being processed).
    if ends_with_audio_ext(line) {
        // Skip stats-looking lines that just happen to end in an audio ext.
        if find_pct(line).is_none() && !line.starts_with(' ') {
            return Some(RsyncLine::File(line.to_string()));
        }
    }

    None
}

#[derive(Debug, Clone, PartialEq)]
pub struct RcloneStats {
    pub done: String,
    pub total: String,
    pub pct: u16,
    pub speed: String,
    pub eta: String,
}

/// Parse a rclone `--stats-one-line` "Transferred:" line.
pub fn parse_rclone_stats(line: &str) -> Option<RcloneStats> {
    let line = line.trim();
    if !line.contains(" / ") || !line.contains('%') {
        return None;
    }
    let body = line
        .split_once(':')
        .map(|(_, rest)| rest.trim())
        .unwrap_or(line);
    if !body.contains(" / ") {
        return None;
    }
    let mut parts = body.split(',');
    let sizes = parts.next()?.trim();
    let (done, total) = sizes.split_once('/')?;
    let done = done.trim().to_string();
    let total = total.trim().to_string();

    let pct = parts
        .next()
        .and_then(|p| {
            let p = p.trim().trim_end_matches('%');
            p.parse::<u16>().ok()
        })
        .unwrap_or(0)
        .min(100);

    let speed = parts.next().map(|s| s.trim().to_string()).unwrap_or_default();

    let eta = parts
        .next()
        .map(|s| s.trim().trim_start_matches("ETA").trim().to_string())
        .unwrap_or_default();

    Some(RcloneStats { done, total, pct, speed, eta })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rclone_stats_line() {
        let s = parse_rclone_stats(
            "Transferred:   512.000 MiB / 1.000 GiB,  50%, 25.00 MiB/s, ETA 20s",
        )
        .unwrap();
        assert_eq!(s.done, "512.000 MiB");
        assert_eq!(s.total, "1.000 GiB");
        assert_eq!(s.pct, 50);
        assert_eq!(s.speed, "25.00 MiB/s");
        assert_eq!(s.eta, "20s");
    }

    #[test]
    fn rejects_non_stats_line() {
        assert!(parse_rclone_stats("Elapsed time:  5.0s").is_none());
    }

    #[test]
    fn parses_rsync_progress() {
        let line = "  512 100%    1.23MB/s    0:00:01 (xfer#12, to-check=38/200)";
        match parse_rsync_line(line) {
            Some(RsyncLine::Progress(p)) => {
                assert_eq!(p.done, 12);
                assert_eq!(p.total, 200);
                assert_eq!(p.pct, 100);
                assert_eq!(p.speed, "1.23MB/s");
            }
            other => panic!("expected progress, got {other:?}"),
        }
    }

    #[test]
    fn parses_rsync_total() {
        match parse_rsync_line("Transfer starting: 150 files") {
            Some(RsyncLine::Total(n)) => assert_eq!(n, 150),
            other => panic!("expected total, got {other:?}"),
        }
    }

    #[test]
    fn parses_rsync_file_line() {
        match parse_rsync_line("Artist - Album/track1.mp3") {
            Some(RsyncLine::File(p)) => assert_eq!(p, "Artist - Album/track1.mp3"),
            other => panic!("expected file, got {other:?}"),
        }
    }

    #[test]
    fn parses_live_byte_progress_without_xfr() {
        // rsync \r segment during a large-file transfer
        match parse_rsync_line("         32.768   0%    0,00kB/s    0:00:00") {
            Some(RsyncLine::ByteProgress(p)) => {
                assert_eq!(p.pct, 0);
                assert_eq!(p.speed, "0,00kB/s");
            }
            other => panic!("expected byte progress, got {other:?}"),
        }
    }
}
