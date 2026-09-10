use crate::app::App;
use crate::engine::{StepState, StepStatus, SyncState};
use crate::ui::components::{firmware_color, key_hint, panel, step_icon};
use crate::ui::theme;
use crate::ui::truncate;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let state = app.engine.state.lock().unwrap().clone();

    let header = match (&state.device, state.running) {
        (Some(name), true) => {
            if state.aborting {
                format!(" Aborting sync → {name}")
            } else {
                format!(" Syncing → {name}")
            }
        }
        (Some(name), false) => format!(" Ready to sync → {name}"),
        _ => " Sync".to_string(),
    };
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            header,
            Style::default()
                .fg(theme::FG)
                .add_modifier(Modifier::BOLD),
        )))
        .style(Style::default().bg(theme::BG)),
        head,
    );

    if !state.running {
        if state.steps.is_empty() {
            draw_ready(f, body, app);
        } else {
            draw_finished(f, body, &state, app);
        }
        return;
    }

    let [steps, progress] =
        Layout::vertical([Constraint::Min(6), Constraint::Length(7)]).areas(body);
    draw_steps(f, steps, &state, app);
    draw_progress(f, progress, &state);
}

/// Horizontally centered, vertically centered block of lines.
fn center_lines(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    let pad = area.height.saturating_sub(lines.len() as u16) / 2;
    let mut content: Vec<Line> = vec![Line::raw(""); pad as usize];
    content.extend(lines);
    f.render_widget(
        Paragraph::new(content)
            .alignment(Alignment::Center)
            .style(Style::default().bg(theme::PANEL_BG)),
        area,
    );
}

/// Idle state before the first sync of this session: a hero panel for the
/// selected device plus the pipeline description.
fn draw_ready(f: &mut Frame, area: Rect, app: &App) {
    let [hero, pipeline] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(4)]).areas(area);

    let block = panel("READY TO SYNC");
    let inner = block.inner(hero);
    f.render_widget(block, hero);

    let Some(device) = app.selected_device() else {
        center_lines(
            f,
            inner,
            vec![
                Line::from(Span::styled(
                    "No device selected",
                    Style::default()
                        .fg(theme::YELLOW)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Add [[devices]] entries to config.toml, then press r",
                    Style::default().fg(theme::DIM),
                )),
            ],
        );
        draw_pipeline(f, pipeline);
        return;
    };

    let (mount, mount_style) = if device.mounted {
        ("● mounted", Style::default().fg(theme::GREEN))
    } else {
        ("○ not mounted", Style::default().fg(theme::RED))
    };
    let last = match device.last_sync {
        Some(dt) if device.last_status == "ok" => {
            format!("{} · ok", crate::app::relative_ago(dt))
        }
        Some(dt) => format!("{} · error", crate::app::relative_ago(dt)),
        None => "never synced".to_string(),
    };

    let lines = vec![
        Line::from(Span::styled(
            truncate(&device.name, inner.width.saturating_sub(4) as usize),
            Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                device.firmware.label().to_string(),
                Style::default()
                    .fg(firmware_color(&device.firmware))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ·  ", Style::default().fg(theme::DIM)),
            Span::styled(device.content_type.clone(), Style::default().fg(theme::FG)),
            Span::styled("  ·  ", Style::default().fg(theme::DIM)),
            Span::styled(mount, mount_style),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("press", Style::default().fg(theme::DIM)),
            key_hint("s"),
            Span::styled("to sync this device", Style::default().fg(theme::DIM)),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            format!("last sync {last}"),
            Style::default().fg(theme::DIM),
        )),
    ];
    center_lines(f, inner, lines);
    draw_pipeline(f, pipeline);
}

fn draw_pipeline(f: &mut Frame, area: Rect) {
    let block = panel("PIPELINE");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let line = Line::from(vec![
        Span::styled(" 1 ", Style::default().fg(theme::ACCENT)),
        Span::styled("rclone sync", Style::default().fg(theme::FG)),
        Span::styled("  →  ", Style::default().fg(theme::DIM)),
        Span::styled("2 ", Style::default().fg(theme::ACCENT)),
        Span::styled("NFC diff", Style::default().fg(theme::FG)),
        Span::styled("  →  ", Style::default().fg(theme::DIM)),
        Span::styled("3 ", Style::default().fg(theme::ACCENT)),
        Span::styled("rsync", Style::default().fg(theme::FG)),
        Span::styled("  →  ", Style::default().fg(theme::DIM)),
        Span::styled("4 ", Style::default().fg(theme::ACCENT)),
        Span::styled("eject / fatsort", Style::default().fg(theme::FG)),
    ]);
    f.render_widget(
        Paragraph::new(vec![Line::raw(""), line])
            .alignment(Alignment::Center)
            .style(Style::default().bg(theme::PANEL_BG)),
        inner,
    );
}

