use std::io::{self, Stdout, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use arboard::Clipboard;
use base64::Engine;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, KeyEvent, MouseEvent,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table,
    TableState, Tabs, Wrap,
};
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::app::{App, DetailTab, Focus};
use crate::config::ThemeName;

pub enum TerminalEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize,
}

pub struct TerminalHandle {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalHandle {
    pub fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
            .context("failed to enter alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).context("failed to create terminal backend")?;
        Ok(Self { terminal })
    }

    pub fn draw<F>(&mut self, draw: F) -> Result<()>
    where
        F: FnOnce(&mut ratatui::Frame),
    {
        self.terminal.draw(draw).context("failed to draw frame")?;
        Ok(())
    }

    pub fn viewport(&self) -> Result<ratatui::layout::Rect> {
        let size = self
            .terminal
            .size()
            .context("failed to query terminal size")?;
        Ok(ratatui::layout::Rect::new(0, 0, size.width, size.height))
    }
}

impl Drop for TerminalHandle {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        let _ = self.terminal.show_cursor();
    }
}

pub struct TerminalEvents {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
    rx: UnboundedReceiver<TerminalEvent>,
}

impl TerminalEvents {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let join = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                match event::poll(Duration::from_millis(100)) {
                    Ok(true) => match event::read() {
                        Ok(CrosstermEvent::Key(key)) => {
                            let _ = tx.send(TerminalEvent::Key(key));
                        }
                        Ok(CrosstermEvent::Mouse(mouse)) => {
                            let _ = tx.send(TerminalEvent::Mouse(mouse));
                        }
                        Ok(CrosstermEvent::Resize(_, _)) => {
                            let _ = tx.send(TerminalEvent::Resize);
                        }
                        Ok(_) => {}
                        Err(_) => {}
                    },
                    Ok(false) => {}
                    Err(_) => {}
                }
            }
        });

        Self {
            stop,
            join: Some(join),
            rx,
        }
    }

    pub async fn recv(&mut self) -> Option<TerminalEvent> {
        self.rx.recv().await
    }
}

impl Drop for TerminalEvents {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub struct ClipboardHandle {
    clipboard: Option<Clipboard>,
    background_jobs: Vec<Child>,
}

impl ClipboardHandle {
    pub fn new() -> Self {
        Self {
            clipboard: Clipboard::new().ok(),
            background_jobs: Vec::new(),
        }
    }

    pub fn copy(&mut self, value: &str) -> Result<()> {
        self.reap_jobs();
        let mut errors = Vec::new();

        for backend in preferred_clipboard_backends() {
            match copy_with_command(backend, value) {
                Ok(Some(child)) => {
                    self.background_jobs.push(child);
                    return Ok(());
                }
                Ok(None) => return Ok(()),
                Err(error) => errors.push(format!("{}: {error}", backend.program)),
            }
        }

        if let Some(clipboard) = self.clipboard.as_mut() {
            match clipboard.set_text(value.to_string()) {
                Ok(()) => return Ok(()),
                Err(error) => errors.push(format!("arboard: {error}")),
            }
        } else {
            errors.push("arboard: unavailable".into());
        }

        match copy_with_osc52(value) {
            Ok(()) => return Ok(()),
            Err(error) => errors.push(format!("osc52: {error}")),
        }

        anyhow::bail!(errors.join("; "))
    }

    fn reap_jobs(&mut self) {
        let mut active = Vec::with_capacity(self.background_jobs.len());
        for mut child in self.background_jobs.drain(..) {
            match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) => active.push(child),
                Err(_) => {}
            }
        }
        self.background_jobs = active;
    }
}

#[derive(Clone, Copy)]
struct ClipboardBackend {
    program: &'static str,
    args: &'static [&'static str],
}

fn clipboard_backends() -> &'static [ClipboardBackend] {
    &[
        ClipboardBackend {
            program: "wl-copy",
            args: &[],
        },
        ClipboardBackend {
            program: "xclip",
            args: &["-selection", "clipboard"],
        },
        ClipboardBackend {
            program: "xsel",
            args: &["--clipboard", "--input"],
        },
        ClipboardBackend {
            program: "pbcopy",
            args: &[],
        },
    ]
}

