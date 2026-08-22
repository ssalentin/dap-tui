use crate::config::{self, Config, Device, Firmware};
use crate::progress::{parse_rclone_stats, parse_rsync_line, RsyncLine};
use chrono::Local;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
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

    fn set_device_progress(&self, done: u64, total: u64, speed: &str, eta: &str, current: &str) {
        if let Ok(mut s) = self.state.lock() {
            let p = &mut s.device_prog;
            p.done = done.to_string();
            p.total = total.to_string();
            p.speed = speed.to_string();
            p.eta = eta.to_string();
            if !current.is_empty() {
                p.current = current.to_string();
            }
            if total > 0 {
                p.pct = ((done as f64 / total as f64) * 100.0) as u16;
            }
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
        if self.running() {
            return;
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
    ctx.log(
        if ok { LogLevel::Success } else { LogLevel::Error },
        if ok { format!("✓ {summary}") } else { format!("✗ {summary}") },
    );
    ctx.changed();
    let _ = ctx.tx.send(EngineEvent::Finished {
        ok,
        summary: summary.clone(),
    });

    config::save_state(&config.output_dir, &name, Local::now(), if ok { "ok" } else { "error" });
}

fn run_pipeline(
    config: &Config,
    device: &Device,
    skip_mirror: bool,
    ctx: &WorkerCtx,
) -> Result<(), String> {
    let steps = build_steps(device);

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

    let _ = steps;
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
    args.extend(
        [
            "--create-empty-src-dirs",
            "--stats",
            "1s",
            "--stats-one-line",
            "--stats-log-level",
            "NOTICE",
        ]
        .iter()
        .map(|s| s.to_string()),
    );

    stream(ctx, "rclone", &args, |line| {
        if let Some(p) = parse_rclone_stats(line) {
            ctx.set_mirror(p);
            true
        } else {
            false
        }
    })
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
    if !mount.exists() {
        return Err(format!(
            "device not found at {} — connect & mount it, then try again",
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
    let src_map = scan_music_dir(&src_dir);
    ctx.log(LogLevel::Info, format!("Scanning device {}…", dest.display()));
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
        remove_stale(&dest, &to_delete);
        ctx.log(LogLevel::Info, format!("Removed {} stale file(s).", to_delete.len()));
    }

    if !to_copy.is_empty() {
        let tmp = write_file_list(&to_copy)?;
        let res = {
            let args = vec![
                "-t".to_string(),
                "--files-from".to_string(),
                tmp.clone(),
                "--progress".to_string(),
                "--exclude=.DS_Store".to_string(),
                "--exclude=._*".to_string(),
                format!("{}/", src_dir.display()),
                dest.display().to_string(),
            ];
            let total = to_copy.len() as u64;
            stream(ctx, "rsync", &args, |line| {
                match parse_rsync_line(line) {
                    Some(RsyncLine::Progress(p)) => {
                        let done = if p.done > 0 { p.done } else { total };
                        ctx.set_device_progress(done, total, &p.speed, &p.eta, "");
                        true
                    }
                    Some(RsyncLine::Total(_)) => true,
                    Some(RsyncLine::File(path)) => {
                        ctx.set_device_progress(0, total, "", "", &path);
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
                if let Some(parent) = p.parent() {
                    let _ = fs::remove_dir(parent); // remove if now empty
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
    let (code, detail) = capture("sudo", &["-n", "umount", mount.to_str().unwrap_or("")])?;
    if code != 0 {
        ctx.log(
            LogLevel::Warn,
            format!(
                "sudo -n umount failed (exit {code}) {detail} — run manually: sudo umount {mount_display}",
                mount_display = mount.display()
            ),
        );
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

/// Run a command, streaming its output lines to `on_line`. Returns Err on
/// failure or abort. Lines returned `true` from `on_line` are treated as
/// consumed (progress); unconsumed lines are logged.
fn stream<F>(
    ctx: &WorkerCtx,
    cmd: &str,
    args: &[String],
    mut on_line: F,
) -> Result<(), String>
where
    F: FnMut(&str) -> bool,
{
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start {cmd}: {e}"))?;

    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();

    let (err_tx, err_rx) = mpsc::channel::<String>();
    let err_handle = thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut partial: Vec<u8> = Vec::new();
        loop {
            match stderr.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    partial.extend_from_slice(&buf[..n]);
                    let mut pos = 0;
                    for (i, b) in partial.iter().enumerate() {
                        if *b == b'\n' || *b == b'\r' {
                            let line = String::from_utf8_lossy(&partial[pos..i]).trim().to_string();
                            if !line.is_empty() {
                                let _ = err_tx.send(line);
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
                let _ = err_tx.send(line);
            }
        }
    });

    let mut buf = [0u8; 8192];
    let mut partial: Vec<u8> = Vec::new();
    loop {
        if ctx.abort.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("aborted".to_string());
        }
        match stdout.read(&mut buf) {
            Ok(0) => break,
            Err(_) => break,
            Ok(n) => {
                partial.extend_from_slice(&buf[..n]);
                let mut pos = 0;
                for (i, b) in partial.iter().enumerate() {
                    if *b == b'\n' || *b == b'\r' {
                        let line = String::from_utf8_lossy(&partial[pos..i]).trim().to_string();
                        if !line.is_empty() && !on_line(&line) {
                            ctx.log(LogLevel::Info, line);
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
        if !line.is_empty() && !on_line(&line) {
            ctx.log(LogLevel::Info, line);
        }
    }

    let _ = err_handle.join();
    for line in err_rx.try_iter() {
        if !line.is_empty() && !on_line(&line) {
            let lower = line.to_lowercase();
            let level = if lower.contains("error") || lower.contains("failed") {
                LogLevel::Error
            } else {
                LogLevel::Info
            };
            ctx.log(level, line);
        }
    }

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
