use crate::config::{self, Config, Device, Firmware};
use crate::progress::{parse_rclone_current, parse_rclone_listed, parse_rclone_stats, parse_rsync_line, RsyncLine};
use chrono::Local;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Transfer,
    Warn,
    Error,
    Success,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: LogLevel,
    pub msg: String,
    pub ts: chrono::DateTime<Local>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Active,
    Done,
    Error,
}

#[derive(Debug, Clone)]
pub struct StepState {
    pub title: String,
    pub detail: String,
    pub status: StepStatus,
}

#[derive(Debug, Clone, Default)]
pub struct ProgressInfo {
    pub pct: u16,
    pub done: String,
    pub total: String,
    pub speed: String,
    pub eta: String,
    pub current: String,
    /// rclone only: number of remote items listed so far (during the scan
    /// phase before `--check-first` lets transfers start).
    pub listed: u64,
}

#[derive(Debug, Clone)]
pub struct SyncState {
    pub running: bool,
    pub aborting: bool,
    pub device: Option<String>,
    pub steps: Vec<StepState>,
    pub mirror: ProgressInfo,
    pub device_prog: ProgressInfo,
    pub result: Option<bool>,
    pub summary: String,
}

impl Default for SyncState {
    fn default() -> Self {
        SyncState {
            running: false,
            aborting: false,
            device: None,
            steps: Vec::new(),
            mirror: ProgressInfo::default(),
            device_prog: ProgressInfo::default(),
            result: None,
            summary: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Log(LogEntry),
    Changed,
    Finished {
        /// Name of the device the finished sync targeted.
        device: String,
        ok: bool,
        summary: String,
    },
}

struct WorkerCtx {
    state: Arc<Mutex<SyncState>>,
    tx: mpsc::Sender<EngineEvent>,
    abort: Arc<AtomicBool>,
}

impl WorkerCtx {
    fn log(&self, level: LogLevel, msg: impl Into<String>) {
        let _ = self.tx.send(EngineEvent::Log(LogEntry {
            level,
            msg: msg.into(),
            ts: Local::now(),
        }));
    }

    fn changed(&self) {
        let _ = self.tx.send(EngineEvent::Changed);
    }

    fn set_step(&self, idx: usize, status: StepStatus, detail: impl Into<String>) {
        if let Ok(mut s) = self.state.lock() {
            if let Some(step) = s.steps.get_mut(idx) {
                step.status = status;
                step.detail = detail.into();
            }
        }
        self.changed();
    }

    fn set_mirror(&self, p: crate::progress::RcloneStats) {
        if let Ok(mut s) = self.state.lock() {
            s.mirror.pct = p.pct;
            s.mirror.done = p.done;
            s.mirror.total = p.total;
            s.mirror.speed = p.speed;
            s.mirror.eta = p.eta;
        }
        self.changed();
    }

    fn set_mirror_current(&self, path: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.mirror.current = path.to_string();
        }
        self.changed();
    }

    fn set_mirror_listed(&self, listed: u64) {
        if let Ok(mut s) = self.state.lock() {
            s.mirror.listed = listed;
        }
        self.changed();
    }

    fn set_device_progress(&self, done: u64, total: u64, speed: &str, eta: &str, current: &str, pct: Option<u16>) {
        if let Ok(mut s) = self.state.lock() {
            let p = &mut s.device_prog;
            p.done = done.to_string();
            p.total = total.to_string();
            p.speed = speed.to_string();
            p.eta = eta.to_string();
            if !current.is_empty() {
                p.current = current.to_string();
            }
            if let Some(pct) = pct {
                p.pct = pct.min(100);
            } else if total > 0 {
                p.pct = ((done as f64 / total as f64) * 100.0) as u16;
            }
        }
        self.changed();
    }

    /// Update only the "currently syncing" file display without touching the
    /// progress percentage (so a new file's start never resets the bar to 0).
    fn set_current_file(&self, path: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.device_prog.current = path.to_string();
        }
        self.changed();
    }
}

pub struct Engine {
    pub state: Arc<Mutex<SyncState>>,
    pub events: mpsc::Receiver<EngineEvent>,
    tx: mpsc::Sender<EngineEvent>,
    pub abort: Arc<AtomicBool>,
}

