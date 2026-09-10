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

/// Parse a rclone `--progress` stats line. Two shapes are supported:
///
/// 1. `--progress` (used by the engine): the byte-weighted line
///    `Transferred: \t 20.141 MiB / 500 MiB, 4%, 20.139 MiB/s, ETA 23s`.
///    These are newline-terminated and stream live even when piped.
///
/// 2. `--stats-one-line` (legacy): rclone prefixes each line with
///    "timestamp LEVEL:" (e.g. "2026/08/22 13:27:19 NOTICE:"), which we strip
///    before parsing the comma-separated body.
///
/// The file-count summary (`Transferred: 0 / 6, 0%`) is deliberately rejected:
/// rclone emits it right after the byte-weighted line on every stats tick, and
/// accepting it reset the sync gauge to 0% each second. Likewise, rclone does
/// not newline-terminate the last `Transferring:` entry, so the next stats line
/// can arrive glued to a per-file line — we take the text after the last
/// `Transferred:` marker.
pub fn parse_rclone_stats(line: &str) -> Option<RcloneStats> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let body = match line.rsplit_once("Transferred:") {
        Some((_, rest)) => rest.trim(),
        None => line
            .split_once("NOTICE:")
            .or_else(|| line.split_once("INFO:"))
            .or_else(|| line.split_once("DEBUG:"))
            .or_else(|| line.split_once("ERROR:"))
            .map(|(_, rest)| rest.trim())
            .unwrap_or(line),
    };

    // Fast reject: the stats body always carries "x / y" and a percentage.
    if !body.contains(" / ") || !body.contains('%') {
        return None;
    }

    // A real byte-stats body starts with the transferred amount. This rejects
    // per-file `Transferring` entries such as
    // `*  a.bin: 25% / 60 MiB, 0 B/s, -`, which rclone emits right after the
    // byte line on every tick. They used to parse as 0% and reset the gauge.
    if !body.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }

    let mut parts = body.split(',');
    let sizes = parts.next()?.trim();
    let (done, total) = sizes.split_once('/')?;
    let done = done.trim();
    let total = total.trim();

    // Reject anything without byte units (the `N / M` file-count summary).
    if !has_size_unit(done) || !has_size_unit(total) {
        return None;
    }

    let pct = parts
        .next()
        .and_then(|p| p.trim().trim_end_matches('%').parse::<u16>().ok())
        .unwrap_or(0)
        .min(100);

    let speed = parts.next().map(|s| s.trim().to_string()).unwrap_or_default();

    let eta = parts
        .next()
        .map(|s| s.trim().trim_start_matches("ETA").trim().to_string())
        .unwrap_or_default();

    Some(RcloneStats {
        done: done.to_string(),
        total: total.to_string(),
        pct,
        speed,
        eta,
    })
}

/// Parse the "Listed N" counter from an rclone `Checks:` progress line:
/// `Checks:                 0 / 0, -, Listed 11810`.
/// rclone prints this while scanning the remote tree (before `--check-first`
/// lets any transfer start), so it is the only progress signal in that phase.
pub fn parse_rclone_listed(line: &str) -> Option<u64> {
    let (_, rest) = line.trim().split_once("Listed")?;
    let n = rest.split_whitespace().next()?;
    n.replace(',', "").parse().ok()
}

/// A size like `80 MiB` / `0 B` — but not a plain file count like `6`.
fn has_size_unit(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_alphabetic())
}

