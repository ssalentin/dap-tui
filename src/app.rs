use crate::config::{Config, Device};
use crate::engine::{Engine, EngineEvent, LogLevel};
use crate::ui;
use anyhow::Result;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard = 0,
    Sync = 1,
    Logs = 2,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Dashboard, Tab::Sync, Tab::Logs];
    pub fn label(&self) -> &'static str {
        match self {
            Tab::Dashboard => "Dashboard",
            Tab::Sync => "Sync",
            Tab::Logs => "Logs",
        }
    }
    pub fn number(&self) -> usize {
        match self {
            Tab::Dashboard => 1,
            Tab::Sync => 2,
            Tab::Logs => 3,
        }
    }
}

pub struct App {
    pub config: Config,
    pub engine: Engine,
    pub skip_mirror: bool,

    pub tab: Tab,
    pub selected: usize,
    pub sidebar_scroll: usize,

    pub logs: Vec<crate::engine::LogEntry>,
    pub log_follow: bool,
    pub log_scroll: usize,
    /// Height of the log viewport as last rendered (used to scroll from the
    /// bottom when leaving follow mode).
    pub log_view: std::cell::Cell<usize>,

    pub frame: u64,
    pub should_quit: bool,
    pub show_help: bool,
    pub status_msg: Option<(String, LogLevel)>,
}

impl App {
    pub fn new(config: Config, skip_mirror: bool) -> Result<App> {
        let engine = Engine::spawn();
        Ok(App {
            config,
            engine,
            skip_mirror,
            tab: Tab::Dashboard,
            selected: 0,
            sidebar_scroll: 0,
            logs: Vec::new(),
            log_follow: true,
            log_scroll: 0,
            log_view: std::cell::Cell::new(0),
            frame: 0,
            should_quit: false,
            show_help: false,
            status_msg: None,
        })
    }

    pub fn selected_device(&self) -> Option<&Device> {
        self.config.devices.get(self.selected)
    }