impl Engine {
    pub fn spawn() -> Engine {
        let (tx, events) = mpsc::channel();
        Engine {
            state: Arc::new(Mutex::new(SyncState::default())),
            events,
            tx,
            abort: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn running(&self) -> bool {
        self.state.lock().map(|s| s.running).unwrap_or(false)
    }

    pub fn start_sync(&self, device: Device, config: Arc<Config>, skip_mirror: bool) {
        // Mark the engine busy *before* spawning. Otherwise a double keypress
        // could pass the `running()` check twice and start two concurrent
        // syncs (the worker used to be the one setting `running`).
        match self.state.lock() {
            Ok(mut s) => {
                if s.running {
                    return;
                }
                s.running = true;
                s.device = Some(device.name.clone());
                s.summary = "Starting sync…".to_string();
            }
            Err(_) => return,
        }
        self.abort.store(false, Ordering::Relaxed);
        let ctx = Arc::new(WorkerCtx {
            state: self.state.clone(),
            tx: self.tx.clone(),
            abort: self.abort.clone(),
        });
        thread::spawn(move || run_sync(config, device, skip_mirror, ctx));
    }

    pub fn abort_sync(&self) {
        self.abort.store(true, Ordering::Relaxed);
    }
}

// ── sync pipeline ────────────────────────────────────────────────────────────

fn build_steps(device: &Device) -> Vec<StepState> {
    let mut steps = vec![
        StepState {
            title: "Mirror StorageBox".to_string(),
            detail: "refresh local library".to_string(),
            status: StepStatus::Pending,
        },
        StepState {
            title: format!("Sync {}", device.name),
            detail: format!("{} · {}", device.firmware.label(), device.content_type),
            status: StepStatus::Pending,
        },
    ];
    if device.firmware == Firmware::Mlove {
        steps.push(StepState {
            title: "Finalize FAT".to_string(),
            detail: "unmount + fatsort".to_string(),
            status: StepStatus::Pending,
        });
    }
    steps
}

fn run_sync(config: Arc<Config>, device: Device, skip_mirror: bool, ctx: Arc<WorkerCtx>) {
    let name = device.name.clone();
    {
        let mut s = ctx.state.lock().unwrap();
        *s = SyncState::default();
        s.running = true;
        s.device = Some(name.clone());
        s.steps = build_steps(&device);
        s.summary = "Starting sync…".to_string();
    }
    ctx.changed();
    ctx.log(LogLevel::Info, format!("── Starting sync for {}", name));

    let (ok, summary) = match run_pipeline(&config, &device, skip_mirror, &ctx) {
        Ok(()) => (true, "Sync complete".to_string()),
        Err(e) => {
            ctx.log(LogLevel::Error, e.clone());
            (false, e)
        }
    };

    // Persist first: a new sync may start as soon as `running` flips to false
    // (and reset the shared abort flag), so all shared state must be final
    // before we publish completion.
    config::save_state(&config.output_dir, &name, Local::now(), if ok { "ok" } else { "error" });
    ctx.log(
        if ok { LogLevel::Success } else { LogLevel::Error },
        if ok { format!("✓ {summary}") } else { format!("✗ {summary}") },
    );
    {
        let mut s = ctx.state.lock().unwrap();
        s.running = false;
        s.aborting = false;
        s.result = Some(ok);
        s.summary = summary.clone();
        for step in s.steps.iter_mut() {
            if step.status == StepStatus::Active {
                step.status = if ok { StepStatus::Done } else { StepStatus::Error };
            }
        }
    }
    ctx.changed();
    let _ = ctx.tx.send(EngineEvent::Finished {
        device: name.clone(),
        ok,
        summary: summary.clone(),
    });
}

fn run_pipeline(
    config: &Config,
    device: &Device,
    skip_mirror: bool,
    ctx: &WorkerCtx,
) -> Result<(), String> {
    if !skip_mirror {
        ctx.set_step(0, StepStatus::Active, "refreshing local library…");
        run_mirror(config, device, ctx)?;
        ctx.set_step(0, StepStatus::Done, "local library refreshed");
    } else {
        ctx.set_step(0, StepStatus::Done, "skipped (--skip-mirror)");
    }

    if ctx.abort.load(Ordering::Relaxed) {
        return Err("aborted".to_string());
    }

    ctx.set_step(1, StepStatus::Active, "connecting to device…");
    match device.firmware {
        Firmware::Apple => run_podkit(config, device, ctx)?,
        _ => run_rsync_device(config, device, ctx)?,
    }
    ctx.set_step(1, StepStatus::Done, "device synced");

    if device.firmware == Firmware::Mlove {
        if ctx.abort.load(Ordering::Relaxed) {
            return Err("aborted".to_string());
        }
        ctx.set_step(2, StepStatus::Active, "unmounting + running fatsort…");
        if let Err(e) = finalize_fat(device, ctx) {
            ctx.log(LogLevel::Warn, format!("finalize skipped: {e}"));
            ctx.set_step(
                2,
                StepStatus::Error,
                "device synced, but eject/fatsort needs to run manually",
            );
        } else {
            ctx.set_step(2, StepStatus::Done, "device ejected & sorted");
        }
    }

    Ok(())
}

// ── step 1: mirror StorageBox → local library ────────────────────────────────

/// Whether the mirror step copies from a local directory with `rsync` (true)
/// rather than from a remote with `rclone` (false).
///
/// A source is treated as local when it is an absolute path that currently
/// exists as a directory. This is the single source of truth for the decision
/// so the engine and the UI's SYNC PATH chain cannot disagree.
pub fn mirror_uses_rsync(source: &str) -> bool {
    let p = PathBuf::from(source);
    p.is_absolute() && p.is_dir()
}

fn run_mirror(config: &Config, device: &Device, ctx: &WorkerCtx) -> Result<(), String> {
    let (source, local_dir, label) = if device.content_type == "hoerspiele" {
        (
            config.storagebox_hoerspiele_source.clone(),
            config.local_hoerspiele_dir.clone(),
            "Hörspiele",
        )
    } else {
        (
            config.storagebox_source.clone(),
            config.local_music_dir.clone(),
            "Music",
        )
    };

    fs::create_dir_all(&local_dir).map_err(|e| format!("cannot create {}: {e}", local_dir.display()))?;

    if mirror_uses_rsync(&source) {
        ctx.log(
            LogLevel::Info,
            format!("Mirroring {label} from local source {} → {}", source, local_dir.display()),
        );
        let args = vec![
            "-a".to_string(),
            "--delete".to_string(),
            "--info=progress2,name1".to_string(),
            "--exclude=.DS_Store".to_string(),
            "--exclude=.localized".to_string(),
            format!("{}/", source),
            local_dir.display().to_string(),
        ];
        return stream(ctx, "rsync", &args, |line| {
            if let Some(l) = parse_rsync_line(line) {
                match l {
                    // overall byte-weighted progress (progress2)
                    RsyncLine::Progress(p) => {
                        if let Ok(mut s) = ctx.state.lock() {
                            s.mirror.pct = p.pct;
                            s.mirror.done = p.done.to_string();
                            s.mirror.total = p.total.to_string();
                            s.mirror.speed = p.speed.clone();
                            s.mirror.eta = p.eta.clone();
                        }
                        true
                    }
                    // per-file byte segment: only speed/eta, never the pct
                    RsyncLine::ByteProgress(p) => {
                        if let Ok(mut s) = ctx.state.lock() {
                            s.mirror.speed = p.speed.clone();
                            s.mirror.eta = p.eta.clone();
                        }
                        true
                    }
                    RsyncLine::Total(_) | RsyncLine::File(_) => true,
                }
            } else {
                false
            }
        });
    }

    ctx.log(
        LogLevel::Info,
        format!("Mirroring {label} via rclone {} → {}", source, local_dir.display()),
    );
    let args = rclone_sync_args(config, &source, &local_dir);

    stream(ctx, "rclone", &args, |line| {
        if let Some(p) = parse_rclone_stats(line) {
            ctx.set_mirror(p);
            return true;
        }
        // `Checks: … Listed N` — the remote scan phase before transfers start.
        if let Some(listed) = parse_rclone_listed(line) {
            ctx.set_mirror_listed(listed);
            return true;
        }
        if let Some(file) = parse_rclone_current(line) {
            // Per-file `Transferring:` entry — show it as the current file
            // instead of logging it once per second.
            ctx.set_mirror_current(&file);
            return true;
        }
        // Remaining progress-block lines (Checks/Elapsed/Transferring, and the
        // `Transferred: 0 / 6` file-count summary) would otherwise spam the log.
        is_rclone_progress_noise(line)
    })
}

fn is_rclone_progress_noise(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("Checks:")
        || line.starts_with("Elapsed time:")
        || line.starts_with("Transferring:")
        || line.starts_with("Transferred:")
}

/// Build the `rclone sync` argv for the StorageBox → local-library mirror.
///
/// Kept as a separate function so the throughput flags are testable: a future
/// refactor that drops them would otherwise silently bring back the slow
/// large-file behaviour, which is invisible in a quick manual run.
fn rclone_sync_args(config: &Config, source: &str, local_dir: &Path) -> Vec<String> {
    let mut args = vec![
        "sync".to_string(),
        source.to_string(),
        local_dir.display().to_string(),
    ];
    for pat in &config.storagebox_excludes {
        args.push("--exclude".to_string());
        args.push(pat.clone());
    }

    // NOTE: do NOT use `--stats-one-line` here. When stdout/stderr are not a
    // TTY (which is always the case for us — the child is piped), rclone emits
    // the one-line stats as a single carriage-return-rewritten line that only
    // surfaces once at the very end (100%). `--progress` instead emits the
    // multi-line `Transferred: … / …, N%, speed, ETA …` block newline-terminated,
    // so it streams live through the pipe and we can update the gauge in real
    // time while a cloud→local mirror is running.
    //
    // `--check-first` completes the remote listing before any transfer starts.
    // Without it (e.g. over SFTP) rclone lists while transferring, so the
    // `Transferred:` total keeps growing and the percentage stuck near 0% —
    // early ticks even showed a single file's percentage, which looked like
    // per-album progress. With the full total known up front the gauge is a
    // true overall percentage; the initial listing shows as a scan phase.
    //
    // `--checkers`/`--transfers` are raised above rclone's defaults (8/4):
    // against a high-latency SFTP remote the scan and the many-small-files
    // case are round-trip-bound, so more parallel requests directly cut
    // wall-clock time. Note these explicit flags take precedence over any
    // RCLONE_CHECKERS/RCLONE_TRANSFERS environment variables.
    //
    // Large-file throughput matters because Hörspiele are single files of
    // 50–240 MB while music tracks are ~5–8 MB, which is exactly why the
    // audiobook mirror felt so much slower than the music one:
    //
    // * `--buffer-size` is the dominant lever. rclone's per-transfer buffer
    //   defaults to 0 for a plain `sync`, so every read is a synchronous
    //   round-trip; over a ~45 ms-latency Storagebox link that pins a single
    //   file to roughly one read per RTT. A 64 MiB buffer lets rclone issue
    //   reads ahead of the writes and hide that latency. (Measured on the
    //   same 63 MiB track: 1m39s → 1m08s.)
    // * `--sftp-chunk-size` raises the SFTP payload packet from the RFC 32 KiB
    //   default to 255 KiB. Hetzner's Storagebox SFTP accepts the larger
    //   packets, which cuts the number of round-trips per megabyte ~8x.
    // * `--sftp-concurrency` (64) is rclone's default but is spelled out so a
    //   stale RCLONE_SFTP_CONCURRENCY in the environment cannot silently
    //   regress it — it bounds how many of those larger packets can be in
    //   flight for a single file.
    args.extend(
        [
            "--create-empty-src-dirs",
            "--check-first",
            "--checkers",
            "16",
            "--transfers",
            "8",
            "--buffer-size",
            "64M",
            "--sftp-chunk-size",
            "255k",
            "--sftp-concurrency",
            "64",
            "--progress",
            "--stats",
            "1s",
        ]
        .iter()
        .map(|s| s.to_string()),
    );

    args
}

// ── step 2: device sync ───────────────────────────────────────────────────────

fn run_podkit(config: &Config, device: &Device, ctx: &WorkerCtx) -> Result<(), String> {
    let cmd = device
        .sync_command
        .replace("{local_music_dir}", &config.local_music_dir.display().to_string());
    ctx.log(LogLevel::Info, format!("Running podkit: {cmd}"));
    stream(ctx, "sh", &["-c".to_string(), cmd], |line| {
        ctx.log(LogLevel::Info, line);
        true // consumed — avoid double-logging in the outer loop
    })
}

// Keyed by the NFC-normalized relative path so src/dst entries that differ
// only in Unicode normalization form still compare equal; the `String` value
// is the *real* on-disk relative path, which is what rsync must be given —
// an NFC key handed to `--files-from` fails `link_stat` if the file is
// actually stored NFD-decomposed on disk.
fn scan_music_dir(base: &Path) -> Result<HashMap<String, (String, u64, i64)>, String> {
    let mut map = HashMap::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => return Err(format!("cannot scan {}: {e}", dir.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot scan {}: {e}", dir.display()))?;
            let name = entry.file_name();
            let name_s = name.to_string_lossy().to_string();
            if name_s.starts_with('.') || name_s.starts_with("._") {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                // Never follow symlinked directories: a device can contain
                // self-referential links and we would recurse forever.
                if entry.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            let rel = match path.strip_prefix(base) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let real = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
            let key = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().nfc().collect::<String>())
                .collect::<Vec<_>>()
                .join("/");
            let meta = entry
                .metadata()
                .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
            let size = meta.len();
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let mtime = mtime.div_euclid(2) * 2; // FAT32 2s resolution
            map.insert(key, (real, size, mtime));
        }
    }
    Ok(map)
}

fn run_rsync_device(config: &Config, device: &Device, ctx: &WorkerCtx) -> Result<(), String> {
    let configured = device
        .mount_point
        .as_ref()
        .ok_or_else(|| "mount_point is not set for this device".to_string())?;

    // Trust the kernel mount table over the configured path: udisks may have
    // mounted the volume at a suffixed path (e.g. /media/.../IPOD1) because the
    // configured directory already existed.
    let mount = match crate::disk::mounted_path_for(configured) {
        Some(p) => p,
        None => {
            ctx.log(
                LogLevel::Info,
                format!(
                    "device not mounted at {} — trying udisksctl…",
                    configured.display()
                ),
            );
            match crate::udisks::mount_device(configured) {
                Ok(msg) => ctx.log(LogLevel::Info, format!("auto-mounted: {msg}")),
                Err(e) => {
                    return Err(format!(
                        "device not mounted at {} — connect & mount it (m) and retry. {e}",
                        configured.display()
                    ));
                }
            }
            crate::disk::mounted_path_for(configured).ok_or_else(|| {
                format!(
                    "auto-mount of {} did not yield a mount point",
                    configured.display()
                )
            })?
        }
    };
    // Paranoia: never write into a directory that is not actually a mount.
    if !crate::disk::is_mounted(&mount) {
        return Err(format!(
            "{} exists but is not a mount point — refusing to sync. Connect the device and press 'm'.",
            mount.display()
        ));
    }

    let (src_dir, label) = if device.content_type == "hoerspiele" {
        (config.local_hoerspiele_dir.clone(), "Hörspiele")
    } else {
        (config.local_music_dir.clone(), "Music")
    };
    let dest = mount.join(&device.music_folder);
    fs::create_dir_all(&dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;

    ctx.log(LogLevel::Info, format!("Scanning local {label} library…"));
    ctx.set_step(1, StepStatus::Active, format!("scanning local {label} library…"));
    let src_map = scan_music_dir(&src_dir)
        .map_err(|e| format!("source scan failed — refusing to sync: {e}"))?;
    ctx.log(LogLevel::Info, format!("Scanning device {}…", dest.display()));
    ctx.set_step(1, StepStatus::Active, format!("scanning device {}…", dest.display()));
    let dst_map = scan_music_dir(&dest)
        .map_err(|e| format!("device scan failed — refusing to sync: {e}"))?;

    // (key, real_path) pairs: sort by the NFC comparison key for stable FAT
    // order, but hand rsync the real on-disk path so `--files-from` resolves
    // against the actual bytes, not the normalized comparison key.
    let mut to_copy: Vec<(&String, &String)> = Vec::new();
    for (k, (real, size, mtime)) in &src_map {
        let changed = dst_map
            .get(k)
            .map(|(_, dsize, dmtime)| dsize != size || dmtime != mtime)
            .unwrap_or(true);
        if changed {
            to_copy.push((k, real));
        }
    }
    if device.preserve_fat_order {
        to_copy.sort_by(|a, b| a.0.cmp(b.0));
    }
    let to_copy: Vec<String> = to_copy.into_iter().map(|(_, real)| real.clone()).collect();
    // Delete the real on-disk paths from the scan (not the NFC keys) so
    // NFD-named files are removed by the name they actually have.
    let to_delete: Vec<String> = dst_map
        .iter()
        .filter(|(k, _)| !src_map.contains_key(*k))
        .map(|(_, (real, _, _))| real.clone())
        .collect();

    ctx.log(
        LogLevel::Info,
        format!(
            "{} source files · {} to copy · {} to remove",
            src_map.len(),
            to_copy.len(),
            to_delete.len()
        ),
    );

    if to_copy.is_empty() && to_delete.is_empty() {
        ctx.log(LogLevel::Success, "Everything up to date.");
        return Ok(());
    }

    if !to_delete.is_empty() {
        ctx.set_step(1, StepStatus::Active, format!("removing {} stale file(s)…", to_delete.len()));
        let removed = remove_stale(&dest, &to_delete)?;
        ctx.log(LogLevel::Info, format!("Removed {removed} stale file(s)."));
    }

    if !to_copy.is_empty() {
        ctx.set_step(1, StepStatus::Active, format!("copying {} file(s)…", to_copy.len()));
        let tmp = write_file_list(&to_copy)?;
        let res = {
            let args = vec![
                "-t".to_string(),
                "--files-from".to_string(),
                tmp.clone(),
                "--info=progress2".to_string(),
                "--out-format=%n".to_string(),
                "--exclude=.DS_Store".to_string(),
                "--exclude=._*".to_string(),
                format!("{}/", src_dir.display()),
                dest.display().to_string(),
            ];
            let total = to_copy.len() as u64;
            stream(ctx, "rsync", &args, |line| {
                match parse_rsync_line(line) {
                    // overall byte-weighted progress (progress2) — use directly
                    Some(RsyncLine::Progress(p)) => {
                        let done = if p.done > 0 { p.done } else { total };
                        ctx.set_device_progress(done, p.total.max(total), &p.speed, &p.eta, "", Some(p.pct));
                        true
                    }
                    // live per-file byte segment: consumed, overall % comes from
                    // the progress2 line above
                    Some(RsyncLine::ByteProgress(_)) => true,
                    Some(RsyncLine::Total(_)) => true,
                    Some(RsyncLine::File(path)) => {
                        ctx.set_current_file(&path);
                        ctx.log(LogLevel::Transfer, path);
                        true
                    }
                    None => false,
                }
            })
        };
        let _ = fs::remove_file(&tmp);
        res?;
    }

    Ok(())
}

fn remove_stale(dest: &Path, to_delete: &[String]) -> Result<usize, String> {
    let mut removed = 0;
    for rel in to_delete {
        let path = dest.join(rel);
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            // Already gone (changed between scan and delete) — nothing removed.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(format!(
                    "could not remove stale file {}: {e}",
                    path.display()
                ))
            }
        }
        // Prune directories that are now empty, up to (not including) dest.
        let mut parent = path.parent();
        while let Some(d) = parent {
            if d == dest || fs::remove_dir(d).is_err() {
                break;
            }
            parent = d.parent();
        }
    }
    Ok(removed)
}

