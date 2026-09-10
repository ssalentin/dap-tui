use crate::config::{self, Config, Device, Firmware};
use crate::progress::{parse_rclone_current, parse_rclone_stats, parse_rsync_line, RsyncLine};
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

    let src_path = PathBuf::from(&source);
    if src_path.is_absolute() && src_path.is_dir() {
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
    let mut args = vec![
        "sync".to_string(),
        source.clone(),
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
    args.extend(
        ["--create-empty-src-dirs", "--progress", "--stats", "1s"]
            .iter()
            .map(|s| s.to_string()),
    );

    stream(ctx, "rclone", &args, |line| {
        if let Some(p) = parse_rclone_stats(line) {
            ctx.set_mirror(p);
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

fn scan_music_dir(base: &Path) -> HashMap<String, (u64, i64)> {
    let mut map = HashMap::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
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
            let key = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().nfc().collect::<String>())
                .collect::<Vec<_>>()
                .join("/");
            if let Ok(meta) = entry.metadata() {
                let size = meta.len();
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let mtime = mtime.div_euclid(2) * 2; // FAT32 2s resolution
                map.insert(key, (size, mtime));
            }
        }
    }
    map
}

fn run_rsync_device(config: &Config, device: &Device, ctx: &WorkerCtx) -> Result<(), String> {
    let mount = device
        .mount_point
        .as_ref()
        .ok_or_else(|| "mount_point is not set for this device".to_string())?;

    if !crate::disk::is_mounted(mount) {
        ctx.log(
            LogLevel::Info,
            format!("device not mounted at {} — trying udisksctl…", mount.display()),
        );
        match crate::udisks::mount_device(mount) {
            Ok(msg) => ctx.log(LogLevel::Info, format!("auto-mounted: {msg}")),
            Err(e) => {
                return Err(format!(
                    "device not mounted at {} — connect & mount it (m) and retry. {e}",
                    mount.display()
                ));
            }
        }
    }
    // A leftover (empty) mount-point directory must not be mistaken for a
    // mounted device — rsync would otherwise happily fill the root filesystem.
    if !crate::disk::is_mounted(mount) {
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
    let src_map = scan_music_dir(&src_dir);
    ctx.log(LogLevel::Info, format!("Scanning device {}…", dest.display()));
    ctx.set_step(1, StepStatus::Active, format!("scanning device {}…", dest.display()));
    let dst_map = scan_music_dir(&dest);

    let mut to_copy: Vec<String> = Vec::new();
    for (k, (size, mtime)) in &src_map {
        let changed = dst_map
            .get(k)
            .map(|(dsize, dmtime)| dsize != size || dmtime != mtime)
            .unwrap_or(true);
        if changed {
            to_copy.push(k.clone());
        }
    }
    if device.preserve_fat_order {
        to_copy.sort();
    }
    let to_delete: Vec<String> = dst_map.keys().filter(|k| !src_map.contains_key(*k)).cloned().collect();

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
        remove_stale(&dest, &to_delete);
        ctx.log(LogLevel::Info, format!("Removed {} stale file(s).", to_delete.len()));
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

fn remove_stale(dest: &Path, to_delete: &[String]) {
    let mut actual: HashMap<String, PathBuf> = HashMap::new();
    let mut stack = vec![dest.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_s = name.to_string_lossy().to_string();
            if name_s.starts_with('.') || name_s.starts_with("._") {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if let Ok(rel) = path.strip_prefix(dest) {
                let key = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().nfc().collect::<String>())
                    .collect::<Vec<_>>()
                    .join("/");
                actual.insert(key, path);
            }
        }
    }
    for key in to_delete {
        if let Some(p) = actual.get(key) {
            if p.exists() {
                let _ = fs::remove_file(p);
                // Prune directories that are now empty, up to (not including) dest.
                let mut parent = p.parent();
                while let Some(d) = parent {
                    if d == dest || fs::remove_dir(d).is_err() {
                        break;
                    }
                    parent = d.parent();
                }
            }
        }
    }
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
    let mount = device
        .mount_point
        .as_ref()
        .ok_or_else(|| "mount_point is required to run fatsort".to_string())?;

    let target = resolve_fatsort_target(mount)?;

    ctx.log(LogLevel::Info, format!("Unmounting {mount_display}", mount_display = mount.display()));
    match crate::udisks::unmount_device(mount) {
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

    ctx.log(LogLevel::Info, format!("Running fatsort on {target}…"));
    let (code, detail) = capture("sudo", &["-n", "fatsort", &target])?;
    if code != 0 {
        return Err(format!(
            "fatsort failed (exit {code}) {detail} — run manually: sudo fatsort {target}"
        ));
    }
    Ok(())
}

fn resolve_fatsort_target(mount: &Path) -> Result<String, String> {
    if !mount.is_dir() {
        return Ok(mount.display().to_string());
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
}
