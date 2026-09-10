use crate::app::App;
use crate::engine::{StepState, StepStatus, SyncState};
use crate::ui::components::step_icon;
use crate::ui::theme;
use crate::ui::truncate;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, BorderType, Gauge, Paragraph};

fn panel(title: &str) -> Block<'static> {
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
        draw_idle(f, body, &state, app);
        return;
    }

    let [steps, progress] =
        Layout::vertical([Constraint::Min(6), Constraint::Length(7)]).areas(body);
    draw_steps(f, steps, &state, app);
    draw_progress(f, progress, &state);
}

fn draw_idle(f: &mut Frame, area: Rect, state: &SyncState, _app: &App) {
    let [info, hint] = Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(area);

    let block = panel("LAST SYNC");
    let inner = block.inner(info);
    f.render_widget(block, info);

    let mut lines: Vec<Line> = Vec::new();
    if let Some(ok) = state.result {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                if ok { "✓" } else { "✗" },
                Style::default().fg(if ok { theme::GREEN } else { theme::RED }).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(state.summary.clone(), Style::default().fg(theme::FG)),
        ]));
    } else {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "No sync has been run yet.",
                Style::default().fg(theme::DIM),
            ),
        ]));
    }
    if let Some(name) = &state.device {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("Press ", Style::default().fg(theme::DIM)),
            Span::styled(" s ", Style::default().fg(theme::BG).bg(theme::ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" to sync {name}"), Style::default().fg(theme::DIM)),
        ]));
    }
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::PANEL_BG)), inner);

    let hint_block = panel("PIPELINE");
    let hint_inner = hint_block.inner(hint);
    f.render_widget(hint_block, hint);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::raw("  "),
                Span::styled("1 ", Style::default().fg(theme::ACCENT)),
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
            ]),
        ])
        .style(Style::default().bg(theme::PANEL_BG)),
        hint_inner,
    );
}

fn draw_steps(f: &mut Frame, area: Rect, state: &SyncState, app: &App) {
    let block = panel("STEPS");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(""));
    for step in &state.steps {
        lines.push(step_line(step, app));
        lines.push(Line::raw(""));
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
        .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(theme::DIM)))
        .gauge_style(Style::default().fg(theme::ACCENT).bg(theme::BG_ALT))
        .ratio((pct as f64) / 100.0)
        .label(Span::styled(
            format!(" {label}  ·  {pct}% "),
            Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
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
