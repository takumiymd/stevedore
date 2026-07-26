//! View: pure rendering of the model. No state mutation beyond the
//! ListState offset that ratatui needs for scrolling.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::{App, ContainerInfo, View};

pub const DOCKER_BLUE: Color = Color::Rgb(36, 150, 237);
pub const RUNNING_GREEN: Color = Color::Rgb(46, 204, 113);
pub const EXITED_RED: Color = Color::Rgb(231, 76, 60);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, app, header);

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(body);
    draw_container_list(frame, app, left);
    match app.view {
        View::Details => draw_details(frame, app, right),
        View::Logs => draw_logs(frame, app, right),
        View::Update => draw_update(frame, app, right),
    }

    draw_footer(frame, app, footer);
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let base = Style::default().bg(DOCKER_BLUE).fg(Color::White);

    // Fill the whole bar with the Docker blue background first.
    frame.render_widget(Paragraph::new("").style(base), area);

    let title = Paragraph::new(" Stevedore - Container Management")
        .style(base.add_modifier(Modifier::BOLD));

    match &app.status {
        Some(status) => {
            let status_width = (status.text.chars().count() as u16).saturating_add(2);
            let [title_area, status_area] =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(status_width)])
                    .areas(area);
            frame.render_widget(title, title_area);

            let status_style = if status.is_error {
                base.fg(Color::Rgb(255, 200, 120)).add_modifier(Modifier::BOLD)
            } else {
                base
            };
            let status_bar = Paragraph::new(format!("{} ", status.text))
                .style(status_style)
                .alignment(Alignment::Right);
            frame.render_widget(status_bar, status_area);
        }
        None => frame.render_widget(title, area),
    }
}

fn draw_container_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = pane_block(format!(" Containers ({}) ", app.containers.len()));

    if app.containers.is_empty() {
        let empty = Paragraph::new("No containers found")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = app.containers.iter().map(container_item).collect();
    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(DOCKER_BLUE)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn container_item(container: &ContainerInfo) -> ListItem<'_> {
    let (icon, color) = if container.is_running() {
        ("●", RUNNING_GREEN)
    } else {
        ("■", EXITED_RED)
    };
    ListItem::new(Line::from(vec![
        Span::styled(format!(" {icon} "), Style::default().fg(color)),
        Span::raw(container.name.as_str()),
    ]))
}

fn draw_details(frame: &mut Frame, app: &App, area: Rect) {
    let block = pane_block(" Details ");

    let Some(container) = app.selected() else {
        let placeholder = Paragraph::new("Select a container to see its details")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(placeholder, area);
        return;
    };

    let state_color = if container.is_running() { RUNNING_GREEN } else { EXITED_RED };
    let short_id: String = container.id.chars().take(12).collect();

    let mut lines = vec![
        kv_line("Name", Span::raw(container.name.as_str())),
        kv_line("ID", Span::raw(short_id)),
        kv_line("Image", Span::raw(container.image.as_str())),
        kv_line(
            "State",
            Span::styled(
                container.state.as_str(),
                Style::default().fg(state_color).add_modifier(Modifier::BOLD),
            ),
        ),
        kv_line("Status", Span::raw(container.status.as_str())),
        kv_line(
            "Ports",
            if container.ports.is_empty() {
                Span::styled("none", Style::default().fg(Color::DarkGray))
            } else {
                Span::raw(container.ports.as_str())
            },
        ),
    ];

    let urls = container.local_urls();
    if !urls.is_empty() {
        for (i, url) in urls.iter().enumerate() {
            let label = if i == 0 { "Local URL" } else { "" };
            lines.push(kv_line(
                label,
                Span::styled(url, Style::default().fg(DOCKER_BLUE).add_modifier(Modifier::UNDERLINED)),
            ));
        }
    }

    lines.push(kv_line(
        "Compose",
        match &container.compose {
            Some(compose) => Span::raw(format!("{}/{}", compose.project, compose.service)),
            None => Span::styled("standalone", Style::default().fg(Color::DarkGray)),
        },
    ));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " Live Stats",
        Style::default().fg(DOCKER_BLUE).add_modifier(Modifier::BOLD),
    )));

    if !container.is_running() {
        lines.push(kv_line(
            "",
            Span::styled("container is not running", Style::default().fg(Color::DarkGray)),
        ));
    } else {
        match &app.stats {
            Some(stats) => {
                let mem_percent = if stats.mem_limit > 0 {
                    stats.mem_usage as f64 / stats.mem_limit as f64 * 100.0
                } else {
                    0.0
                };
                lines.push(kv_line("CPU", Span::raw(format!("{:.2}%", stats.cpu_percent))));
                lines.push(kv_line(
                    "Memory",
                    Span::raw(format!(
                        "{} / {} ({:.1}%)",
                        format_bytes(stats.mem_usage),
                        format_bytes(stats.mem_limit),
                        mem_percent
                    )),
                ));
                lines.push(kv_line(
                    "Network",
                    Span::raw(format!(
                        "rx {} / tx {}",
                        format_bytes(stats.net_rx),
                        format_bytes(stats.net_tx)
                    )),
                ));
            }
            None => lines.push(kv_line(
                "",
                Span::styled("collecting stats...", Style::default().fg(Color::DarkGray)),
            )),
        }
    }

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_logs(frame: &mut Frame, app: &mut App, area: Rect) {
    let name = app.selected().map(|c| c.name.clone()).unwrap_or_else(|| "?".to_string());

    if app.logs.is_empty() {
        let waiting = Paragraph::new("Waiting for log output...")
            .style(Style::default().fg(Color::DarkGray))
            .block(pane_block(format!(" Logs: {name} ")));
        frame.render_widget(waiting, area);
        return;
    }

    let (start, end) = scroll_window(&mut app.scroll_back, app.logs.len(), area);
    let title = if app.scroll_back > 0 {
        format!(" Logs: {name} [scrolled back {} lines, End to follow] ", app.scroll_back)
    } else {
        format!(" Logs: {name} ")
    };
    let lines: Vec<Line> = app.logs[start..end].iter().map(|l| Line::raw(l.as_str())).collect();
    frame.render_widget(Paragraph::new(lines).block(pane_block(title)), area);
}

