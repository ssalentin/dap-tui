pub mod components;
pub mod tabs;
pub mod theme;

use crate::app::App;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, BorderType, Clear, Paragraph};

use theme::BG;

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::default().bg(BG)), area);

    let v = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Length(1), // gradient rule
        Constraint::Min(0),    // body
        Constraint::Length(1), // status
        Constraint::Length(1), // keys
    ]);
    let [title, rule, body, status, keys] = v.areas(area);

    components::draw_title(f, title, app);
    components::draw_rule(f, rule, app);

    let [side, main] = Layout::horizontal([
        Constraint::Length(if area.width < 100 { 28 } else { 34 }),
        Constraint::Min(0),
    ])
    .areas(body);
    components::draw_sidebar(f, side, app);
    draw_main(f, main, app);

    components::draw_status(f, status, app);
    components::draw_keys(f, keys, app);

    if app.show_help {
        draw_help(f);
    }
}

fn draw_main(f: &mut Frame, area: Rect, app: &App) {
    let [tabbar, content] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    components::draw_tabbar(f, tabbar, app);
    match app.tab {
        crate::app::Tab::Dashboard => tabs::dashboard::draw(f, content, app),
        crate::app::Tab::Sync => tabs::sync::draw(f, content, app),
        crate::app::Tab::Logs => tabs::logs::draw(f, content, app),
    }
}