fn preferred_clipboard_backends() -> Vec<ClipboardBackend> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some();

    clipboard_backends()
        .iter()
        .copied()
        .filter(|backend| match backend.program {
            "wl-copy" | "wl-paste" => wayland,
            "xclip" | "xsel" => x11,
            "pbcopy" => cfg!(target_os = "macos"),
            _ => true,
        })
        .collect()
}

fn copy_with_command(backend: ClipboardBackend, value: &str) -> Result<Option<Child>> {
    let mut child = Command::new(backend.program)
        .args(backend.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn {}", backend.program))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(value.as_bytes())
            .with_context(|| "failed to write clipboard payload")?;
    }
    let _ = child.stdin.take();

    thread::sleep(Duration::from_millis(20));
    match child.try_wait() {
        Ok(Some(status)) if status.success() => Ok(None),
        Ok(Some(status)) => anyhow::bail!("exit status {status}"),
        Ok(None) => Ok(Some(child)),
        Err(error) => Err(error).with_context(|| "clipboard command did not finish cleanly"),
    }
}

fn copy_with_osc52(value: &str) -> Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(value);
    let sequence = if std::env::var_os("TMUX").is_some() {
        format!("\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\")
    } else if std::env::var("TERM")
        .map(|term| term.contains("screen"))
        .unwrap_or(false)
    {
        format!("\x1bP\x1b]52;c;{encoded}\x07\x1b\\")
    } else {
        format!("\x1b]52;c;{encoded}\x07")
    };

    let mut stdout = io::stdout();
    stdout
        .write_all(sequence.as_bytes())
        .context("failed to write OSC52 sequence")?;
    stdout.flush().context("failed to flush OSC52 sequence")?;
    Ok(())
}

pub fn render(frame: &mut ratatui::Frame, app: &App) {
    let theme = theme(app.runtime.theme);
    let root = frame.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(root);

    render_main(frame, layout[0], app, theme);
    render_status(frame, layout[1], app, theme);
    render_footer(frame, layout[2], app, theme);

    if app.confirm_remove {
        render_remove_dialog(frame, centered_rect(58, 5, root), theme);
    }
}

fn render_main(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: Theme) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
        .split(area);

    render_container_list(frame, columns[0], app, theme);
    render_detail_pane(frame, columns[1], app, theme);
}

fn render_container_list(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: Theme) {
    let block = titled_block(
        "Containers",
        app.focus == Focus::Containers,
        theme,
        Style::default().bg(theme.panel),
    );
    let inner = block.inner(area);
    let (view, scrollbar_area) = split_right_scrollbar(inner);
    let scrollbar_area = trim_top(scrollbar_area, 1);
    frame.render_widget(block, area);

    let containers = app.filtered_containers();
    let header = Row::new([
        "State", "Name", "Project", "Service", "Image", "Ports", "Status",
    ])
    .style(
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    );
    let rows = containers.iter().map(|container| {
        Row::new(vec![
            Cell::from(container.state.clone()),
            Cell::from(container.name.clone()),
            Cell::from(container.project.clone().unwrap_or_else(|| "-".into())),
            Cell::from(container.service.clone().unwrap_or_else(|| "-".into())),
            Cell::from(container.image.clone()),
            Cell::from(container.ports_summary()),
            Cell::from(container.status.clone()),
        ])
    });

    let widths = [
        Constraint::Length(10),
        Constraint::Length(20),
        Constraint::Length(14),
        Constraint::Length(12),
        Constraint::Length(22),
        Constraint::Length(20),
        Constraint::Min(12),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().bg(theme.selection).fg(theme.background))
        .column_spacing(1);

    let mut state = TableState::default()
        .with_offset(app.container_offset)
        .with_selected(app.selected_visible_index());
    frame.render_stateful_widget(table, view, &mut state);
    render_vertical_scrollbar(
        frame,
        scrollbar_area,
        containers.len(),
        view.height.saturating_sub(1) as usize,
        app.container_offset,
        app.focus == Focus::Containers,
        theme,
    );
}