fn draw_update(frame: &mut Frame, app: &mut App, area: Rect) {
    let target = app.update_target.clone().unwrap_or_else(|| "?".to_string());
    let state = if app.update_running { "running" } else { "done" };

    if app.update_output.is_empty() {
        let waiting = Paragraph::new("Starting compose update...")
            .style(Style::default().fg(Color::DarkGray))
            .block(pane_block(format!(" Update: {target} [{state}] ")));
        frame.render_widget(waiting, area);
        return;
    }

    let (start, end) = scroll_window(&mut app.scroll_back, app.update_output.len(), area);
    let title = if app.scroll_back > 0 {
        format!(" Update: {target} [{state}] [scrolled back {} lines] ", app.scroll_back)
    } else {
        format!(" Update: {target} [{state}] ")
    };
    let lines: Vec<Line> = app.update_output[start..end]
        .iter()
        .map(|l| {
            if l.starts_with("$ ") {
                Line::styled(l.as_str(), Style::default().fg(DOCKER_BLUE).add_modifier(Modifier::BOLD))
            } else {
                Line::raw(l.as_str())
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(pane_block(title)), area);
}

/// Clamp the scroll offset to the buffer and return the visible slice
/// bounds. Offset zero follows the tail; larger offsets walk back in time.
fn scroll_window(scroll_back: &mut usize, len: usize, area: Rect) -> (usize, usize) {
    let visible = area.height.saturating_sub(2) as usize;
    let max_back = len.saturating_sub(visible);
    if *scroll_back > max_back {
        *scroll_back = max_back;
    }
    let end = len - *scroll_back;
    (end.saturating_sub(visible), end)
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    // Show only the bindings that apply to the current view.
    let bindings: &[(&str, &str)] = match app.view {
        View::Details => &[
            ("↑/↓ j/k", "navigate"),
            ("enter", "logs"),
            ("s", "start/stop"),
            ("r", "restart"),
            ("u", "update"),
            ("q", "quit"),
        ],
        View::Logs => &[
            ("↑/↓ j/k", "switch container"),
            ("pgup/pgdn", "scroll"),
            ("end", "follow"),
            ("esc", "back"),
            ("q", "quit"),
        ],
        View::Update => &[
            ("pgup/pgdn", "scroll"),
            ("end", "follow"),
            ("enter", "logs"),
            ("esc", "back"),
            ("q", "quit"),
        ],
    };

    let key_style = Style::default().fg(DOCKER_BLUE).add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(Color::Gray);
    let separator_style = Style::default().fg(Color::DarkGray);

    let mut spans = vec![Span::raw(" ")];
    for (i, (key, label)) in bindings.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" | ", separator_style));
        }
        spans.push(Span::styled(*key, key_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(*label, label_style));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn pane_block(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DOCKER_BLUE))
        .title(title.into())
}

fn kv_line<'a>(label: &'a str, value: Span<'a>) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!(" {label:<9}"), Style::default().fg(Color::Gray)),
        value,
    ])
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::{format_bytes, scroll_window, Rect};

    #[test]
    fn scroll_window_follows_tail_and_clamps() {
        // 10-row pane: 8 visible lines inside the borders
        let area = Rect::new(0, 0, 80, 10);

        // Follow mode shows the last 8 of 100 lines
        let mut back = 0;
        assert_eq!(scroll_window(&mut back, 100, area), (92, 100));

        // Scrolled back 20 lines shifts the window up by 20
        let mut back = 20;
        assert_eq!(scroll_window(&mut back, 100, area), (72, 80));

        // Overshoot clamps to the top of the buffer
        let mut back = usize::MAX / 2;
        assert_eq!(scroll_window(&mut back, 100, area), (0, 8));
        assert_eq!(back, 92);

        // A buffer smaller than the pane never scrolls
        let mut back = 50;
        assert_eq!(scroll_window(&mut back, 5, area), (0, 5));
        assert_eq!(back, 0);
    }

    #[test]
    fn format_bytes_picks_sensible_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
