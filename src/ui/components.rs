use crate::app::{relative_ago, App, Tab};
use crate::config::Firmware;
use crate::engine::{LogLevel, StepStatus};
use crate::ui::theme;
use crate::ui::truncate;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

// ── shared panel chrome ───────────────────────────────────────────────────────

/// Rounded card with the accent title used by every tab, so all panels share
/// exactly one look.
pub fn panel(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().bg(theme::PANEL_BG))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ))
}

/// Colour per firmware family, used for the firmware tag/chip.
pub fn firmware_color(fw: &Firmware) -> ratatui::style::Color {
    match fw {
        Firmware::Rockbox => theme::MAGENTA,
        Firmware::Android => theme::CYAN,
        Firmware::Mlove => theme::ORANGE,
        Firmware::Sony => theme::GREEN,
        Firmware::Apple => theme::YELLOW,
    }
}

/// Highlighted inline chip like `[storagebox:backup/masters]`.
pub fn chip(text: &str) -> Span<'static> {
    Span::styled(
        format!(" [{text}]"),
        Style::default()
            .fg(theme::BG)
            .bg(theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    )
}

/// Arrow between two chips: ` ─rsync▶ `.
pub fn arrow(label: &str) -> Span<'static> {
    Span::styled(format!(" ─{label}▶ "), Style::default().fg(theme::CYAN))
}

/// A small key hint, e.g. ` s `, for inline "press …" prompts.
pub fn key_hint(key: &str) -> Span<'static> {
    Span::styled(
        format!(" {key} "),
        Style::default()
            .fg(theme::BG)
            .bg(theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    )
}

// ── title bar ─────────────────────────────────────────────────────────────────

pub fn draw_title(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![
        Span::styled(
            " dap-tui ",
            Style::default()
                .fg(theme::BG)
                .bg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            "StorageBox",
            Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  →  ", Style::default().fg(theme::DIM)),
        Span::styled("local", Style::default().fg(theme::FG).add_modifier(Modifier::BOLD)),
        Span::styled("  →  ", Style::default().fg(theme::DIM)),
        Span::styled("player", Style::default().fg(theme::FG).add_modifier(Modifier::BOLD)),
    ];

    let mut right_spans = vec![
        Span::styled(
            if app.skip_mirror { "mirror off" } else { "mirror on" },
            Style::default().fg(if app.skip_mirror {
                theme::YELLOW
            } else {
                theme::DIM
            }),
        ),
        Span::raw("   "),
        Span::styled(
            if app.is_syncing() {
                format!("{} syncing", app.spinner())
            } else {
                "● ready".to_string()
            },
            Style::default().fg(if app.is_syncing() {
                theme::CYAN
            } else {
                theme::GREEN
            }),
        ),
    ];

    // pad to full width
    let left_w: usize = spans.iter().map(|s| s.width()).sum();
    let right_w: usize = right_spans.iter().map(|s| s.width()).sum();
    let total = area.width as usize;
    let pad = total.saturating_sub(left_w + right_w);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.append(&mut right_spans);

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::BG_ALT)),
        area,
    );
}

// ── gradient rule ─────────────────────────────────────────────────────────────

pub fn draw_rule(f: &mut Frame, area: Rect, _app: &App) {
    let mut spans = Vec::with_capacity(area.width as usize);
    let colors = [theme::ACCENT, theme::CYAN, theme::MAGENTA, theme::DIM];
    for i in 0..area.width {
        let t = i as f32 / area.width.max(1) as f32;
        let idx = if t < 0.25 {
            0
        } else if t < 0.5 {
            1
        } else if t < 0.75 {
            2
        } else {
            3
        };
        spans.push(Span::styled("─", Style::default().fg(colors[idx])));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::BG)),
        area,
    );
}

// ── sidebar ───────────────────────────────────────────────────────────────────

