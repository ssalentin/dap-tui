use crate::app::App;
use crate::engine::LogLevel;
use crate::ui::theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, BorderType, Paragraph};

fn level_style(level: LogLevel) -> Style {
    match level {
        LogLevel::Info => Style::default().fg(theme::FG),
        LogLevel::Transfer => Style::default().fg(theme::CYAN),
        LogLevel::Warn => Style::default().fg(theme::YELLOW),
        LogLevel::Error => Style::default().fg(theme::RED).add_modifier(Modifier::BOLD),
        LogLevel::Success => Style::default().fg(theme::GREEN),
    }
}

fn level_tag(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Info => "INF",
        LogLevel::Transfer => "XFER",
        LogLevel::Warn => "WRN",
        LogLevel::Error => "ERR",
        LogLevel::Success => "OK",
    }
}

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);

    let follow_tag = if app.log_follow {
        "follow on".to_string()
    } else {
        "follow off".to_string()
    };
    let head_line = Line::from(vec![
        Span::styled(
            format!("  {} log entries", app.logs.len()),
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  ·  "),
        Span::styled(follow_tag, Style::default().fg(theme::DIM)),
        Span::raw("  ·  "),
        Span::styled(
            "f toggle · u/d scroll · x clear",
            Style::default().fg(theme::DIM),
        ),
    ]);
    f.render_widget(
        Paragraph::new(head_line).style(Style::default().bg(theme::BG)),
        head,
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::DIM))
        .style(Style::default().bg(theme::PANEL_BG));
    let inner = block.inner(body);
    f.render_widget(block, body);

    let lines: Vec<Line> = app
        .logs
        .iter()
        .map(|e| {
            let ts = e.ts.format("%H:%M:%S").to_string();
            Line::from(vec![
                Span::styled(format!(" {ts} "), Style::default().fg(theme::DIM)),
                Span::styled(format!("{:>4}", level_tag(e.level)), level_style(e.level)),
                Span::raw("  "),
                Span::styled(e.msg.clone(), level_style(e.level)),
            ])
        })
        .collect();

    let content_h = inner.height.saturating_sub(1) as usize;
    app.log_view.set(content_h);
    let offset = if app.log_follow {
        lines.len().saturating_sub(content_h)
    } else {
        app.log_scroll.min(lines.len().saturating_sub(content_h))
    };

    f.render_widget(
        Paragraph::new(lines)
            .scroll((offset as u16, 0))
            .style(Style::default().bg(theme::PANEL_BG)),
        inner,
    );
}