fn write_file_list(items: &[String]) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("dap-tui-files-{}.txt", std::process::id()));
    let content = items.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
    fs::write(&path, content).map_err(|e| format!("cannot write file list: {e}"))?;
    Ok(path.display().to_string())
}

// ── mlove finalize ────────────────────────────────────────────────────────────

fn capture(cmd: &str, args: &[&str]) -> Result<(i32, String), String> {
    let out = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {cmd}: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((out.status.code().unwrap_or(-1), text))
}

fn finalize_fat(device: &Device, ctx: &WorkerCtx) -> Result<(), String> {
    let configured = device
        .mount_point
        .as_ref()
        .ok_or_else(|| "mount_point is required to run fatsort".to_string())?;
    // Use the actual mount point in case udisks moved it to a suffixed path.
    let mount = crate::disk::mounted_path_for(configured).unwrap_or_else(|| configured.clone());

    let target = resolve_fatsort_target(&mount)?;

    ctx.log(LogLevel::Info, format!("Unmounting {mount_display}", mount_display = mount.display()));
    match crate::udisks::unmount_device(&mount) {
        Ok(msg) => ctx.log(LogLevel::Info, format!("unmounted: {msg}")),
        Err(e) => {
            // fall back to sudo for setups without udisks
            let (code, detail) = capture("sudo", &["-n", "umount", mount.to_str().unwrap_or("")])?;
            if code != 0 {
                ctx.log(
                    LogLevel::Warn,
                    format!(
                        "udisks failed ({e}) and sudo -n umount failed (exit {code}) {detail} — run manually: sudo umount {mount_display}",
                        mount_display = mount.display()
                    ),
                );
            }
        }
    }

    ensure_unmounted(&target)?;

    ctx.log(LogLevel::Info, format!("Running fatsort on {target}…"));
    let (code, detail) = capture("sudo", &["-n", "fatsort", &target])?;
    if code != 0 {
        return Err(format!(
            "fatsort failed (exit {code}) {detail} — run manually: sudo fatsort {target}"
        ));
    }
    Ok(())
}

