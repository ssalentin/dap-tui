use crate::app::{relative_ago, App};
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
        Span::styled(
            format!("  {label:<12}"),
            Style::default().fg(theme::DIM),
        ),
        Span::styled(value, value_style),
    ])
}

fn firmware_badge(fw: &str) -> Span<'static> {
    let color = match fw {
        "rockbox" => theme::MAGENTA,
        "android" => theme::CYAN,
        "mlove" => theme::ORANGE,
        _ => theme::GREEN,
    };
    Span::styled(
        format!(" [{fw}] "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(device) = app.selected_device() else {
        draw_empty(f, area);
        return;
    };

    let [top, chain] = Layout::vertical([Constraint::Min(8), Constraint::Length(4)]).areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(top);

    // ── device card ──
    let dev_block = card("DEVICE");
    let inner = dev_block.inner(left);
    f.render_widget(dev_block, left);

    let status = if device.mounted {
        Span::styled("● mounted", Style::default().fg(theme::GREEN))
    } else {
        Span::styled("○ not mounted", Style::default().fg(theme::RED))
    };
    let fw = device.firmware.label().to_string();
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

    let lines = vec![
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                truncate(&device.name, inner.width.saturating_sub(4) as usize),
                Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
            ),
            firmware_badge(&fw),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  status", Style::default().fg(theme::DIM)),
            Span::raw("       "),
            status.clone(),
        ]),
        kv("firmware", fw.clone(), Style::default().fg(theme::FG)),
        kv("content", device.content_type.clone(), Style::default().fg(theme::FG)),
        kv("mount", device.mount_str(), Style::default().fg(theme::CYAN)),
        kv("folder", device.music_folder.clone(), Style::default().fg(theme::CYAN)),
        kv("last sync", last, Style::default().fg(theme::YELLOW)),
    ];
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::PANEL_BG)), inner);

    // ── library card ──
    let lib_block = card("LIBRARY");
    let inner = lib_block.inner(right);
    f.render_widget(lib_block, right);

    let excludes = app.config.storagebox_excludes.len();
    let lib_lines = vec![
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
    f.render_widget(Paragraph::new(lib_lines).style(Style::default().bg(theme::PANEL_BG)), inner);

    // ── source chain ──
    draw_chain(f, chain, app, device.name.clone());
}

fn draw_chain(f: &mut Frame, area: Rect, app: &App, device_name: String) {
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
    let device = truncate(&device_name, seg3.max(3));

    let mut line = vec![
        Span::raw("  "),
        chip(&src),
        arrow("rclone"),
        chip(&local),
        arrow("rsync"),
        chip(&device),
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
    Span::styled(
        format!(" ─{label}▶ "),
        Style::default().fg(theme::CYAN),
    )
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
