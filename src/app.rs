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

    fn refresh_mounts(&mut self) {
        for d in self.config.devices.iter_mut() {
            d.mounted = d
                .mount_point
                .as_ref()
                .map(|p| p.exists())
                .unwrap_or(false);
        }
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
                EngineEvent::Finished { ok, summary } => {
                    let level = if ok { LogLevel::Success } else { LogLevel::Error };
                    self.status_msg = Some((summary.clone(), level));
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
            KeyCode::Char('r') => {
                self.refresh_mounts();
                self.status_msg = Some(("devices refreshed".to_string(), LogLevel::Info));
            }
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
        const VISIBLE: usize = 12;
        if self.selected < self.sidebar_scroll {
            self.sidebar_scroll = self.selected;
        }
        if self.selected >= self.sidebar_scroll + VISIBLE {
            self.sidebar_scroll = self.selected + 1 - VISIBLE;
        }
    }

    fn scroll_logs(&mut self, delta: i32) {
        self.log_follow = false;
        let max = self.logs.len().saturating_sub(1) as i32;
        self.log_scroll = ((self.log_scroll as i32 + delta).clamp(0, max)) as usize;
    }

    fn tick(&mut self) {
        self.frame += 1;
        if self.frame % 30 == 0 {
            self.refresh_mounts();
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