pub fn draw_sidebar(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().bg(theme::BG_ALT))
        .title(Span::styled(
            "  DEVICES  ",
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [head, list, foot] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
            .areas(inner);

    let mounted = app.config.devices.iter().filter(|d| d.mounted).count();
    let syncing = app.engine.state.lock().map(|s| s.running).unwrap_or(false);

    let head_line = Line::from(vec![
        Span::styled(
            format!("{} devices", app.config.devices.len()),
            Style::default().fg(theme::FG),
        ),
        Span::raw(" · "),
        Span::styled(
            format!("{mounted} mounted"),
            Style::default().fg(if mounted > 0 { theme::GREEN } else { theme::DIM }),
        ),
    ]);
    f.render_widget(
        Paragraph::new(head_line).style(Style::default().bg(theme::BG_ALT)),
        head,
    );

    draw_device_list(f, list, app, syncing);

    let foot_line = Line::from(vec![
        Span::styled("s sync", Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(" · ", Style::default().fg(theme::DIM)),
        Span::styled("r refresh", Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD)),
        if app.skip_mirror {
            Span::styled(" · mirror off", Style::default().fg(theme::YELLOW))
        } else {
            Span::styled("", Style::default())
        },
    ]);
    f.render_widget(
        Paragraph::new(foot_line).style(Style::default().bg(theme::BG_ALT)),
        foot,
    );
}

fn draw_device_list(f: &mut Frame, area: Rect, app: &App, syncing: bool) {
    if app.config.devices.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  no devices configured",
                Style::default().fg(theme::DIM),
            )))
            .style(Style::default().bg(theme::BG_ALT)),
            area,
        );
        return;
    }

    let item_h: u16 = 2;
    let visible = (area.height / item_h) as usize;
    let start = app.sidebar_scroll.min(app.config.devices.len());

    let rows = Layout::vertical(
        (0..visible)
            .map(|_| Constraint::Length(item_h))
            .collect::<Vec<_>>(),
    )
    .split(area);

    let sync_device = if syncing {
        app.engine.state.lock().unwrap().device.clone()
    } else {
        None
    };

    for (i, row) in rows.iter().enumerate() {
        let idx = start + i;
        let Some(device) = app.config.devices.get(idx) else {
            continue;
        };
        let selected = idx == app.selected;

        // Soft selection: highlight background + accent marker instead of
        // flooding both rows with the full accent colour.
        let row_style = if selected {
            Style::default().bg(theme::SELECTION_BG).fg(theme::FG)
        } else {
            Style::default().bg(theme::BG_ALT).fg(theme::FG)
        };
        let mut spans: Vec<Span> = Vec::new();

        // status icon
        let is_this = sync_device.as_ref().map(|d| d == &device.name).unwrap_or(false);
        if is_this {
            spans.push(Span::styled(
                app.spinner().to_string(),
                Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD),
            ));
        } else if device.mounted {
            spans.push(Span::styled("●", Style::default().fg(theme::GREEN)));
        } else {
            spans.push(Span::styled("○", Style::default().fg(theme::DIM)));
        }
        spans.push(Span::raw("  "));

        // selection marker + name
        let marker = if selected { "▸ " } else { "  " };
        let name = truncate(&device.name, (row.width as usize).saturating_sub(8));
        spans.push(Span::styled(
            marker,
            Style::default()
                .fg(theme::ACCENT)
                .bg(if selected { theme::SELECTION_BG } else { theme::BG_ALT })
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            name,
            Style::default()
                .fg(theme::FG)
                .add_modifier(if selected { Modifier::BOLD } else { Modifier::default() }),
        ));

        f.render_widget(Paragraph::new(Line::from(spans)).style(row_style), *row);

        // second line
        let sub = if let Some(last) = device.last_sync {
            format!(
                "  {} · {} · {}",
                device.firmware.label(),
                device.content_type,
                relative_ago(last)
            )
        } else {
            format!("  {} · {}", device.firmware.label(), device.content_type)
        };
        let sub_style = Style::default()
            .bg(if selected { theme::SELECTION_BG } else { theme::BG_ALT })
            .fg(theme::DIM);
        let row2 = Rect {
            y: row.y + 1,
            ..*row
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate(&sub, row.width.max(1) as usize),
                sub_style,
            )))
            .style(sub_style),
            row2,
        );
    }
}

// ── tab bar ───────────────────────────────────────────────────────────────────