/// `fatsort` writes the raw block device, so it must never run while that
/// filesystem is still mounted — at the old path or anywhere else. Read the
/// whole mount table and look for the device there; a query that fails to
/// answer cannot prove anything, so it fails closed too.
fn ensure_unmounted(device: &str) -> Result<(), String> {
    // `-r` keeps one "source target" pair per line without the tree glyphs,
    // so the target can be quoted in the manual-action error as-is.
    let (code, table) = capture("findmnt", &["-n", "-r", "-o", "SOURCE,TARGET"])?;
    verify_unmounted(device, code, &table)
}

fn verify_unmounted(device: &str, findmnt_code: i32, table: &str) -> Result<(), String> {
    if findmnt_code != 0 {
        return Err(format!(
            "could not read the mount table to verify {device} is unmounted — refusing to run fatsort. Check mounts manually, then run: sudo fatsort {device}"
        ));
    }
    let target = table.lines().find_map(|line| {
        let (source, target) = line.split_once(' ')?;
        (source == device).then(|| target.trim())
    });
    if let Some(target) = target {
        return Err(format!(
            "{device} is still mounted at {target} after the unmount attempts — refusing to run fatsort. Run manually: sudo umount {target}"
        ));
    }
    Ok(())
}

fn resolve_fatsort_target(mount: &Path) -> Result<String, String> {
    if !mount.is_dir() {
        return Ok(mount.display().to_string());
    }
    // Never let findmnt fall back to the parent filesystem (e.g. the root
    // disk) when the directory is not actually a mount point — fatsort on the
    // wrong block device would corrupt it.
    if !crate::disk::is_mounted(mount) {
        return Err(format!("{} is not a mount point", mount.display()));
    }
    let (code, out) = capture(
        "findmnt",
        &["-n", "-o", "SOURCE", "-T", mount.to_str().unwrap_or("")],
    )?;
    if code != 0 || out.is_empty() {
        return Err(format!(
            "could not resolve a block device for {}",
            mount.display()
        ));
    }
    Ok(out)
}

