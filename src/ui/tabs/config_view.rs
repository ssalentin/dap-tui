use crate::app::App;
use crate::ui::theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, BorderType, Paragraph};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);

    let path = app.config.config_file.display().to_string();
    let head_line = Line::from(vec![
        Span::styled("  config", Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(path.clone(), Style::default().fg(theme::DIM)),
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
        .config
        .raw_text
        .lines()
        .map(syntax_line)
        .collect();

    let content_h = inner.height.saturating_sub(1) as usize;
    let offset = lines.len().saturating_sub(content_h);

    f.render_widget(
        Paragraph::new(lines)
            .scroll((offset as u16, 0))
            .style(Style::default().bg(theme::PANEL_BG)),
        inner,
    );
}

fn syntax_line(raw: &str) -> Line<'static> {
    let line = raw;
    let trimmed = line.trim_start();

    if trimmed.is_empty() {
        return Line::raw("");
    }
    if trimmed.starts_with('#') {
        return Line::from(Span::styled(line.to_string(), Style::default().fg(theme::DIM)));
    }
    if trimmed.starts_with('[') {
        return Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(theme::MAGENTA).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(idx) = line.find('=') {
        let key = line[..idx].trim_end();
        let value = line[idx + 1..].trim_start();
        let value_style = if value.starts_with('"') {
            Style::default().fg(theme::GREEN)
        } else if value.starts_with('[') {
            Style::default().fg(theme::CYAN)
        } else if value.starts_with("true") || value.starts_with("false") {
            Style::default().fg(theme::YELLOW)
        } else {
            Style::default().fg(theme::ORANGE)
        };
        return Line::from(vec![
            Span::styled(format!("{key}"), Style::default().fg(theme::ACCENT)),
            Span::styled(" = ", Style::default().fg(theme::DIM)),
            Span::styled(value.to_string(), value_style),
        ]);
    }
    Line::from(Span::styled(line.to_string(), Style::default().fg(theme::FG)))
}