fn render_detail_pane(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: Theme) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(8)])
        .split(area);

    let titles = DetailTab::ALL
        .iter()
        .map(|tab| Line::from(format!(" {} ", tab.title())))
        .collect::<Vec<_>>();
    let tabs = Tabs::new(titles)
        .block(titled_block(
            "Details",
            app.focus == Focus::Detail,
            theme,
            Style::default().bg(theme.panel),
        ))
        .select(
            DetailTab::ALL
                .iter()
                .position(|tab| *tab == app.detail_tab)
                .unwrap_or_default(),
        )
        .highlight_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().fg(theme.text));
    frame.render_widget(tabs, sections[0]);

    match app.detail_tab {
        DetailTab::Logs => render_logs(frame, sections[1], app, theme),
        _ => render_detail_list(frame, sections[1], app, theme),
    }
}

fn render_logs(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: Theme) {
    let block = titled_block(
        "Logs",
        app.focus == Focus::Detail,
        theme,
        Style::default().bg(theme.panel_alt),
    );
    let inner = block.inner(area);
    let logs_region = trim_bottom(inner, 1);
    let (view, scrollbar_area) = split_right_scrollbar(logs_region);
    frame.render_widget(block, area);

    let rows = app.visible_log_rows();
    let selected_range = app.logs.selected_range();
    let items = if rows.is_empty() {
        let placeholder = if app.logs.loading {
            "Loading logs..."
        } else {
            "No log lines"
        };
        vec![ListItem::new(Line::from(placeholder))]
    } else {
        rows.iter()
            .map(|row| {
                let line = highlight_search(row.text.clone(), &app.logs.search_query, theme);
                let mut item = ListItem::new(line);
                let in_multi = selected_range
                    .map(|(start, end)| row.entry_index >= start && row.entry_index <= end)
                    .unwrap_or(false);
                if in_multi {
                    item = item.style(Style::default().bg(theme.selection).fg(theme.background));
                } else if row.entry_index == app.logs.selected {
                    item = item.style(
                        Style::default()
                            .bg(theme.selection_alt)
                            .fg(theme.text)
                            .add_modifier(Modifier::BOLD),
                    );
                }
                item
            })
            .collect::<Vec<_>>()
    };

    let list = List::new(items);
    frame.render_widget(list, view);
    render_vertical_scrollbar(
        frame,
        scrollbar_area,
        app.logs.filtered_entries().len(),
        view.height as usize,
        app.logs.scroll_top,
        app.focus == Focus::Detail,
        theme,
    );

    let subtitle = match app.logs.regex_error() {
        Some(error) => format!("filter error: {error}"),
        None => format!(
            "follow:{}  wrap:{}  timestamps:{}  filter:{} ({})  search:{}  selected:{}",
            if app.logs.follow { "on" } else { "off" },
            if app.logs.wrap { "on" } else { "off" },
            if app.logs.show_timestamps {
                "on"
            } else {
                "off"
            },
            if app.logs.filter_query.is_empty() {
                "<none>"
            } else {
                &app.logs.filter_query
            },
            match app.logs.filter_mode {
                crate::model::LogFilterMode::Substring => "substring",
                crate::model::LogFilterMode::Regex => "regex",
            },
            if app.logs.search_query.is_empty() {
                "<none>"
            } else {
                &app.logs.search_query
            },
            selected_range
                .map(|(start, end)| format!("{} lines", end.saturating_sub(start) + 1))
                .unwrap_or_else(|| "1 line".into())
        ),
    };
    frame.render_widget(
        Paragraph::new(subtitle).style(Style::default().fg(theme.muted).bg(theme.panel_alt)),
        Rect {
            x: inner.x,
            y: inner.y.saturating_add(inner.height.saturating_sub(1)),
            width: inner.width,
            height: 1,
        },
    );
}