    pub fn spinner(&self) -> &'static str {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        FRAMES[(self.frame as usize / 3) % FRAMES.len()]
    }

    pub fn is_syncing(&self) -> bool {
        self.engine.running()
    }

    fn drain_engine(&mut self) {
        while let Ok(ev) = self.engine.events.try_recv() {
            match ev {
                EngineEvent::Log(entry) => {
                    let is_error = matches!(entry.level, LogLevel::Error);
                    self.logs.push(entry);
                    if self.logs.len() > 2000 {
                        let overflow = self.logs.len() - 2000;
                        self.logs.drain(0..overflow);
                    }
                    if is_error {
                        self.status_msg = Some((
                            "see Logs tab (3) for details".to_string(),
                            LogLevel::Error,
                        ));
                    }
                }
                EngineEvent::Changed => {}
                EngineEvent::Finished { device, ok, summary } => {
                    // Refresh in-memory last-sync state so the dashboard shows
                    // the run that just finished instead of the stale time
                    // loaded at startup (the engine already persisted it).
                    if let Some(d) = self.config.devices.iter_mut().find(|d| d.name == device) {
                        d.last_sync = Some(chrono::Local::now());
                        d.last_status = if ok { "ok" } else { "error" }.to_string();
                    }
                    self.config.refresh_mounts();
                    let level = if ok { LogLevel::Success } else { LogLevel::Error };
                    self.status_msg = Some((summary, level));
                }
            }
        }
    }

    fn start_sync(&mut self) {
        let Some(device) = self.selected_device().cloned() else {
            self.status_msg = Some(("no device selected".to_string(), LogLevel::Warn));
            return;
        };
        if self.is_syncing() {
            return;
        }
        self.status_msg = None;
        let cfg = Arc::new(self.config.clone());
        self.engine
            .start_sync(device, cfg, self.skip_mirror);
    }

    fn abort_sync(&mut self) {
        if self.is_syncing() {
            self.engine.abort_sync();
            self.status_msg = Some(("aborting…".to_string(), LogLevel::Warn));
        }
    }

    fn toggle_mount(&mut self) {
        let Some(device) = self.selected_device().cloned() else {
            self.status_msg = Some(("no device selected".to_string(), LogLevel::Warn));
            return;
        };
        let Some(mp) = device.mount_point.clone() else {
            self.status_msg = Some(("device has no mount_point".to_string(), LogLevel::Warn));
            return;
        };
        if self.is_syncing() {
            return;
        }
        let (action, res) = if device.mounted {
            ("Unmounting", crate::udisks::unmount_device(&mp))
        } else {
            ("Mounting", crate::udisks::mount_device(&mp))
        };
        match res {
            Ok(msg) => {
                self.status_msg = Some((format!("{action}: {msg}"), LogLevel::Success));
            }
            Err(e) => {
                self.status_msg = Some((format!("{action} failed — {e}"), LogLevel::Error));
            }
        }
        self.config.refresh_mounts();
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if self.show_help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')) {
                self.show_help = false;
            }
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('?') | KeyCode::Char('h') => self.show_help = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('g') => {
                self.selected = 0;
                self.sidebar_scroll = 0;
            }
            KeyCode::Char('G') => {
                let n = self.config.devices.len().saturating_sub(1);
                self.selected = n;
                self.ensure_selection_visible();
            }
            KeyCode::Char('s') | KeyCode::Enter => self.start_sync(),
            KeyCode::Char('a') => self.abort_sync(),
            KeyCode::Char('m') => self.toggle_mount(),
            KeyCode::Char('r') => self.reload_config(),
            KeyCode::Char('f') => {
                self.log_follow = !self.log_follow;
                self.log_scroll = 0;
            }
            KeyCode::Char('x') => {
                self.logs.clear();
                self.log_scroll = 0;
            }
            KeyCode::Char('u') => self.scroll_logs(-10),
            KeyCode::Char('d') => self.scroll_logs(10),
            KeyCode::Tab => self.tab = next_tab(self.tab),
            KeyCode::BackTab => self.tab = prev_tab(self.tab),
            KeyCode::Char('1') => self.tab = Tab::Dashboard,
            KeyCode::Char('2') => self.tab = Tab::Sync,
            KeyCode::Char('3') => self.tab = Tab::Logs,
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.config.devices.is_empty() {
            return;
        }
        let n = self.config.devices.len();
        self.selected = ((self.selected as i32 + delta).rem_euclid(n as i32)) as usize;
        self.ensure_selection_visible();
    }

    fn ensure_selection_visible(&mut self) {
        // Sidebar rows available: terminal height − title(2) − rule(1) −
        // status(1) − keys(1) − sidebar head/foot(2), in 2-line device rows.
        let visible = ratatui::crossterm::terminal::size()
            .map(|(_, h)| ((h.saturating_sub(7) / 2).max(1)) as usize)
            .unwrap_or(12);
        if self.selected < self.sidebar_scroll {
            self.sidebar_scroll = self.selected;
        }
        if self.selected >= self.sidebar_scroll + visible {
            self.sidebar_scroll = self.selected + 1 - visible;
        }
    }

    fn scroll_logs(&mut self, delta: i32) {
        if self.log_follow {
            // Already pinned to the bottom: scrolling further down is a no-op,
            // while scrolling up must start from the bottom of the buffer
            // instead of jumping back to the top of the log.
            if delta > 0 {
                return;
            }
            let page = self.log_view.get().max(1);
            self.log_scroll = self.logs.len().saturating_sub(page);
            self.log_follow = false;
        }
        let max = self.logs.len().saturating_sub(1) as i32;
        self.log_scroll = ((self.log_scroll as i32 + delta).clamp(0, max)) as usize;
    }

    fn reload_config(&mut self) {
        let path = self.config.config_file.display().to_string();
        match crate::config::load(Some(path)) {
            Ok(cfg) => {
                self.config = cfg;
                if self.selected >= self.config.devices.len() {
                    self.selected = self.config.devices.len().saturating_sub(1);
                }
                self.sidebar_scroll = 0;
                self.ensure_selection_visible();
                self.status_msg = Some((
                    "config reloaded · devices refreshed".to_string(),
                    LogLevel::Info,
                ));
            }
            Err(e) => {
                self.status_msg = Some((format!("config reload failed — {e}"), LogLevel::Error));
            }
        }
    }

    fn tick(&mut self) {
        self.frame += 1;
        if self.frame % 30 == 0 {
            self.config.refresh_mounts();
        }
        if self.is_syncing() {
            let aborted = self.engine.abort.load(Ordering::Relaxed);
            if let Ok(mut s) = self.engine.state.lock() {
                s.aborting = aborted;
            }
        }
    }

    pub fn run(&mut self) -> Result<()> {
        let mut terminal = ratatui::init();
        let res = (|| -> Result<()> {
            loop {
                self.drain_engine();
                self.tick();
                terminal.draw(|f| ui::draw(f, self))?;
                if self.should_quit {
                    break;
                }
                if event::poll(Duration::from_millis(40))? {
                    if let Event::Key(key) = event::read()? {
                        self.on_key(key);
                    }
                }
            }
            Ok(())
        })();
        // Never leave a sync child process orphaned when the user quits:
        // request an abort and give the engine a moment to terminate it.
        if self.engine.running() {
            self.engine.abort_sync();
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while self.engine.running() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        ratatui::restore();
        res
    }
}

fn next_tab(t: Tab) -> Tab {
    Tab::ALL[(t as usize + 1) % Tab::ALL.len()]
}
fn prev_tab(t: Tab) -> Tab {
    Tab::ALL[(t as usize + Tab::ALL.len() - 1) % Tab::ALL.len()]
}

pub fn relative_ago(dt: chrono::DateTime<chrono::Local>) -> String {
    let now = chrono::Local::now();
    let secs = (now - dt).num_seconds().max(0);
    if secs < 5 {
        "just now".to_string()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}