// ── subprocess plumbing ───────────────────────────────────────────────────────

/// Read lines from a subprocess pipe and forward them (tagged by origin) to `tx`.
fn pump_lines<R: std::io::Read + Send + 'static>(
    mut reader: R,
    is_stderr: bool,
    tx: mpsc::Sender<(bool, String)>,
) {
    let mut buf = [0u8; 8192];
    let mut partial: Vec<u8> = Vec::new();
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                partial.extend_from_slice(&buf[..n]);
                let mut pos = 0;
                for (i, b) in partial.iter().enumerate() {
                    if *b == b'\n' || *b == b'\r' {
                        let line = String::from_utf8_lossy(&partial[pos..i]).trim().to_string();
                        if !line.is_empty() {
                            let _ = tx.send((is_stderr, line));
                        }
                        pos = i + 1;
                    }
                }
                partial.drain(0..pos);
            }
        }
    }
    if !partial.is_empty() {
        let line = String::from_utf8_lossy(&partial).trim().to_string();
        if !line.is_empty() {
            let _ = tx.send((is_stderr, line));
        }
    }
}

/// Put the child in its own process group so a whole pipeline (e.g.
/// `sh -c podkit …`, or rsync's own children) can be signalled on abort.
#[cfg(unix)]
fn detach_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
}

