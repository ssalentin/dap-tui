use crate::app::{relative_ago, App};
use crate::config::Firmware;
use crate::disk::{disk_stats, fmt_bytes};
use crate::ui::theme;
use crate::ui::truncate;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, BorderType, Paragraph};

fn card(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::DIM))
        .style(Style::default().bg(theme::PANEL_BG))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ))
}

fn kv<'a>(label: &str, value: String, value_style: Style) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("  {label:<14}"), Style::default().fg(theme::DIM)),
        Span::styled(value, value_style),
    ])
}

fn firmware_color(fw: &Firmware) -> ratatui::style::Color {
    match fw {
        Firmware::Rockbox => theme::MAGENTA,
        Firmware::Android => theme::CYAN,
        Firmware::Mlove => theme::ORANGE,
        Firmware::Sony => theme::GREEN,
        Firmware::Apple => theme::YELLOW,
    }
}

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(device) = app.selected_device() else {
        draw_empty(f, area);
        return;
    };

    let [row1, row2, chain] = Layout::vertical([
        Constraint::Min(9),
        Constraint::Min(8),
        Constraint::Length(4),
    ])
    .areas(area);

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)]).areas(row1);
    draw_device_card(f, left, app, device);
    draw_settings_card(f, right, device);

    let [lib, pipe] =
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).areas(row2);
    draw_library_card(f, lib, app);
    draw_pipeline_card(f, pipe, device);

    draw_chain(f, chain, app, device);
}

fn draw_device_card(f: &mut Frame, area: Rect, app: &App, device: &crate::config::Device) {
    let block = card("DEVICE");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let name = truncate(&device.name, inner.width.saturating_sub(6) as usize);
    let fw = device.firmware.label().to_string();
    let fw_color = firmware_color(&device.firmware);

    let status = if device.mounted {
        Span::styled("● mounted", Style::default().fg(theme::GREEN))
    } else {
        Span::styled("○ not mounted", Style::default().fg(theme::RED))
    };

    let last = match device.last_sync {
        Some(dt) => {
            if device.last_status == "ok" {
                format!("{} (ok)", relative_ago(dt))
            } else {
                format!("{} (error)", relative_ago(dt))
            }
        }
        None => "never".to_string(),
    };

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::raw("  "),
            Span::styled(name, Style::default().fg(theme::FG).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(" [{fw}] "),
                Style::default().fg(fw_color).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  status", Style::default().fg(theme::DIM)),
            Span::raw("         "),
            status,
        ]),
        kv("last sync", last, Style::default().fg(theme::YELLOW)),
    ];

    // mounted storage usage (total / available)
    let storage = if device.mounted {
        device.mount_point.as_ref().and_then(|p| disk_stats(p))
    } else {
        None
    };

    if let Some(stats) = storage {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("  storage", Style::default().fg(theme::DIM)),
            Span::raw("       "),
            Span::styled(
                format!(
                    "{} total · {} free",
                    fmt_bytes(stats.total),
                    fmt_bytes(stats.available)
                ),
                Style::default().fg(theme::CYAN),
            ),
        ]));
    }
    let _ = app;
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::PANEL_BG)), inner);

    // storage usage gauge underneath
    if let Some(stats) = storage {
        let used = stats.total.saturating_sub(stats.available);
        let pct = if stats.total > 0 {
            (used as f64 / stats.total as f64 * 100.0) as u16
        } else {
            0
        };
        let gauge_area = Rect {
            y: inner.y + inner.height.saturating_sub(1),
            x: inner.x,
            width: inner.width,
            height: 1,
        };
        f.render_widget(
            ratatui::widgets::Gauge::default()
                .gauge_style(Style::default().fg(theme::ACCENT).bg(theme::BG_ALT))
                .ratio((pct as f64) / 100.0)
                .label(Span::styled(
                    format!(" {pct}% used "),
                    Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
                )),
            gauge_area,
        );
    }
}

fn draw_settings_card(f: &mut Frame, area: Rect, device: &crate::config::Device) {
    let block = card("PLAYER SETTINGS");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = vec![Line::raw("")];
    lines.push(kv(
        "content type",
        device.content_type.clone(),
        Style::default().fg(theme::FG),
    ));
    lines.push(kv(
        "mount point",
        device.mount_str(),
        Style::default().fg(theme::CYAN),
    ));
    lines.push(kv(
        "music folder",
        device.music_folder.clone(),
        Style::default().fg(theme::CYAN),
    ));
    lines.push(kv(
        "fat order",
        if device.preserve_fat_order { "yes".into() } else { "no".into() },
        Style::default().fg(if device.preserve_fat_order { theme::YELLOW } else { theme::DIM }),
    ));
    if device.firmware == Firmware::Apple {
        lines.push(kv(
            "sync command",
            truncate(&device.sync_command, inner.width.saturating_sub(18) as usize),
            Style::default().fg(theme::MAGENTA),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            "  edit ~/.config/dap-sync/config.toml",
            Style::default().fg(theme::DIM),
        ),
    ]));
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::PANEL_BG)), inner);
}

