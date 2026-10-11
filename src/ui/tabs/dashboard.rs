use crate::app::{relative_ago, App};
use crate::config::Firmware;
use crate::disk::{disk_stats, fmt_bytes};
use crate::ui::components::{arrow, chip, firmware_color, panel};
use crate::ui::theme;
use crate::ui::truncate;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

fn kv_w<'a>(label: &str, value: String, value_style: Style, label_w: usize) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("  {label:<label_w$}"), Style::default().fg(theme::DIM)),
        Span::styled(value, value_style),
    ])
}

/// `age` shortened for narrow cards (`42m ago` → `42m`).
fn short_ago(dt: chrono::DateTime<chrono::Local>) -> String {
    relative_ago(dt).trim_end_matches(" ago").to_string()
}

/// A `kv` line whose value is a filesystem path, marking whether it exists.
fn path_kv<'a>(
    label: &str,
    path: &std::path::Path,
    max_value: usize,
    label_w: usize,
) -> Line<'a> {
    let (mark, mark_style) = if path.exists() {
        ("✓", Style::default().fg(theme::GREEN))
    } else {
        ("✗", Style::default().fg(theme::RED))
    };
    Line::from(vec![
        Span::styled(format!("  {label:<label_w$}"), Style::default().fg(theme::DIM)),
        Span::styled(format!("{mark} "), mark_style),
        Span::styled(
            // the mark + space take two columns of the value budget
            truncate(
                &path.display().to_string(),
                max_value.saturating_sub(2).max(4),
            ),
            Style::default().fg(theme::FG),
        ),
    ])
}

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(device) = app.selected_device() else {
        draw_empty(f, area);
        return;
    };

    // Three stacked rows normally; drop the decorative SYNC PATH card when the
    // terminal is too short for all cards to keep a content row.
    let (row1, row2, chain) = if area.height >= 19 {
        let [r1, r2, ch] = Layout::vertical([
            Constraint::Min(8),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .areas(area);
        (r1, r2, Some(ch))
    } else {
        let [r1, r2] = Layout::vertical([
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .areas(area);
        (r1, r2, None)
    };

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)]).areas(row1);
    draw_device_card(f, left, app, device);
    draw_settings_card(f, right, device);

    let [lib, status] =
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).areas(row2);
    draw_library_card(f, lib, app);
    draw_sync_status_card(f, status, app);

    if let Some(chain) = chain {
        draw_chain(f, chain, app, device);
    }
}