/// After a sync finished: keep the final step timeline visible instead of
/// discarding it and showing only an empty "last sync" card.
fn draw_finished(f: &mut Frame, area: Rect, state: &SyncState, app: &App) {
    let [result, steps] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);

    let ok = state.result.unwrap_or(false);
    let block = panel("LAST RUN");
    let inner = block.inner(result);
    f.render_widget(block, result);

    let (icon, color) = if ok {
        ("✓", theme::GREEN)
    } else {
        ("✗", theme::RED)
    };
    let device = state.device.clone().unwrap_or_default();
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(icon, Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(
                state.summary.clone(),
                Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("   {device}"), Style::default().fg(theme::DIM)),
        ]))
        .style(Style::default().bg(theme::PANEL_BG)),
        inner,
    );

    draw_steps(f, steps, state, app);
}

fn draw_steps(f: &mut Frame, area: Rect, state: &SyncState, app: &App) {
    let block = panel("STEPS");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(""));
    for (i, step) in state.steps.iter().enumerate() {
        lines.push(step_line(step, app));
        if i + 1 < state.steps.len() {
            // timeline connector between the step icons
            lines.push(Line::from(Span::styled(
                "  │",
                Style::default().fg(theme::BORDER),
            )));
        }
    }
    if !state.running {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::raw("  "),
            key_hint("s"),
            Span::styled(" run again", Style::default().fg(theme::DIM)),
        ]));
    }
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::PANEL_BG)), inner);
}

fn step_line(step: &StepState, app: &App) -> Line<'static> {
    let icon = step_icon(step.status, app);
    let title_style = match step.status {
        StepStatus::Active => Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
        StepStatus::Done => Style::default().fg(theme::GREEN),
        StepStatus::Error => Style::default().fg(theme::RED),
        StepStatus::Pending => Style::default().fg(theme::DIM),
    };
    Line::from(vec![
        Span::raw("  "),
        icon,
        Span::raw("  "),
        Span::styled(step.title.clone(), title_style),
        Span::raw("   "),
        Span::styled(step.detail.clone(), Style::default().fg(theme::DIM)),
    ])
}

fn draw_progress(f: &mut Frame, area: Rect, state: &SyncState) {
    let block = panel("PROGRESS");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let active = state
        .steps
        .iter()
        .position(|s| s.status == StepStatus::Active)
        .unwrap_or(usize::MAX);

    let (pct, done, total, speed, eta, current, label) = if active == 0 {
        (
            state.mirror.pct,
            state.mirror.done.clone(),
            state.mirror.total.clone(),
            state.mirror.speed.clone(),
            state.mirror.eta.clone(),
            state.mirror.current.clone(),
            "Mirroring local library",
        )
    } else if active == 1 {
        (
            state.device_prog.pct,
            state.device_prog.done.clone(),
            state.device_prog.total.clone(),
            state.device_prog.speed.clone(),
            state.device_prog.eta.clone(),
            state.device_prog.current.clone(),
            "Syncing device",
        )
    } else {
        (0, String::new(), String::new(), String::new(), String::new(), String::new(), "Finalizing")
    };

    let [gauge_row, stats] = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);

    let g = Gauge::default()
        .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(theme::BORDER)))
        .gauge_style(Style::default().fg(theme::ACCENT).bg(theme::BG_ALT))
        .ratio((pct as f64) / 100.0)
        .label(Span::styled(
            format!(" {label}  ·  {pct}% "),
            // No explicit colour: Gauge swaps fg/bg under the label so text
            // stays readable over both the filled and empty bar.
            Style::default().add_modifier(Modifier::BOLD),
        ));
    f.render_widget(g, gauge_row);

    // Line 1: bytes transferred on the left, speed + ETA right-aligned on the
    // same row (they used to be pushed onto a third line that the panel height
    // clipped, so they were usually invisible).
    let mut left: Vec<Span> = vec![Span::raw("  ")];
    left.push(Span::styled(
        if done.is_empty() { "…".to_string() } else { done.clone() },
        Style::default().fg(theme::CYAN),
    ));
    left.push(Span::styled(" / ", Style::default().fg(theme::DIM)));
    left.push(Span::styled(
        if total.is_empty() { "…".to_string() } else { total },
        Style::default().fg(theme::CYAN),
    ));

    let mut right: Vec<Span> = Vec::new();
    if !speed.is_empty() {
        right.push(Span::styled(speed, Style::default().fg(theme::YELLOW)));
    }
    if !eta.is_empty() {
        right.push(Span::styled("  ETA ", Style::default().fg(theme::DIM)));
        right.push(Span::styled(eta, Style::default().fg(theme::YELLOW)));
    }

    let used: usize = left.iter().map(|s| s.width()).sum::<usize>()
        + right.iter().map(|s| s.width()).sum::<usize>();
    let pad = (inner.width as usize).saturating_sub(used + 2);
    left.push(Span::raw(" ".repeat(pad)));
    left.extend(right);

    let mut lines = vec![Line::from(left)];
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            truncate(&current, inner.width.saturating_sub(2) as usize),
            Style::default().fg(theme::DIM),
        ),
    ]));
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::PANEL_BG)),
        stats,
    );
}
