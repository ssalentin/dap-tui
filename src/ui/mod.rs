pub mod components;
pub mod tabs;
pub mod theme;

use crate::app::App;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::widgets::{Block, Borders, Clear};

use theme::{BG, DIM};

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
        Constraint::Length(2), // title
        Constraint::Length(1), // gradient rule
        Constraint::Min(0),    // body
        Constraint::Length(1), // status
        Constraint::Length(1), // keys
    ]);
    let [title, rule, body, status, keys] = v.areas(area);

    components::draw_title(f, title, app);
    components::draw_rule(f, rule, app);

    let [side, main] = Layout::horizontal([Constraint::Length(34), Constraint::Min(0)]).areas(body);
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
    let area = f.area();
    let box_w = 58;
    let box_h = 16;
    let x = (area.width.saturating_sub(box_w)) / 2;
    let y = (area.height.saturating_sub(box_h)) / 2;
    let rect = Rect::new(
        x.max(0),
        y.max(0),
        box_w.min(area.width),
        box_h.min(area.height),
    );
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(crate::ui::theme::ACCENT))
        .style(Style::default().bg(crate::ui::theme::PANEL_BG))
        .title("  KEYBINDINGS  ".cyan().bold());
    let lines = [
        "  ↑↓ / j k     move selection",
        "  s / Enter     sync selected device",
        "  a             abort running sync",
        "  Tab / 1-3     switch tab",
        "  r             refresh devices",
        "  f             toggle log follow",
        "  u / d         scroll logs",
        "  x             clear logs",
        "  ? / h         this help",
        "  q / Esc       quit",
    ];
    let mut spans = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            spans.push(ratatui::text::Line::raw(""));
        }
        let styled = if line.contains('/') {
            ratatui::text::Span::styled(*line, ratatui::style::Style::default().fg(crate::ui::theme::FG))
        } else {
            ratatui::text::Span::styled(*line, ratatui::style::Style::default().fg(DIM))
        };
        spans.push(ratatui::text::Line::from(styled));
    }
    f.render_widget(ratatui::widgets::Paragraph::new(ratatui::text::Text::from(spans)).block(block), rect);
}