fn draw_device_card(f: &mut Frame, area: Rect, app: &App, device: &crate::config::Device) {
    let block = panel("DEVICE");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let narrow = inner.width < 34;
    let lw = if narrow { 10 } else { 14 };
    let value_w = (inner.width as usize).saturating_sub(2 + lw).max(4);
    let line = |label: &str, value: String, style: Style| {
        kv_w(label, truncate(&value, value_w), style, lw)
    };

    let name = truncate(&device.name, inner.width.saturating_sub(6) as usize);
    let fw = device.firmware.label().to_string();
    let fw_color = firmware_color(&device.firmware);

    let (status, status_style) = if device.mounted {
        (
            if narrow { "● on" } else { "● mounted" },
            Style::default().fg(theme::GREEN),
        )
    } else {
        (
            if narrow { "○ off" } else { "○ not mounted" },
            Style::default().fg(theme::RED),
        )
    };

    let (last, last_style) = match device.last_sync {
        Some(dt) if device.last_status == "ok" => (
            format!(
                "{} · ok",
                if narrow { short_ago(dt) } else { relative_ago(dt) }
            ),
            Style::default().fg(theme::GREEN),
        ),
        Some(dt) => (
            format!(
                "{} · error",
                if narrow { short_ago(dt) } else { relative_ago(dt) }
            ),
            Style::default().fg(theme::RED),
        ),
        None => ("never".to_string(), Style::default().fg(theme::DIM)),
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
        line("status", status.to_string(), status_style),
        line("last sync", last, last_style),
    ];

    // Mounted storage usage. udisks may mount the volume at a suffixed path
    // (IPOD → IPOD1), so report stats for wherever it actually is mounted.
    let actual_mount = device
        .mount_point
        .as_deref()
        .and_then(crate::disk::mounted_path_for);
    let storage = if device.mounted {
        actual_mount.as_deref().and_then(disk_stats)
    } else {
        None
    };

    let mut pct = 0u16;
    if let Some(stats) = storage {
        let used = stats.total.saturating_sub(stats.available);
        pct = if stats.total > 0 {
            (used as f64 / stats.total as f64 * 100.0).round() as u16
        } else {
            0
        };
        let value = if narrow {
            format!("{pct}% used")
        } else {
            format!("{} free · {}%", fmt_bytes(stats.available), pct)
        };
        lines.push(line("storage", value, Style::default().fg(theme::CYAN)));
    }

    let _ = app;
    let content_h = lines.len() as u16;
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::PANEL_BG)), inner);

    // Usage bar on its own row, only when the card is tall enough that it
    // cannot collide with the text above.
    if let Some(stats) = storage {
        if inner.height >= content_h + 2 {
            let used = stats.total.saturating_sub(stats.available);
            let ratio = if stats.total > 0 {
                used as f64 / stats.total as f64
            } else {
                0.0
            };
            let bar_area = Rect {
                y: inner.y + inner.height.saturating_sub(1),
                x: inner.x + 1,
                width: inner.width.saturating_sub(2),
                height: 1,
            };
            f.render_widget(
                ratatui::widgets::Gauge::default()
                    .gauge_style(Style::default().fg(theme::ACCENT).bg(theme::BG_ALT))
                    .ratio(ratio.clamp(0.0, 1.0))
                    .label(Span::styled(
                        format!(" {pct}% used "),
                        // No explicit colour: Gauge swaps fg/bg under the label so
                        // text stays readable over both the filled and empty bar.
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                bar_area,
            );
        }
    }
}

fn draw_settings_card(f: &mut Frame, area: Rect, device: &crate::config::Device) {
    let block = panel("PLAYER SETTINGS");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let narrow = inner.width < 34;
    let lw = if narrow { 8 } else { 14 };
    let value_w = (inner.width as usize).saturating_sub(2 + lw).max(4);
    let line = |label: &str, value: String, style: Style| {
        kv_w(label, truncate(&value, value_w), style, lw)
    };
    let (l_content, l_mount, l_folder, l_fat) = if narrow {
        ("content", "mount", "folder", "fat")
    } else {
        ("content type", "mount point", "music folder", "fat order")
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(line(
        l_content,
        device.content_type.clone(),
        Style::default().fg(theme::FG),
    ));
    match device.mount_point.as_deref() {
        Some(p) => {
            // Show where it is actually mounted when udisks used a suffixed
            // path (e.g. the configured IPOD dir existed, so it chose IPOD1).
            let shown = crate::disk::mounted_path_for(p).unwrap_or_else(|| p.to_path_buf());
            lines.push(path_kv(l_mount, &shown, value_w, lw));
        }
        None => lines.push(line(l_mount, "—".to_string(), Style::default().fg(theme::DIM))),
    }
    lines.push(line(
        l_folder,
        device.music_folder.clone(),
        Style::default().fg(theme::CYAN),
    ));
    lines.push(line(
        l_fat,
        if device.preserve_fat_order { "yes".into() } else { "no".into() },
        Style::default().fg(if device.preserve_fat_order { theme::YELLOW } else { theme::DIM }),
    ));
    if device.firmware == Firmware::Apple {
        lines.push(line(
            if narrow { "cmd" } else { "sync command" },
            device.sync_command.clone(),
            Style::default().fg(theme::MAGENTA),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "r",
            Style::default()
                .fg(theme::BG)
                .bg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" reloads config.toml", Style::default().fg(theme::DIM)),
    ]));
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::PANEL_BG)), inner);
}

fn draw_library_card(f: &mut Frame, area: Rect, app: &App) {
    let block = panel("LIBRARY");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let excludes = app.config.storagebox_excludes.len();
    let narrow = inner.width < 34;
    let lw = if narrow { 12 } else { 14 };
    let value_w = (inner.width as usize).saturating_sub(2 + lw).max(4);
    let mut lines = vec![
        path_kv("music", &app.config.local_music_dir, value_w, lw),
        path_kv("hoerspiele", &app.config.local_hoerspiele_dir, value_w, lw),
        Line::raw(""),
    ];
    let source_kv = |label: &str, source: &str, lines: &mut Vec<Line>| {
        let path = std::path::Path::new(source);
        if path.is_absolute() {
            lines.push(path_kv(label, path, value_w, lw));
        } else {
            lines.push(kv_w(
                label,
                truncate(source, value_w),
                Style::default().fg(theme::FG),
                lw,
            ));
        }
    };
    source_kv("storagebox", &app.config.storagebox_source, &mut lines);
    source_kv(
        "source hsp",
        &app.config.storagebox_hoerspiele_source,
        &mut lines,
    );
    lines.push(kv_w(
        "excludes",
        format!("{excludes} patterns"),
        Style::default().fg(theme::DIM),
        lw,
    ));
    lines.push(Line::raw(""));
    lines.push(kv_w(
        "output",
        truncate(&app.config.output_dir.display().to_string(), value_w),
        Style::default().fg(theme::DIM),
        lw,
    ));
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::PANEL_BG)), inner);
}