fn render_detail_list(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: Theme) {
    let block = titled_block(
        app.detail_tab.title(),
        app.focus == Focus::Detail,
        theme,
        Style::default().bg(theme.panel_alt),
    );
    let inner = block.inner(area);
    let (view, scrollbar_area) = split_right_scrollbar(inner);
    frame.render_widget(block, area);

    let items = app.selected_detail_items();
    let rows = if items.is_empty() {
        vec![ListItem::new(Line::from("No data loaded"))]
    } else {
        items
            .iter()
            .map(|item| {
                let line = Line::from(vec![
                    Span::styled(
                        format!("{: <18}", item.label),
                        Style::default()
                            .fg(theme.muted)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(item.value.clone()),
                ]);
                ListItem::new(line)
            })
            .collect::<Vec<_>>()
    };

    let list = List::new(rows)
        .highlight_style(Style::default().bg(theme.selection).fg(theme.background))
        .highlight_symbol("▌ ");
    let mut state = ListState::default()
        .with_offset(app.detail_offset)
        .with_selected(Some(
            app.selected_detail_index()
                .min(items.len().saturating_sub(1)),
        ));
    frame.render_stateful_widget(list, view, &mut state);
    render_vertical_scrollbar(
        frame,
        scrollbar_area,
        items.len(),
        view.height as usize,
        app.detail_offset,
        app.focus == Focus::Detail,
        theme,
    );
}

fn render_status(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: Theme) {
    let status = if let Some(input) = &app.input {
        match input.kind {
            crate::app::InputKind::LogFilter => format!("Filter: {}", input.value),
            crate::app::InputKind::LogSearch => format!("Search: {}", input.value),
        }
    } else {
        app.status.clone()
    };

    let paragraph = Paragraph::new(status)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(theme.border))
                .style(Style::default().bg(theme.background)),
        )
        .style(Style::default().fg(if app.last_error.is_some() {
            theme.error
        } else {
            theme.text
        }));
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: Theme) {
    let footer = app.help_text();
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(theme.muted).bg(theme.background)),
        area,
    );
}

fn render_remove_dialog(frame: &mut ratatui::Frame, area: Rect, theme: Theme) {
    frame.render_widget(Clear, area);
    let paragraph = Paragraph::new("Remove the selected container?\nEnter confirms, Esc cancels.")
        .block(
            Block::default()
                .title("Confirm remove")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.error))
                .border_type(BorderType::Rounded)
                .style(Style::default().bg(theme.panel_alt)),
        )
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(theme.text));
    frame.render_widget(paragraph, area);
}

fn titled_block(title: &'static str, focused: bool, theme: Theme, style: Style) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused { theme.accent } else { theme.border }))
        .style(style)
}

fn highlight_search(content: String, query: &str, theme: Theme) -> Line<'static> {
    if query.trim().is_empty() {
        return Line::from(content);
    }

    let haystack = content.to_ascii_lowercase();
    let needle = query.to_ascii_lowercase();
    if let Some(index) = haystack.find(&needle) {
        let end = index + needle.len();
        return Line::from(vec![
            Span::raw(content[..index].to_string()),
            Span::styled(
                content[index..end].to_string(),
                Style::default()
                    .bg(theme.accent)
                    .fg(theme.background)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(content[end..].to_string()),
        ]);
    }

    Line::from(content)
}

fn render_vertical_scrollbar(
    frame: &mut ratatui::Frame,
    area: Rect,
    content_length: usize,
    viewport_length: usize,
    position: usize,
    focused: bool,
    theme: Theme,
) {
    if area.width == 0 || area.height == 0 || content_length == 0 {
        return;
    }

    let (thumb_top, thumb_height) = scrollbar_thumb_bounds(
        content_length,
        viewport_length.max(1),
        position,
        area.height,
    );
    let lines = (0..area.height)
        .map(|offset| {
            let (symbol, style) =
                if offset >= thumb_top && offset < thumb_top.saturating_add(thumb_height) {
                    (
                        "█",
                        Style::default().fg(if focused { theme.accent } else { theme.muted }),
                    )
                } else {
                    ("│", Style::default().fg(theme.border))
                };
            Line::from(Span::styled(symbol, style))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), area);
}