fn draw_help(f: &mut Frame) {
    let entries: &[(&str, &str)] = &[
        ("↑↓ / j k", "move selection"),
        ("s / Enter", "sync selected device"),
        ("a", "abort running sync"),
        ("m", "mount / unmount device (udisks)"),
        ("Tab / 1-3", "switch tab"),
        ("r", "reload config · refresh mounts"),
        ("f", "toggle log follow"),
        ("u / d", "scroll logs"),
        ("x", "clear logs"),
        ("? / h", "this help"),
        ("q / Esc", "quit"),
    ];

    let area = f.area();
    let box_w: u16 = 52.min(area.width);
    // blank + entries + blank + footer hint + two borders
    let box_h: u16 = (entries.len() as u16 + 5).min(area.height);
    let x = area.width.saturating_sub(box_w) / 2;
    let y = area.height.saturating_sub(box_h) / 2;
    let rect = Rect::new(x, y, box_w, box_h);
    f.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::ACCENT))
        .style(Style::default().bg(theme::PANEL_BG))
        .title(Span::styled(
            "  KEYBINDINGS  ",
            Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD),
        ));

    let mut lines: Vec<Line> = vec![Line::raw("")];
    for (key, desc) in entries {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{key:<11}"),
                Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(*desc, Style::default().fg(theme::FG)),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  press Esc / ? / q to close",
        Style::default().fg(theme::DIM),
    )));

    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().bg(theme::PANEL_BG)),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::config::{Config, Device, Firmware};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    fn test_app() -> App {
        let config = Config {
            config_file: PathBuf::from("/tmp/dap-tui-test/config.toml"),
            local_music_dir: PathBuf::from("/tmp/dap-tui-test/music"),
            local_hoerspiele_dir: PathBuf::from("/tmp/dap-tui-test/hoerspiele"),
            output_dir: PathBuf::from("/tmp/dap-tui-test/state"),
            storagebox_source: "storagebox:test".into(),
            storagebox_hoerspiele_source: "storagebox:test-hoerspiele".into(),
            storagebox_excludes: vec!["/Music/**".into()],
            devices: vec![Device {
                name: "Test Player".into(),
                firmware: Firmware::Rockbox,
                content_type: "music".into(),
                sync_command: "true".into(),
                mount_point: Some(PathBuf::from("/tmp/dap-tui-test/not-mounted")),
                music_folder: "Music".into(),
                preserve_fat_order: false,
                mounted: false,
                last_sync: None,
                last_status: String::new(),
            }],
        };
        App::new(config, false).unwrap()
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn renders_all_tabs_and_help_without_panicking() {
        // Small terminals used to underflow widths; the help overlay used to
        // clip its last entries because the hardcoded height was too small.
        for (w, h) in [(40u16, 10u16), (80, 24), (160, 48)] {
            let mut app = test_app();
            for tab in crate::app::Tab::ALL {
                app.tab = tab;
                let _ = render(&app, w, h);
            }
            app.show_help = true;
            let out = render(&app, w, h);
            if h >= 24 {
                assert!(out.contains("KEYBINDINGS"), "help title missing at {w}x{h}");
                assert!(out.contains("quit"), "last help entry clipped at {w}x{h}");
            }
        }
    }

    #[allow(dead_code)]
    fn ui_sample_app() -> App {
        use chrono::Duration as CDuration;
        let now = chrono::Local::now();
        let dev = |name: &str, fw: Firmware, mounted: bool, mount: &str, ago: i64, status: &str| Device {
            name: name.into(),
            firmware: fw,
            content_type: if name.contains("Mlove") { "hoerspiele" } else { "music" }.into(),
            sync_command: "podkit sync -c main".into(),
            mount_point: Some(PathBuf::from(mount)),
            music_folder: "Music".into(),
            preserve_fat_order: false,
            mounted,
            last_sync: if ago >= 0 { Some(now - CDuration::minutes(ago)) } else { None },
            last_status: status.into(),
        };
        let config = Config {
            config_file: PathBuf::from("~/.config/dap-sync/config.toml"),
            local_music_dir: PathBuf::from("/home/sebastian/music"),
            local_hoerspiele_dir: PathBuf::from("/home/sebastian/hoerspiele"),
            output_dir: PathBuf::from("/home/sebastian/.local/share/dap-sync"),
            storagebox_source: "storagebox:backup/masters".into(),
            storagebox_hoerspiele_source: "/home/sebastian/storagebox/hoerspiele".into(),
            storagebox_excludes: vec!["/Music/**".into(), ".DS_Store".into(), ".localized".into()],
            devices: vec![
                dev("iPod Rockbox", Firmware::Rockbox, true, "/", 42, "ok"),
                dev("Hifi Walker H2", Firmware::Rockbox, false, "/media/sebastian/ROCKBOX", 1440, "ok"),
                dev("ffiom21", Firmware::Android, false, "/media/sebastian/FIIOM21", -1, ""),
                dev("Sony NW-A55", Firmware::Sony, false, "/media/sebastian/SD_CARD", 2880, "error"),
                dev("Mlove Player", Firmware::Mlove, false, "/media/sebastian/MLOVE", 10080, "ok"),
            ],
        };
        App::new(config, false).unwrap()
    }

    fn dump_frame(app: &App, name: &str) {
        dump_frame_sized(app, name, 120, 34);
    }

    fn dump_frame_sized(app: &App, name: &str, w: u16, h: u16) {
        println!("\n===== {name} =====");
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        for y in 0..h {
            let mut line = String::new();
            for x in 0..w {
                line.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            println!("{}", line.trim_end());
        }
    }

    #[test]
    #[ignore = "dev helper: cargo test dump_ui -- --ignored --nocapture"]
    fn dump_ui() {
        use crate::engine::{LogEntry, LogLevel, StepState, StepStatus};

        let mut app = ui_sample_app();
        app.logs.push(LogEntry {
            level: LogLevel::Info,
            msg: "── Starting sync for iPod Rockbox".into(),
            ts: chrono::Local::now(),
        });
        app.logs.push(LogEntry {
            level: LogLevel::Transfer,
            msg: "Artist/Album/track01.m4a".into(),
            ts: chrono::Local::now(),
        });
        app.logs.push(LogEntry {
            level: LogLevel::Error,
            msg: "rsync failed (exit 23)".into(),
            ts: chrono::Local::now(),
        });

        app.tab = crate::app::Tab::Dashboard;
        dump_frame(&app, "DASHBOARD");

        app.tab = crate::app::Tab::Sync;
        dump_frame(&app, "SYNC / READY");

        {
            let mut s = app.engine.state.lock().unwrap();
            s.running = true;
            s.device = Some("iPod Rockbox".into());
            s.steps = vec![
                StepState {
                    title: "Mirror StorageBox".into(),
                    detail: "local library refreshed".into(),
                    status: StepStatus::Done,
                },
                StepState {
                    title: "Sync iPod Rockbox".into(),
                    detail: "copying 12 file(s)…".into(),
                    status: StepStatus::Active,
                },
                StepState {
                    title: "Finalize FAT".into(),
                    detail: "unmount + fatsort".into(),
                    status: StepStatus::Pending,
                },
            ];
            s.device_prog.pct = 42;
            s.device_prog.done = "1.2 GiB".into();
            s.device_prog.total = "2.9 GiB".into();
            s.device_prog.speed = "18.4 MiB/s".into();
            s.device_prog.eta = "1m 30s".into();
            s.device_prog.current = "Artist/Album/track07.flac".into();
        }
        dump_frame(&app, "SYNC / RUNNING");

        {
            let mut s = app.engine.state.lock().unwrap();
            s.running = false;
            s.result = Some(true);
            s.summary = "Sync complete".into();
            s.steps[1].status = StepStatus::Done;
            s.steps[1].detail = "device synced".into();
            s.steps[2].status = StepStatus::Done;
            s.steps[2].detail = "device ejected & sorted".into();
        }
        dump_frame(&app, "SYNC / FINISHED");

        app.tab = crate::app::Tab::Logs;
        dump_frame(&app, "LOGS");

        app.tab = crate::app::Tab::Sync;
        dump_frame_sized(&app, "SYNC / FINISHED 80x24", 80, 24);
        app.tab = crate::app::Tab::Dashboard;
        dump_frame_sized(&app, "DASHBOARD 80x24", 80, 24);
    }

    #[test]
    fn renders_live_cloud_mirror_progress() {
        use crate::engine::{StepState, StepStatus};

        let mut app = test_app();
        {
            let mut s = app.engine.state.lock().unwrap();
            s.running = true;
            s.device = Some("Test Player".into());
            s.steps = vec![
                StepState {
                    title: "Mirror StorageBox".into(),
                    detail: "refresh local library".into(),
                    status: StepStatus::Active,
                },
                StepState {
                    title: "Sync Test Player".into(),
                    detail: "rockbox · music".into(),
                    status: StepStatus::Pending,
                },
            ];
            s.mirror.pct = 40;
            s.mirror.done = "32.082 MiB".into();
            s.mirror.total = "80 MiB".into();
            s.mirror.speed = "16.108 MiB/s".into();
            s.mirror.eta = "2s".into();
            s.mirror.current = "sub/file3.bin".into();
        }
        app.tab = crate::app::Tab::Sync;

        let out = render(&app, 100, 30);
        assert!(out.contains("Mirroring local library"), "mirror label missing");
        assert!(out.contains("40%"), "percentage missing");
        assert!(out.contains("32.082 MiB"), "transferred bytes missing");
        assert!(out.contains("sub/file3.bin"), "current file missing");
    }

    #[test]
    fn renders_mirror_scanning_phase() {
        use crate::engine::{StepState, StepStatus};

        let mut app = test_app();
        {
            let mut s = app.engine.state.lock().unwrap();
            s.running = true;
            s.device = Some("Test Player".into());
            s.steps = vec![
                StepState {
                    title: "Mirror StorageBox".into(),
                    detail: "refreshing local library…".into(),
                    status: StepStatus::Active,
                },
                StepState {
                    title: "Sync Test Player".into(),
                    detail: "rockbox · music".into(),
                    status: StepStatus::Pending,
                },
            ];
            // --check-first listing phase: no bytes moved, total still growing.
            s.mirror.pct = 0;
            s.mirror.done = "0 B".into();
            s.mirror.total = "113.477 GiB".into();
            s.mirror.listed = 11810;
        }
        app.tab = crate::app::Tab::Sync;

        let out = render(&app, 100, 30);
        assert!(out.contains("Scanning remote"), "scan label missing");
        assert!(out.contains("11,810"), "listed count missing");
        assert!(out.contains("113.477 GiB"), "pending total missing");
        assert!(
            !out.contains("Mirroring local library"),
            "scan phase must replace the 0% mirror label"
        );
    }
}