fn draw_sync_status_card(f: &mut Frame, area: Rect, app: &App) {
    let block = panel("SYNC STATUS");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let syncing = app.engine.state.lock().map(|s| s.running).unwrap_or(false);
    let sync_device = if syncing {
        app.engine.state.lock().unwrap().device.clone()
    } else {
        None
    };
    let now = chrono::Local::now();

    let mut ok = 0usize;
    let mut err = 0usize;
    let mut never = 0usize;
    // Narrow cards get time-only statuses so the device name keeps its space.
    let compact = inner.width < 34;

    let mut lines: Vec<Line> = Vec::new();
    for device in &app.config.devices {
        let is_syncing = sync_device.as_ref().map(|d| d == &device.name).unwrap_or(false);
        let status_span: Span = if is_syncing {
            Span::styled(
                if compact {
                    format!("{} sync", app.spinner())
                } else {
                    format!("{} syncing…", app.spinner())
                },
                Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD),
            )
        } else if let Some(dt) = device.last_sync {
            let ago = relative_ago(dt);
            if device.last_status == "ok" {
                ok += 1;
                Span::styled(
                    if compact { format!("✓ {ago}") } else { format!("✓ ok · {ago}") },
                    Style::default().fg(theme::GREEN),
                )
            } else {
                err += 1;
                Span::styled(
                    if compact { format!("✗ {ago}") } else { format!("✗ error · {ago}") },
                    Style::default().fg(theme::RED).add_modifier(Modifier::BOLD),
                )
            }
        } else {
            never += 1;
            Span::styled("— never", Style::default().fg(theme::DIM))
        };

        let name_w = (inner.width as usize)
            .saturating_sub(5 + status_span.width())
            .max(4);
        let name = truncate(&device.name, name_w);
        let mut spans: Vec<Span> = vec![Span::raw("  "), Span::styled(name, Style::default().fg(theme::FG))];
        let used: usize = spans.iter().map(|s| s.width()).sum();
        let left = (inner.width as usize).saturating_sub(2 + status_span.width());
        if used < left {
            spans.push(Span::raw(" ".repeat(left - used)));
        }
        spans.push(status_span);
        lines.push(Line::from(spans));
    }

    // summary footer (two compact lines — the one-line variant used to end in
    // an ugly "…" because the card is narrow)
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        format!("  {ok} ok · {err} err · {never} never"),
        Style::default().fg(theme::DIM),
    )));
    let last = app
        .config
        .devices
        .iter()
        .filter_map(|d| d.last_sync)
        .min_by_key(|dt| now.signed_duration_since(*dt).num_seconds().max(0))
        .map(relative_ago)
        .unwrap_or_else(|| "—".to_string());
    lines.push(Line::from(Span::styled(
        format!("  last sync {last}"),
        Style::default().fg(theme::DIM),
    )));

    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::PANEL_BG)), inner);
}

fn draw_chain(f: &mut Frame, area: Rect, app: &App, device: &crate::config::Device) {
    let block = panel("SYNC PATH");
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

    // Mirror transport: an existing local absolute directory is copied with
    // rsync; anything else (e.g. an rclone remote like `storagebox:...`) goes
    // through rclone. Keep this in sync with `engine::run_mirror`.
    let mirror_tool = if crate::engine::mirror_uses_rsync(&app.config.storagebox_source) {
        "rsync"
    } else {
        "rclone"
    };

    let mut line = vec![
        Span::raw("  "),
        chip(&src),
        arrow(mirror_tool),
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

fn draw_empty(f: &mut Frame, area: Rect) {
    let block = panel("DEVICE");
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