pub fn draw_tabbar(f: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span> = Vec::new();
    for (i, tab) in Tab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let active = app.tab == *tab;
        let syncing_badge = *tab == Tab::Sync && app.is_syncing();
        let text = format!(" {} {} ", tab.number(), tab.label());
        if active {
            spans.push(Span::styled(
                text,
                Style::default()
                    .bg(theme::ACCENT)
                    .fg(theme::BG)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                text,
                Style::default().bg(theme::BG_ALT).fg(theme::DIM),
            ));
        }
        if syncing_badge {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                app.spinner().to_string(),
                Style::default().fg(theme::CYAN),
            ));
        }
    }
    let rest: u16 = area
        .width
        .saturating_sub(spans.iter().map(|s| s.width() as u16).sum());
    if rest > 0 {
        spans.push(Span::raw(" ".repeat(rest as usize)));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::BG)),
        area,
    );
}

// ── status bar ────────────────────────────────────────────────────────────────

pub fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let (msg, style) = status_text(app);
    let left_spans: Vec<Span> = vec![
        Span::styled(" ", Style::default()),
        Span::styled(msg, style),
    ];

    let mounted = app.config.devices.iter().filter(|d| d.mounted).count();
    let now = chrono::Local::now().format("%H:%M:%S").to_string();
    let right_spans = vec![
        Span::styled(
            format!("{} devices · {mounted} mounted", app.config.devices.len()),
            Style::default().fg(theme::DIM),
        ),
        Span::raw("  "),
        Span::styled(
            format!("v{} · {now}", crate::app::VERSION),
            Style::default().fg(theme::DIM),
        ),
    ];

    let total = area.width as usize;
    let lw: usize = left_spans.iter().map(|s| s.width()).sum();
    let rw: usize = right_spans.iter().map(|s| s.width()).sum();
    let pad = total.saturating_sub(lw + rw);
    let mut all = left_spans;
    all.push(Span::raw(" ".repeat(pad)));
    all.extend(right_spans);

    f.render_widget(
        Paragraph::new(Line::from(all)).style(Style::default().bg(theme::BG_ALT)),
        area,
    );
}

fn status_text(app: &App) -> (String, Style) {
    if let Some((msg, level)) = &app.status_msg {
        let style = match level {
            LogLevel::Error => Style::default().fg(theme::RED).add_modifier(Modifier::BOLD),
            LogLevel::Warn => Style::default().fg(theme::YELLOW),
            _ => Style::default().fg(theme::GREEN),
        };
        return (format!(" {msg}"), style);
    }
    if app.is_syncing() {
        let dev = app
            .engine
            .state
            .lock()
            .unwrap()
            .device
            .clone()
            .unwrap_or_default();
        return (
            format!(" {} Syncing {dev}…", app.spinner()),
            Style::default().fg(theme::CYAN),
        );
    }
    (" Ready".to_string(), Style::default().fg(theme::DIM))
}

// ── key bar ───────────────────────────────────────────────────────────────────

pub fn draw_keys(f: &mut Frame, area: Rect, _app: &App) {
    let keys: &[(&str, &str)] = &[
        ("↑↓/jk", "nav"),
        ("s", "sync"),
        ("a", "abort"),
        ("m", "mount"),
        ("Tab/1-3", "tab"),
        ("r", "reload"),
        ("f", "follow"),
        ("?", "help"),
        ("q", "quit"),
    ];
    let mut spans: Vec<Span> = Vec::new();
    let mut first = true;
    for (key, label) in keys {
        if !first {
            spans.push(Span::styled("  ", Style::default().fg(theme::DIM)));
        }
        first = false;
        spans.push(Span::styled(
            format!(" {key} "),
            Style::default().fg(theme::ACCENT).bg(theme::BG_ALT).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(*label, Style::default().fg(theme::DIM)));
    }
    let rest = area.width.saturating_sub(spans.iter().map(|s| s.width() as u16).sum());
    if rest > 0 {
        spans.push(Span::raw(" ".repeat(rest as usize)));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans))
            .style(Style::default().bg(theme::BG))
            .alignment(Alignment::Left),
        area,
    );
}

// ── shared helpers ────────────────────────────────────────────────────────────

pub fn step_icon(status: StepStatus, app: &App) -> Span<'static> {
    match status {
        StepStatus::Done => Span::styled("●", Style::default().fg(theme::GREEN)),
        StepStatus::Active => Span::styled(
            app.spinner().to_string(),
            Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD),
        ),
        StepStatus::Pending => Span::styled("○", Style::default().fg(theme::DIM)),
        StepStatus::Error => Span::styled("✗", Style::default().fg(theme::RED).add_modifier(Modifier::BOLD)),
    }
}