fn split_right_scrollbar(area: Rect) -> (Rect, Rect) {
    let scrollbar_width = area.width.min(1);
    let view_width = area.width.saturating_sub(scrollbar_width);

    (
        Rect {
            x: area.x,
            y: area.y,
            width: view_width,
            height: area.height,
        },
        Rect {
            x: area.x.saturating_add(view_width),
            y: area.y,
            width: scrollbar_width,
            height: area.height,
        },
    )
}

fn trim_top(area: Rect, rows: u16) -> Rect {
    Rect {
        x: area.x,
        y: area.y.saturating_add(rows.min(area.height)),
        width: area.width,
        height: area.height.saturating_sub(rows),
    }
}

fn trim_bottom(area: Rect, rows: u16) -> Rect {
    Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height.saturating_sub(rows),
    }
}

fn scrollbar_thumb_bounds(
    content_length: usize,
    viewport_length: usize,
    position: usize,
    track_height: u16,
) -> (u16, u16) {
    if track_height == 0 || content_length == 0 {
        return (0, 0);
    }
    if content_length <= viewport_length {
        return (0, track_height);
    }

    let thumb_height = (((viewport_length as f64 / content_length as f64) * track_height as f64)
        .round() as u16)
        .clamp(1, track_height);
    let max_position = content_length.saturating_sub(viewport_length);
    let travel = track_height.saturating_sub(thumb_height);
    let thumb_top = if max_position == 0 || travel == 0 {
        0
    } else {
        ((position.min(max_position) as f64 / max_position as f64) * travel as f64).round() as u16
    };
    (thumb_top, thumb_height)
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let width = area.width.saturating_mul(percent_x) / 100;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[derive(Clone, Copy)]
struct Theme {
    background: Color,
    panel: Color,
    panel_alt: Color,
    text: Color,
    muted: Color,
    border: Color,
    accent: Color,
    selection: Color,
    selection_alt: Color,
    error: Color,
}

fn theme(theme: ThemeName) -> Theme {
    match theme {
        ThemeName::Graphite => Theme {
            background: Color::Rgb(17, 20, 24),
            panel: Color::Rgb(24, 28, 33),
            panel_alt: Color::Rgb(30, 35, 41),
            text: Color::Rgb(230, 236, 241),
            muted: Color::Rgb(132, 144, 156),
            border: Color::Rgb(73, 82, 91),
            accent: Color::Rgb(240, 113, 82),
            selection: Color::Rgb(56, 92, 129),
            selection_alt: Color::Rgb(42, 54, 66),
            error: Color::Rgb(232, 87, 87),
        },
        ThemeName::Ember => Theme {
            background: Color::Rgb(26, 18, 15),
            panel: Color::Rgb(38, 27, 22),
            panel_alt: Color::Rgb(49, 35, 28),
            text: Color::Rgb(248, 235, 222),
            muted: Color::Rgb(176, 150, 128),
            border: Color::Rgb(121, 85, 62),
            accent: Color::Rgb(239, 145, 70),
            selection: Color::Rgb(123, 74, 39),
            selection_alt: Color::Rgb(82, 52, 36),
            error: Color::Rgb(242, 96, 96),
        },
        ThemeName::Ocean => Theme {
            background: Color::Rgb(9, 21, 30),
            panel: Color::Rgb(14, 33, 46),
            panel_alt: Color::Rgb(19, 44, 59),
            text: Color::Rgb(224, 241, 245),
            muted: Color::Rgb(121, 155, 168),
            border: Color::Rgb(68, 103, 116),
            accent: Color::Rgb(70, 173, 209),
            selection: Color::Rgb(33, 89, 110),
            selection_alt: Color::Rgb(23, 58, 72),
            error: Color::Rgb(236, 100, 101),
        },
    }
}