/// Parse a rclone `--progress` per-file transfer line and return the file path:
/// ` *                                 sub/file3.bin: 51% / 8 MiB, 0 B/s, -`.
/// The line may be glued to the following stats block, which is fine since only
/// the part up to the first `:` is used.
pub fn parse_rclone_current(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix('*')?;
    let rest = rest.trim_start();
    let (name, _stats) = rest.split_once(':')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
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
    fn parses_rclone_progress_line_with_tabs() {
        // real `rclone --progress` output when piped (non-TTY): the done column
        // is padded with spaces and tabs after the `Transferred:` label.
        let s = parse_rclone_stats(
            "Transferred:   \t  20.141 MiB / 500 MiB, 4%, 20.139 MiB/s, ETA 23s",
        )
        .unwrap();
        assert_eq!(s.done, "20.141 MiB");
        assert_eq!(s.total, "500 MiB");
        assert_eq!(s.pct, 4);
        assert_eq!(s.speed, "20.139 MiB/s");
        assert_eq!(s.eta, "23s");
    }

    #[test]
    fn parses_rclone_stats_with_log_prefix() {
        // real `rclone copy --stats-one-line --stats-log-level NOTICE` output
        let s = parse_rclone_stats(
            "2026/08/22 13:27:21 NOTICE:    13.996 MiB / 20 MiB, 70%, 5.497 MiB/s, ETA 1s",
        )
        .unwrap();
        assert_eq!(s.done, "13.996 MiB");
        assert_eq!(s.total, "20 MiB");
        assert_eq!(s.pct, 70);
        assert_eq!(s.speed, "5.497 MiB/s");
        assert_eq!(s.eta, "1s");
    }

    #[test]
    fn rejects_non_stats_line() {
        assert!(parse_rclone_stats("Elapsed time:  5.0s").is_none());
    }

    #[test]
    fn parses_live_rclone_progress_line() {
        let s = parse_rclone_stats(
            "Transferred:   \t   8.109 MiB / 80 MiB, 10%, 0 B/s, ETA -",
        )
        .unwrap();
        assert_eq!(s.done, "8.109 MiB");
        assert_eq!(s.total, "80 MiB");
        assert_eq!(s.pct, 10);
        assert_eq!(s.speed, "0 B/s");
        assert_eq!(s.eta, "-");
    }

    #[test]
    fn rejects_rclone_file_count_summary() {
        // rclone prints this right after the byte line on every stats tick;
        // accepting it used to reset the gauge to 0% each second.
        assert!(parse_rclone_stats("Transferred:            0 / 6, 0%").is_none());
        assert!(parse_rclone_stats("Transferred:            6 / 6, 100%").is_none());
    }

    #[test]
    fn parses_stats_glued_to_per_file_line() {
        // rclone does not newline-terminate the last `Transferring:` entry, so
        // the next stats block starts on the same line.
        let line = " *                                 sub/file3.bin: 51% / 8 MiB, 0 B/s, -Transferred:   \t   32.082 MiB / 80 MiB, 40%, 16.108 MiB/s, ETA 2s";
        let s = parse_rclone_stats(line).unwrap();
        assert_eq!(s.done, "32.082 MiB");
        assert_eq!(s.total, "80 MiB");
        assert_eq!(s.pct, 40);
        assert_eq!(s.speed, "16.108 MiB/s");
        assert_eq!(s.eta, "2s");
    }

    #[test]
    fn parses_rclone_listed_count() {
        assert_eq!(
            parse_rclone_listed("Checks:                 0 / 0, -, Listed 11810"),
            Some(11810)
        );
        assert_eq!(
            parse_rclone_listed("Checks:   5 / 10, 50%, Listed 1,234"),
            Some(1234)
        );
        assert!(parse_rclone_listed("Transferred: 1 MiB / 2 MiB, 50%").is_none());
        assert!(parse_rclone_listed("Elapsed time: 5.0s").is_none());
    }

    #[test]
    fn parses_rclone_current_file() {
        assert_eq!(
            parse_rclone_current(
                " *                                 sub/file3.bin: 51% / 8 MiB, 0 B/s, -"
            )
            .unwrap(),
            "sub/file3.bin"
        );
        assert_eq!(
            parse_rclone_current(" *  big.bin:  5% / 40 MiB, 2.059 MiB/s, 18s").unwrap(),
            "big.bin"
        );
        assert!(parse_rclone_current("Transferred: 1 MiB / 2 MiB, 50%, 1 MiB/s, ETA 1s").is_none());
        assert!(parse_rclone_current(" * ").is_none());
    }

    #[test]
    fn rejects_rclone_per_file_lines() {
        // Emitted every tick right after the byte line; must never reset the
        // gauge to 0%.
        assert!(parse_rclone_stats(
            " *                                         a.bin: 25% / 60 MiB, 0 B/s, -"
        )
        .is_none());
        assert!(parse_rclone_stats("*  big.bin:  5% / 40 MiB, 2.059 MiB/s, 18s").is_none());
        // The glued variant still has to yield the trailing stats block.
        let glued = "*  a.bin: 25% / 60 MiB, 0 B/s, -Transferred:   \t   60.086 MiB / 120 MiB, 50%, 30.041 MiB/s, ETA 1s";
        assert_eq!(parse_rclone_stats(glued).unwrap().pct, 50);
    }

    #[test]
    fn survives_real_rclone_progress_blocks() {
        // Captured from `rclone sync --progress --stats 1s` with a pipe: the
        // byte line is followed by the unit-less file-count summary, and the
        // last `Transferring:` entry is not newline-terminated, so the next
        // byte line is glued to it. The byte progress must survive both.
        let sample = "\
Transferred:   \t   8.109 MiB / 80 MiB, 10%, 0 B/s, ETA -
Checks:                 0 / 0, -, Listed 7
Transferred:            0 / 6, 0%
Elapsed time:         1.0s
Transferring:
 *                                       big.bin:  5% / 40 MiB, 2.059 MiB/s, 18s
 *                                 sub/file1.bin: 25% / 8 MiB, 2.058 MiB/s, 2s
 *                                 sub/file3.bin: 51% / 8 MiB, 0 B/s, -Transferred:   \t   32.082 MiB / 80 MiB, 40%, 16.108 MiB/s, ETA 2s
Checks:                 0 / 0, -, Listed 7
Transferred:            2 / 6, 33%
";
        // Only the two byte lines may parse; every per-file or count line must
        // be rejected, so the gauge can never snap back to 0 between ticks.
        let mut pcts = Vec::new();
        for line in sample.lines() {
            if let Some(p) = parse_rclone_stats(line) {
                pcts.push(p.pct);
            }
        }
        assert_eq!(pcts, vec![10, 40]);
        let last = parse_rclone_stats(sample.lines().nth(7).unwrap()).unwrap();
        assert_eq!(last.done, "32.082 MiB");
        assert_eq!(last.total, "80 MiB");
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

    #[test]
    fn parses_progress2_overall_pct() {
        // --info=progress2 line: byte-weighted overall progress
        match parse_rsync_line("    209.715.200  66%  566,48MB/s    0:00:00 (xfr#1, to-chk=1/2)") {
            Some(RsyncLine::Progress(p)) => {
                assert_eq!(p.pct, 66);
                assert_eq!(p.done, 1);
                assert_eq!(p.total, 2);
                assert_eq!(p.speed, "566,48MB/s");
            }
            other => panic!("expected progress, got {other:?}"),
        }
    }
}