/// Signal the process group of `child` (SIGTERM, or SIGKILL when `force`).
fn terminate_process_group(child: &mut std::process::Child, force: bool) {
    #[cfg(unix)]
    unsafe {
        let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
        libc::kill(-(child.id() as i32), sig);
    }
    #[cfg(not(unix))]
    {
        let _ = force;
        let _ = child.kill();
    }
}

/// Run a command, streaming its output lines to `on_line`. Returns Err on
/// failure or abort. Lines returned `true` from `on_line` are treated as
/// consumed (progress); unconsumed lines are logged.
///
/// Both stdout and stderr are pumped into a shared channel so lines from
/// stderr (e.g. rclone stats) are consumed live instead of being buffered
/// until the process exits.
fn stream<F>(
    ctx: &WorkerCtx,
    cmd: &str,
    args: &[String],
    mut on_line: F,
) -> Result<(), String>
where
    F: FnMut(&str) -> bool,
{
    let mut command = Command::new(cmd);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    detach_process_group(&mut command);

    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start {cmd}: {e}"))?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let (tx, rx) = mpsc::channel::<(bool, String)>(); // (is_stderr, line)
    let out_tx = tx.clone();
    let out_thread = thread::spawn(move || pump_lines(stdout, false, out_tx));
    let err_thread = thread::spawn(move || pump_lines(stderr, true, tx));

    // On abort, ask the whole process group to terminate; escalate to SIGKILL
    // if the pipes have not closed within a grace period.
    let mut terminating: Option<std::time::Instant> = None;
    loop {
        if ctx.abort.load(Ordering::Relaxed) && terminating.is_none() {
            terminate_process_group(&mut child, false);
            terminating = Some(std::time::Instant::now());
        }
        if let Some(started) = terminating {
            if started.elapsed() > std::time::Duration::from_secs(2) {
                terminate_process_group(&mut child, true);
                break;
            }
        }
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok((is_stderr, line)) => {
                if !line.is_empty() && !on_line(&line) {
                    if is_stderr {
                        let lower = line.to_lowercase();
                        let level = if lower.contains("error") || lower.contains("failed") {
                            LogLevel::Error
                        } else {
                            LogLevel::Info
                        };
                        ctx.log(level, line);
                    } else {
                        ctx.log(LogLevel::Info, line);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = out_thread.join();
    let _ = err_thread.join();

    let status = child.wait().map_err(|e| format!("{cmd} failed: {e}"))?;
    if ctx.abort.load(Ordering::Relaxed) {
        return Err("aborted".to_string());
    }
    if !status.success() {
        return Err(format!(
            "{cmd} failed (exit {})",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn mirror_transport_follows_source_shape() {
        // An existing local absolute directory is mirrored with rsync, so the
        // UI's SYNC PATH chain must label that arrow `rsync`.
        let tmp = std::env::temp_dir().join("dap-tui-mirror-transport-test");
        fs::create_dir_all(&tmp).unwrap();
        assert!(mirror_uses_rsync(&tmp.display().to_string()));

        // An rclone remote (`storagebox:...`) or a non-existent path is not a
        // local directory, so it goes through rclone.
        assert!(!mirror_uses_rsync("storagebox:backup/masters"));
        assert!(!mirror_uses_rsync("/nonexistent/dap-tui/definitely-missing"));
        assert!(!mirror_uses_rsync("relative/path"));
    }

    #[test]
    fn stream_consumes_stderr_lines_live() {
        // rclone prints stats to stderr; they must reach on_line while the
        // process is still running (not only after exit).
        let (tx, _rx) = mpsc::channel::<EngineEvent>();
        let ctx = Arc::new(WorkerCtx {
            state: Arc::new(Mutex::new(SyncState::default())),
            tx,
            abort: Arc::new(AtomicBool::new(false)),
        });
        let args: Vec<String> = vec![
            "-c".to_string(),
            "echo 'NOTICE: first stats line'; sleep 0.5; echo 'NOTICE: second stats line'".to_string(),
        ];
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let result = stream(&ctx, "sh", &args, move |line| {
            if line.starts_with("NOTICE:") {
                seen2.lock().unwrap().push(line.to_string());
                true
            } else {
                false
            }
        });
        assert!(result.is_ok());
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "stderr lines should arrive live");
        assert!(seen[0].contains("first stats line"));
        assert!(seen[1].contains("second stats line"));
    }





    #[cfg(unix)]
    fn process_is_alive(pid: i32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            // `pid (comm) state …` — comm may itself contain spaces/parens, so
            // split at the *last* `) ` and read the state character.
            Ok(stat) => stat
                .rsplit(") ")
                .next()
                .map(|rest| !rest.starts_with('Z'))
                .unwrap_or(false),
            Err(_) => false,
        }
    }

    #[test]
    #[cfg(unix)]
    fn abort_terminates_whole_process_group() {
        // `sh -c` spawns a long-running grandchild; aborting must terminate
        // the whole group, not just the direct shell child (which used to be
        // left orphaned and running when the user pressed `a` or quit).
        let (tx, _rx) = mpsc::channel::<EngineEvent>();
        let ctx = Arc::new(WorkerCtx {
            state: Arc::new(Mutex::new(SyncState::default())),
            tx,
            abort: Arc::new(AtomicBool::new(false)),
        });
        let grandchild: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
        let capture = grandchild.clone();
        let args: Vec<String> = vec![
            "-c".to_string(),
            "sleep 30 & echo CHILD:$!; wait".to_string(),
        ];

        let abort = ctx.abort.clone();
        thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(400));
            abort.store(true, Ordering::Relaxed);
        });

        let start = std::time::Instant::now();
        let result = stream(&ctx, "sh", &args, move |line| {
            if let Some(pid) = line.strip_prefix("CHILD:") {
                *capture.lock().unwrap() = pid.trim().parse().ok();
            }
            true
        });
        assert!(result.is_err(), "aborted stream must return an error");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "abort must not hang"
        );

        let pid = grandchild.lock().unwrap().expect("grandchild pid captured");
        for _ in 0..20 {
            if !process_is_alive(pid) {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("grandchild process {pid} survived the abort");
    }

    #[test]
    fn scan_music_dir_keeps_real_path_for_nfd_filenames() {
        // On-disk name is NFD-decomposed ("o" + combining diaeresis U+0308),
        // as e.g. macOS can produce. scan_music_dir must keep the real,
        // un-normalized relative path alongside the NFC comparison key —
        // the key alone isn't a valid on-disk path for rsync to stat.
        let base = std::env::temp_dir().join(format!("dap-tui-nfc-scan-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let nfd_name = "Ho\u{0308}rspiel.mp3"; // "o" + combining diaeresis
        let nfc_name = "H\u{00f6}rspiel.mp3"; // precomposed "ö"
        assert_ne!(nfd_name, nfc_name, "sanity: the two forms are different byte sequences");
        fs::write(base.join(nfd_name), b"data").unwrap();

        let map = scan_music_dir(&base).unwrap();
        assert_eq!(map.len(), 1);
        let (key, (real, _size, _mtime)) = map.iter().next().unwrap();
        assert_eq!(key, nfc_name, "comparison key must be NFC-normalized");
        assert_eq!(real, nfd_name, "value must keep the real on-disk path, not the normalized key");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn rsync_files_from_resolves_real_path_but_not_nfc_key() {
        // Reproduces the reported failure end to end: handing rsync's
        // `--files-from` the NFC-normalized key for an NFD-named file fails
        // link_stat (the bug); handing it the real on-disk path — what
        // scan_music_dir now returns, and what run_rsync_device's to_copy
        // now contains — succeeds.
        let pid = std::process::id();
        let src = std::env::temp_dir().join(format!("dap-tui-nfc-rsync-src-{pid}"));
        let dst = std::env::temp_dir().join(format!("dap-tui-nfc-rsync-dst-{pid}"));
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        let nfd_name = "Ho\u{0308}rspiel.mp3";
        fs::write(src.join(nfd_name), b"data").unwrap();

        let src_map = scan_music_dir(&src).unwrap();
        assert_eq!(src_map.len(), 1);
        let (key, (real, _, _)) = src_map.iter().next().unwrap();
        assert_eq!(real, nfd_name);

        let run_rsync = |list: &[String]| -> std::process::Output {
            let file_list = write_file_list(list).unwrap();
            let out = Command::new("rsync")
                .args([
                    "-t",
                    "--files-from",
                    &file_list,
                    &format!("{}/", src.display()),
                    &dst.display().to_string(),
                ])
                .output()
                .unwrap();
            let _ = fs::remove_file(&file_list);
            out
        };

        // The bug: the NFC key doesn't match the real NFD bytes on disk.
        let bad = run_rsync(std::slice::from_ref(key));
        assert!(
            !bad.status.success(),
            "sanity check: the NFC key must fail link_stat against the NFD on-disk file"
        );

        // The fix: the real on-disk path transfers successfully.
        let good = run_rsync(std::slice::from_ref(real));
        assert!(
            good.status.success(),
            "real on-disk path must transfer successfully: {}",
            String::from_utf8_lossy(&good.stderr)
        );
        assert!(dst.join(nfd_name).exists(), "file must land at dest under its real name");

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
    }

    #[test]
    fn scan_fails_closed_and_allows_a_genuinely_empty_source() {
        let pid = std::process::id();
        let missing = std::env::temp_dir().join(format!("dap-tui-scan-missing-{pid}"));
        let _ = fs::remove_dir_all(&missing);
        let err = scan_music_dir(&missing).unwrap_err();
        assert!(err.contains("cannot scan"), "{err}");

        // A scan that succeeds on an empty directory must stay Ok(empty): an
        // empty library legitimately means every device file is stale, so
        // emptiness alone must not be treated as a failure.
        let empty = std::env::temp_dir().join(format!("dap-tui-scan-empty-{pid}"));
        let _ = fs::remove_dir_all(&empty);
        fs::create_dir_all(&empty).unwrap();
        assert!(scan_music_dir(&empty).unwrap().is_empty());

        let _ = fs::remove_dir_all(&empty);
    }

    #[test]
    #[cfg(unix)]
    fn scan_fails_when_a_subdirectory_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;

        // Root ignores directory permissions, so this distinction can only be
        // exercised as a regular user (as CI runs).
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        // Skipping an unreadable subtree would silently shrink the source
        // map, and the next step would treat device files missing from that
        // incomplete map as stale and delete them.
        let base = std::env::temp_dir().join(format!("dap-tui-scan-locked-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("locked")).unwrap();
        fs::write(base.join("locked/song.mp3"), b"x").unwrap();
        fs::set_permissions(base.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();

        let err = scan_music_dir(&base).unwrap_err();
        assert!(err.contains("cannot scan"), "{err}");

        let _ = fs::set_permissions(base.join("locked"), fs::Permissions::from_mode(0o755));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn rclone_sync_args_keep_the_large_file_throughput_flags() {
        // Hörspiele are single files of 50–240 MB versus ~5–8 MB music tracks,
        // so a dropped buffer/chunk setting silently makes the audiobook
        // mirror feel far slower than the music one. Pin the flags here.
        let cfg = crate::config::load(Some(
            "/nonexistent/dap-tui-test-config.toml".to_string(),
        ))
        .expect("config must fall back to defaults");
        let args = rclone_sync_args(&cfg, "storagebox:hoerspiele-flat", Path::new("/tmp/lib"));

        assert_eq!(args[0], "sync");
        assert_eq!(args[1], "storagebox:hoerspiele-flat");
        assert_eq!(args[2], "/tmp/lib");

        let has_pair = |flag: &str, value: &str| {
            args.windows(2).any(|w| w[0] == flag && w[1] == value)
        };
        assert!(has_pair("--buffer-size", "64M"), "{args:?}");
        assert!(has_pair("--sftp-chunk-size", "255k"), "{args:?}");
        assert!(has_pair("--sftp-concurrency", "64"), "{args:?}");
        assert!(has_pair("--checkers", "16"), "{args:?}");
        assert!(has_pair("--transfers", "8"), "{args:?}");
        assert!(has_pair("--stats", "1s"), "{args:?}");
        assert!(args.iter().any(|a| a == "--check-first"), "{args:?}");
        // `--stats-one-line` would collapse the live progress into one line
        // that only appears once at 100%.
        assert!(!args.iter().any(|a| a.contains("one-line")), "{args:?}");
    }

    #[test]
    fn remove_stale_reports_only_files_actually_removed() {
        let dest =
            std::env::temp_dir().join(format!("dap-tui-remove-stale-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dest);
        fs::create_dir_all(dest.join("Artist")).unwrap();
        fs::write(dest.join("Artist/gone.mp3"), b"x").unwrap();

        let removed = remove_stale(
            &dest,
            &[
                "Artist/gone.mp3".to_string(),
                "already-absent.mp3".to_string(),
            ],
        )
        .unwrap();

        assert_eq!(removed, 1);
        assert!(!dest.join("Artist/gone.mp3").exists());
        assert!(!dest.join("Artist").exists(), "empty parent must be pruned");

        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn remove_stale_fails_when_a_file_cannot_be_removed() {
        let dest = std::env::temp_dir().join(format!("dap-tui-remove-fail-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dest);
        fs::create_dir_all(dest.join("stale-dir")).unwrap();

        let err = remove_stale(&dest, &["stale-dir".to_string()]).unwrap_err();
        assert!(err.contains("could not remove stale file"), "{err}");

        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn fatsort_guard_needs_a_positive_mount_table_answer() {
        // A query that cannot answer proves nothing: refuse rather than assume
        // the device is unmounted.
        let err = verify_unmounted("/dev/sda1", 1, "").unwrap_err();
        assert!(err.contains("could not read the mount table"), "{err}");
        assert!(err.contains("refusing to run fatsort"), "{err}");

        // A successfully read table with a target for this device must refuse,
        // even when the target is not the old mount path.
        let table = "/dev/sdb1 /media/other\n/dev/sda1 /media/ipod";
        let err = verify_unmounted("/dev/sda1", 0, table).unwrap_err();
        assert!(err.contains("still mounted at /media/ipod"), "{err}");

        // A successfully read table without this device positively proves it
        // is unmounted everywhere.
        verify_unmounted("/dev/sda1", 0, "/dev/sdb1 /media/other").unwrap();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn fatsort_guard_refuses_a_live_mount() {
        // Resolve whatever `/` is mounted from, then ask the guard about that
        // source device: it must refuse while any mount target exists.
        let (code, source) = capture("findmnt", &["-n", "-o", "SOURCE", "-T", "/"]).unwrap();
        if code != 0 || source.trim().is_empty() {
            return; // no findmnt in this environment
        }
        let err = ensure_unmounted(source.trim()).unwrap_err();
        assert!(err.contains("still mounted"), "{err}");
        assert!(err.contains("refusing to run fatsort"), "{err}");

        // A device with no mount target at all may proceed to fatsort.
        assert!(ensure_unmounted("/dev/definitely-not-a-dap-tui-device").is_ok());
    }
}