fn draw_library_card(f: &mut Frame, area: Rect, app: &App) {
    let block = card("LIBRARY");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let excludes = app.config.storagebox_excludes.len();
    let lines = vec![
        Line::raw(""),
        kv(
            "music",
            app.config.local_music_dir.display().to_string(),
            Style::default().fg(theme::CYAN),
        ),
        kv(
            "hoerspiele",
            app.config.local_hoerspiele_dir.display().to_string(),
            Style::default().fg(theme::CYAN),
        ),
        Line::raw(""),
        kv(
            "storagebox",
            app.config.storagebox_source.clone(),
            Style::default().fg(theme::FG),
        ),
        kv(
            "source hsp",
            app.config.storagebox_hoerspiele_source.clone(),
            Style::default().fg(theme::FG),
        ),
        kv(
            "excludes",
            format!("{excludes} patterns"),
            Style::default().fg(theme::DIM),
        ),
        Line::raw(""),
        kv(
            "output",
            app.config.output_dir.display().to_string(),
            Style::default().fg(theme::DIM),
        ),
    ];
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::PANEL_BG)), inner);
}

fn draw_pipeline_card(f: &mut Frame, area: Rect, device: &crate::config::Device) {
    let block = card("SYNC PIPELINE");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let steps: Vec<(&str, &str)> = vec![
        ("1", "mirror StorageBox → local"),
        ("2", "NFC-aware diff"),
        ("3", "rsync copy"),
        ("4", if device.firmware == Firmware::Mlove {
            "eject + fatsort"
        } else {
            "eject"
        }),
    ];
    let mut lines: Vec<Line> = Vec::new();
    for (n, label) in steps {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!(" {n} "),
                Style::default().fg(theme::BG).bg(theme::ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(label, Style::default().fg(theme::FG)),
        ]));
        lines.push(Line::raw(""));
    }
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::PANEL_BG)), inner);
}

fn draw_chain(f: &mut Frame, area: Rect, app: &App, device: &crate::config::Device) {
    let block = card("SYNC PATH");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let w = inner.width.saturating_sub(2) as usize;
    // reserve: indent(2) + 2×arrow(20) + 3×chip brackets(9)
    let avail = w.saturating_sub(31);
    let seg1 = avail * 40 / 100;
    let seg2 = avail * 30 / 100;
    let seg3 = avail.saturating_sub(seg1 + seg2);

    let src = truncate(&app.config.storagebox_source, seg1.max(3));
    let local = truncate(&app.config.local_music_dir.display().to_string(), seg2.max(3));
    let device_name = truncate(&device.name, seg3.max(3));

    let mut line = vec![
        Span::raw("  "),
        chip(&src),
        arrow("rclone"),
        chip(&local),
        arrow("rsync"),
        chip(&device_name),
    ];
    let used: usize = line.iter().map(|s| s.width()).sum();
    if used < inner.width as usize {
        line.push(Span::raw(" ".repeat(inner.width as usize - used)));
    }
    f.render_widget(
        Paragraph::new(Line::from(line)).style(Style::default().bg(theme::PANEL_BG)),
        inner,
    );
}

fn chip(text: &str) -> Span<'static> {
    Span::styled(
        format!(" [{text}]"),
        Style::default().fg(theme::BG).bg(theme::ACCENT).add_modifier(Modifier::BOLD),
    )
}

fn arrow(label: &str) -> Span<'static> {
    Span::styled(format!(" ─{label}▶ "), Style::default().fg(theme::CYAN))
}

fn draw_empty(f: &mut Frame, area: Rect) {
    let block = card("DEVICE");
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(vec![
            Line::raw(""),
            Line::from(Span::styled(
                "  No devices configured.",
                Style::default().fg(theme::YELLOW),
            )),
            Line::from(Span::styled(
                "  Add [[devices]] entries to your config.toml",
                Style::default().fg(theme::DIM),
            )),
        ])
        .style(Style::default().bg(theme::PANEL_BG)),
        inner,
    );
}
