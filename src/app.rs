use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::config::RuntimeConfig;
use crate::docker::{ContainerAction, DockerCommand, DockerEvent};
use crate::model::{
    ContainerDetails, ContainerRecord, DetailItem, LogEntry, LogFilterMode,
    apply_container_filters, sort_containers,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Containers,
    Detail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DetailTab {
    Logs,
    Overview,
    Env,
    Ports,
    Mounts,
    Health,
}

impl DetailTab {
    pub const ALL: [Self; 6] = [
        Self::Logs,
        Self::Overview,
        Self::Env,
        Self::Ports,
        Self::Mounts,
        Self::Health,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Logs => "Logs",
            Self::Overview => "Overview",
            Self::Env => "Env",
            Self::Ports => "Ports",
            Self::Mounts => "Mounts",
            Self::Health => "Health",
        }
    }
}

#[derive(Clone, Debug)]
pub enum UiCommand {
    Quit,
    Copy(String),
    Docker(DockerCommand),
    SetStatus(String),
}

#[derive(Clone, Debug)]
pub(crate) enum InputKind {
    LogFilter,
    LogSearch,
}

#[derive(Clone, Debug)]
pub(crate) struct InputState {
    pub kind: InputKind,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleLogRow {
    pub entry_index: usize,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollbarTarget {
    Containers,
    Detail,
    Logs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScrollbarDrag {
    target: ScrollbarTarget,
    grab_offset: u16,
}

pub struct App {
    pub(crate) runtime: RuntimeConfig,
    pub(crate) containers: Vec<ContainerRecord>,
    pub(crate) selected_id: Option<String>,
    pub(crate) focus: Focus,
    pub(crate) detail_tab: DetailTab,
    pub(crate) show_stopped: bool,
    pub(crate) project_filter: Option<String>,
    pub(crate) input: Option<InputState>,
    pub(crate) confirm_remove: bool,
    pub(crate) status: String,
    pub(crate) last_error: Option<String>,
    pub(crate) details: Option<ContainerDetails>,
    pub(crate) logs: LogView,
    pub(crate) detail_cursor: BTreeMap<DetailTab, usize>,
    pub(crate) container_offset: usize,
    pub(crate) detail_offset: usize,
    viewport: Rect,
    scrollbar_drag: Option<ScrollbarDrag>,
    startup_container_query: Option<String>,
}

impl App {
    pub fn new(runtime: RuntimeConfig, config_path: Option<std::path::PathBuf>) -> Self {
        let show_timestamps = runtime.show_timestamps;
        let log_backlog_lines = runtime.log_backlog_lines;

        Self {
            status: config_path
                .as_ref()
                .map(|path| format!("Config: {}", path.display()))
                .unwrap_or_else(|| "Config: defaults".into()),
            runtime,
            containers: Vec::new(),
            selected_id: None,
            focus: Focus::Containers,
            detail_tab: DetailTab::Logs,
            show_stopped: false,
            project_filter: None,
            input: None,
            confirm_remove: false,
            last_error: None,
            details: None,
            logs: LogView::new(show_timestamps, log_backlog_lines.saturating_mul(20)),
            detail_cursor: BTreeMap::new(),
            container_offset: 0,
            detail_offset: 0,
            viewport: Rect::new(0, 0, 0, 0),
            scrollbar_drag: None,
            startup_container_query: None,
        }
        .with_runtime_defaults()
    }

    fn with_runtime_defaults(mut self) -> Self {
        self.show_stopped = self.runtime.show_stopped_by_default;
        self.project_filter = self.runtime.project_filter.clone();
        self.startup_container_query = self.runtime.startup_container_query.clone();
        self
    }

    pub fn bootstrap_commands(&self) -> Vec<UiCommand> {
        Vec::new()
    }

    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.last_error = None;
    }

    pub fn set_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.status = message.clone();
        self.last_error = Some(message);
    }

    pub fn set_viewport(&mut self, viewport: Rect) {
        let changed = self.viewport != viewport;
        self.viewport = viewport;
        if changed {
            self.ensure_container_visible();
            self.ensure_detail_visible();
            self.logs.ensure_visible(self.log_rows_height());
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<UiCommand> {
        if self.confirm_remove {
            return self.handle_remove_confirmation(key);
        }

        if self.input.is_some() {
            return self.handle_input_key(key);
        }

        if matches_binding(
            key,
            self.runtime.keymap.quit.as_deref(),
            "q",
            Some(("c", KeyModifiers::CONTROL)),
        ) {
            return vec![UiCommand::Quit];
        }
        if matches_binding(
            key,
            self.runtime.keymap.toggle_stopped.as_deref(),
            "a",
            None,
        ) {
            self.show_stopped = !self.show_stopped;
            return self.reconcile_selection();
        }
        if matches_binding(key, self.runtime.keymap.copy.as_deref(), "y", None) {
            return self
                .copy_selected_value()
                .map(UiCommand::Copy)
                .into_iter()
                .collect();
        }
        if matches_binding(key, self.runtime.keymap.start_stop.as_deref(), "s", None) {
            return self
                .selected_container()
                .map(|container| {
                    UiCommand::Docker(DockerCommand::Action {
                        id: container.id.clone(),
                        action: ContainerAction::StartStop,
                    })
                })
                .into_iter()
                .collect();
        }
        if matches_binding(key, self.runtime.keymap.restart.as_deref(), "r", None) {
            return self
                .selected_container()
                .map(|container| {
                    UiCommand::Docker(DockerCommand::Action {
                        id: container.id.clone(),
                        action: ContainerAction::Restart,
                    })
                })
                .into_iter()
                .collect();
        }
        if matches_binding(key, self.runtime.keymap.remove.as_deref(), "D", None) {
            if self.selected_container().is_some() {
                self.confirm_remove = true;
                self.status =
                    "Press Enter to remove the selected container, or Esc to cancel".into();
            }
            return Vec::new();
        }

        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Containers => Focus::Detail,
                    Focus::Detail => Focus::Containers,
                };
                Vec::new()
            }
            KeyCode::Left if self.focus == Focus::Detail => {
                self.cycle_tab(-1);
                self.ensure_detail_visible();
                Vec::new()
            }
            KeyCode::Right if self.focus == Focus::Detail => {
                self.cycle_tab(1);
                self.ensure_detail_visible();
                Vec::new()
            }
            KeyCode::Char('1') => {
                self.detail_tab = DetailTab::Logs;
                self.logs.ensure_visible(self.log_rows_height());
                Vec::new()
            }
            KeyCode::Char('2') => {
                self.detail_tab = DetailTab::Overview;
                self.ensure_detail_visible();
                Vec::new()
            }
            KeyCode::Char('3') => {
                self.detail_tab = DetailTab::Env;
                self.ensure_detail_visible();
                Vec::new()
            }
            KeyCode::Char('4') => {
                self.detail_tab = DetailTab::Ports;
                self.ensure_detail_visible();
                Vec::new()
            }
            KeyCode::Char('5') => {
                self.detail_tab = DetailTab::Mounts;
                self.ensure_detail_visible();
                Vec::new()
            }
            KeyCode::Char('6') => {
                self.detail_tab = DetailTab::Health;
                self.ensure_detail_visible();
                Vec::new()
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_up(),
            KeyCode::Down | KeyCode::Char('j') => self.move_down(),
            KeyCode::PageUp => self.move_page(-10),
            KeyCode::PageDown => self.move_page(10),
            KeyCode::Home => self.jump_to_edge(false),
            KeyCode::End => self.jump_to_edge(true),
            KeyCode::Char('/') if self.detail_tab == DetailTab::Logs => {
                self.input = Some(InputState {
                    kind: InputKind::LogSearch,
                    value: self.logs.search_query.clone(),
                });
                Vec::new()
            }
            KeyCode::Char('f') if self.detail_tab == DetailTab::Logs => {
                self.input = Some(InputState {
                    kind: InputKind::LogFilter,
                    value: self.logs.filter_query.clone(),
                });
                Vec::new()
            }
            KeyCode::Char('m') if self.detail_tab == DetailTab::Logs => {
                self.logs.toggle_filter_mode();
                Vec::new()
            }
            KeyCode::Char('n') if self.detail_tab == DetailTab::Logs => {
                self.logs.jump_to_match(false);
                self.logs.ensure_visible(self.log_rows_height());
                Vec::new()
            }
            KeyCode::Char('N') if self.detail_tab == DetailTab::Logs => {
                self.logs.jump_to_match(true);
                self.logs.ensure_visible(self.log_rows_height());
                Vec::new()
            }
            KeyCode::Char(' ') if self.detail_tab == DetailTab::Logs => {
                self.logs.toggle_follow();
                self.logs.ensure_visible(self.log_rows_height());
                Vec::new()
            }
            KeyCode::Char('w') if self.detail_tab == DetailTab::Logs => {
                self.logs.wrap = !self.logs.wrap;
                self.logs.ensure_visible(self.log_rows_height());
                Vec::new()
            }
            KeyCode::Char('t') if self.detail_tab == DetailTab::Logs => {
                self.logs.show_timestamps = !self.logs.show_timestamps;
                self.logs.ensure_visible(self.log_rows_height());
                Vec::new()
            }
            KeyCode::Char('?') | KeyCode::Char('h') => vec![UiCommand::SetStatus(self.help_text())],
            _ => Vec::new(),
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> Vec<UiCommand> {
        if self.confirm_remove || self.input.is_some() {
            return Vec::new();
        }

        let layout = self.layout();
        let x = mouse.column;
        let y = mouse.row;

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(target) = self.scrollbar_target_at(layout, x, y) {
                    return self.begin_scrollbar_drag(target, y);
                }
                if contains(layout.container_view, x, y) {
                    self.focus = Focus::Containers;
                    return self.select_container_by_mouse_row(y);
                }
                if contains(layout.detail_tabs_inner, x, y) {
                    self.focus = Focus::Detail;
                    return self.select_tab_by_mouse_x(x, layout.detail_tabs_inner);
                }
                if self.detail_tab == DetailTab::Logs && contains(layout.logs_view, x, y) {
                    self.focus = Focus::Detail;
                    self.scrollbar_drag = None;
                    self.select_log_row_from_mouse(y);
                    return Vec::new();
                }
                if self.detail_tab != DetailTab::Logs && contains(layout.detail_view, x, y) {
                    self.focus = Focus::Detail;
                    self.scrollbar_drag = None;
                    self.select_detail_by_mouse_row(y);
                }
                Vec::new()
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.scrollbar_drag.is_some() {
                    self.update_scrollbar_drag(y);
                    return Vec::new();
                }
                if self.detail_tab == DetailTab::Logs
                    && self.logs.drag_active
                    && contains(layout.logs_view, x, y)
                {
                    self.update_log_drag(y);
                }
                Vec::new()
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.logs.finish_drag();
                self.scrollbar_drag = None;
                Vec::new()
            }
            MouseEventKind::ScrollUp => {
                if self.detail_tab == DetailTab::Logs && contains(layout.logs_outer, x, y) {
                    self.focus = Focus::Detail;
                    self.logs.move_selection(-1);
                    self.logs.ensure_visible(self.log_rows_height());
                    return Vec::new();
                }
                if contains(layout.container_outer, x, y) {
                    self.focus = Focus::Containers;
                    return self.move_container_selection(-1);
                }
                if contains(layout.detail_outer, x, y) {
                    self.focus = Focus::Detail;
                    self.move_detail_selection(-1);
                }
                Vec::new()
            }
            MouseEventKind::ScrollDown => {
                if self.detail_tab == DetailTab::Logs && contains(layout.logs_outer, x, y) {
                    self.focus = Focus::Detail;
                    self.logs.move_selection(1);
                    self.logs.ensure_visible(self.log_rows_height());
                    return Vec::new();
                }
                if contains(layout.container_outer, x, y) {
                    self.focus = Focus::Containers;
                    return self.move_container_selection(1);
                }
                if contains(layout.detail_outer, x, y) {
                    self.focus = Focus::Detail;
                    self.move_detail_selection(1);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    pub fn apply_docker_event(&mut self, event: DockerEvent) -> Vec<UiCommand> {
        match event {
            DockerEvent::Connected(message) => {
                self.set_status(message);
                Vec::new()
            }
            DockerEvent::ContainersUpdated(mut containers) => {
                sort_containers(&mut containers);
                self.containers = containers;
                if self.containers.is_empty() {
                    self.selected_id = None;
                    self.details = None;
                    self.logs.clear();
                    self.set_status("No containers found");
                    return Vec::new();
                }

                let running = self
                    .containers
                    .iter()
                    .filter(|container| container.is_running())
                    .count();
                self.set_status(format!(
                    "{} containers loaded ({} running)",
                    self.containers.len(),
                    running
                ));
                let commands = self.reconcile_selection();
                self.ensure_container_visible();
                commands
            }
            DockerEvent::InspectLoaded { id, details } => {
                if self.selected_id.as_deref() == Some(id.as_str()) {
                    self.details = Some(details);
                }
                Vec::new()
            }
            DockerEvent::LogsReset { id } => {
                if self.selected_id.as_deref() == Some(id.as_str()) {
                    self.logs.reset(id);
                }
                Vec::new()
            }
            DockerEvent::LogsReady { id } => {
                if self.selected_id.as_deref() == Some(id.as_str()) {
                    self.logs.finish_loading();
                }
                Vec::new()
            }
            DockerEvent::LogChunk { id, entries } => {
                if self.selected_id.as_deref() == Some(id.as_str()) {
                    self.logs.append(entries);
                    self.logs.finish_loading();
                    self.logs.ensure_visible(self.log_rows_height());
                }
                Vec::new()
            }
            DockerEvent::OperationSucceeded(message) => {
                self.set_status(message);
                let mut commands = vec![UiCommand::Docker(DockerCommand::RefreshContainers)];
                if let Some(container) = self.selected_container() {
                    commands.push(UiCommand::Docker(DockerCommand::LoadInspect {
                        id: container.id.clone(),
                    }));
                }
                commands
            }
            DockerEvent::OperationFailed(message) => {
                self.set_error(message);
                Vec::new()
            }
        }
    }

    pub fn filtered_containers(&self) -> Vec<&ContainerRecord> {
        apply_container_filters(
            &self.containers,
            self.show_stopped,
            self.project_filter.as_deref(),
            None,
        )
    }

    pub fn selected_visible_index(&self) -> Option<usize> {
        let selected = self.selected_id.as_deref()?;
        self.filtered_containers()
            .iter()
            .position(|container| container.id == selected)
    }

    pub fn selected_container(&self) -> Option<&ContainerRecord> {
        let selected = self.selected_id.as_deref()?;
        self.containers
            .iter()
            .find(|container| container.id == selected)
    }

    pub fn selected_detail_items(&self) -> &[DetailItem] {
        self.details
            .as_ref()
            .map(|details| details.items_for_tab(self.detail_tab))
            .unwrap_or(&[])
    }

    pub fn selected_detail_index(&self) -> usize {
        *self.detail_cursor.get(&self.detail_tab).unwrap_or(&0)
    }

    pub fn visible_log_rows(&self) -> Vec<VisibleLogRow> {
        self.logs
            .visible_rows(self.log_rows_height(), self.log_content_width())
    }

    fn cycle_tab(&mut self, delta: i32) {
        let current = DetailTab::ALL
            .iter()
            .position(|tab| *tab == self.detail_tab)
            .unwrap_or_default() as i32;
        let len = DetailTab::ALL.len() as i32;
        let next = (current + delta).rem_euclid(len) as usize;
        self.detail_tab = DetailTab::ALL[next];
    }

    fn move_up(&mut self) -> Vec<UiCommand> {
        match self.focus {
            Focus::Containers => self.move_container_selection(-1),
            Focus::Detail if self.detail_tab == DetailTab::Logs => {
                self.logs.move_selection(-1);
                self.logs.ensure_visible(self.log_rows_height());
                Vec::new()
            }
            Focus::Detail => {
                self.move_detail_selection(-1);
                Vec::new()
            }
        }
    }

    fn move_down(&mut self) -> Vec<UiCommand> {
        match self.focus {
            Focus::Containers => self.move_container_selection(1),
            Focus::Detail if self.detail_tab == DetailTab::Logs => {
                self.logs.move_selection(1);
                self.logs.ensure_visible(self.log_rows_height());
                Vec::new()
            }
            Focus::Detail => {
                self.move_detail_selection(1);
                Vec::new()
            }
        }
    }

    fn move_page(&mut self, delta: isize) -> Vec<UiCommand> {
        match self.focus {
            Focus::Containers => self.move_container_selection(delta),
            Focus::Detail if self.detail_tab == DetailTab::Logs => {
                self.logs.move_selection(delta);
                self.logs.ensure_visible(self.log_rows_height());
                Vec::new()
            }
            Focus::Detail => {
                self.move_detail_selection(delta);
                Vec::new()
            }
        }
    }

    fn jump_to_edge(&mut self, end: bool) -> Vec<UiCommand> {
        match self.focus {
            Focus::Containers => self.jump_container_selection(end),
            Focus::Detail if self.detail_tab == DetailTab::Logs => {
                self.logs.jump_to_edge(end);
                self.logs.ensure_visible(self.log_rows_height());
                Vec::new()
            }
            Focus::Detail => {
                let len = self.selected_detail_items().len();
                let cursor = if end { len.saturating_sub(1) } else { 0 };
                self.detail_cursor.insert(self.detail_tab, cursor);
                self.ensure_detail_visible();
                Vec::new()
            }
        }
    }

    fn move_container_selection(&mut self, delta: isize) -> Vec<UiCommand> {
        let visible = self.filtered_containers();
        if visible.is_empty() {
            return Vec::new();
        }

        let selected = self.selected_visible_index().unwrap_or_default() as isize;
        let max = visible.len().saturating_sub(1) as isize;
        let next = (selected + delta).clamp(0, max) as usize;
        let next_id = visible[next].id.clone();
        if self.selected_id.as_deref() == Some(next_id.as_str()) {
            return Vec::new();
        }

        self.selected_id = Some(next_id);
        self.details = None;
        self.logs.begin_loading(self.selected_id.clone());
        self.ensure_container_visible();
        self.selected_context_commands()
    }

    fn jump_container_selection(&mut self, end: bool) -> Vec<UiCommand> {
        let visible = self.filtered_containers();
        if visible.is_empty() {
            return Vec::new();
        }

        let next = if end { visible.len() - 1 } else { 0 };
        let next_id = visible[next].id.clone();
        if self.selected_id.as_deref() == Some(next_id.as_str()) {
            return Vec::new();
        }

        self.selected_id = Some(next_id);
        self.details = None;
        self.logs.begin_loading(self.selected_id.clone());
        self.ensure_container_visible();
        self.selected_context_commands()
    }

    fn move_detail_selection(&mut self, delta: isize) {
        let items = self.selected_detail_items();
        if items.is_empty() {
            return;
        }

        let current = self.selected_detail_index() as isize;
        let max = items.len().saturating_sub(1) as isize;
        let next = (current + delta).clamp(0, max) as usize;
        self.detail_cursor.insert(self.detail_tab, next);
        self.ensure_detail_visible();
    }

    fn selected_context_commands(&self) -> Vec<UiCommand> {
        let Some(container) = self.selected_container() else {
            return Vec::new();
        };

        vec![
            UiCommand::Docker(DockerCommand::LoadInspect {
                id: container.id.clone(),
            }),
            UiCommand::Docker(DockerCommand::WatchLogs {
                id: container.id.clone(),
            }),
        ]
    }

    fn reconcile_selection(&mut self) -> Vec<UiCommand> {
        let visible = self.filtered_containers();
        if visible.is_empty() {
            self.selected_id = None;
            self.details = None;
            self.logs.clear();
            return Vec::new();
        }

        let preserved = self
            .selected_id
            .as_deref()
            .and_then(|selected| visible.iter().find(|container| container.id == selected))
            .map(|container| container.id.clone());

        let next_id = preserved.or_else(|| {
            self.startup_container_query.as_ref().and_then(|query| {
                visible
                    .iter()
                    .find(|container| container.matches_query(query))
                    .map(|container| container.id.clone())
            })
        });

        let next_id = next_id.unwrap_or_else(|| visible[0].id.clone());
        let changed = self.selected_id.as_deref() != Some(next_id.as_str());
        self.selected_id = Some(next_id);
        self.startup_container_query = None;
        self.ensure_container_visible();

        if changed {
            self.details = None;
            self.logs.begin_loading(self.selected_id.clone());
            self.selected_context_commands()
        } else {
            Vec::new()
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> Vec<UiCommand> {
        let Some(input) = self.input.as_mut() else {
            return Vec::new();
        };

        match key.code {
            KeyCode::Esc => {
                self.input = None;
                Vec::new()
            }
            KeyCode::Enter => {
                let completed = self.input.take().expect("input present");
                match completed.kind {
                    InputKind::LogFilter => self.logs.set_filter_query(completed.value),
                    InputKind::LogSearch => self.logs.set_search_query(completed.value),
                }
                self.logs.ensure_visible(self.log_rows_height());
                Vec::new()
            }
            KeyCode::Backspace => {
                input.value.pop();
                Vec::new()
            }
            KeyCode::Char(character) => {
                input.value.push(character);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn handle_remove_confirmation(&mut self, key: KeyEvent) -> Vec<UiCommand> {
        match key.code {
            KeyCode::Esc => {
                self.confirm_remove = false;
                self.set_status("Remove canceled");
                Vec::new()
            }
            KeyCode::Enter => {
                self.confirm_remove = false;
                self.selected_container()
                    .map(|container| {
                        UiCommand::Docker(DockerCommand::Action {
                            id: container.id.clone(),
                            action: ContainerAction::Remove,
                        })
                    })
                    .into_iter()
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn copy_selected_value(&self) -> Option<String> {
        if self.focus == Focus::Detail && self.detail_tab == DetailTab::Logs {
            return self.logs.selected_text();
        }

        if self.focus == Focus::Detail {
            return self
                .selected_detail_items()
                .get(self.selected_detail_index())
                .map(|item| format!("{}: {}", item.label, item.value));
        }

        self.selected_container()
            .map(|container| container.id.clone())
    }

    pub(crate) fn help_text(&self) -> String {
        let mut help = format!(
            "{} quit | tab focus | arrows move | 1-6 tabs | {} copy | {} toggle stopped | {} start/stop | {} restart | {} remove",
            display_binding(self.runtime.keymap.quit.as_deref(), "q"),
            display_binding(self.runtime.keymap.copy.as_deref(), "y"),
            display_binding(self.runtime.keymap.toggle_stopped.as_deref(), "a"),
            display_binding(self.runtime.keymap.start_stop.as_deref(), "s"),
            display_binding(self.runtime.keymap.restart.as_deref(), "r"),
            display_binding(self.runtime.keymap.remove.as_deref(), "D"),
        );

        if self.detail_tab == DetailTab::Logs {
            help.push_str(
                " | / search | f filter | m regex | space follow | w wrap | t timestamps | mouse drag select | drag scrollbar",
            );
        }
        help
    }

    fn layout(&self) -> AppLayout {
        let root = self.viewport;
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(2),
                Constraint::Length(1),
            ])
            .split(root);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
            .split(outer[1]);
        let detail_sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(8)])
            .split(columns[1]);
        AppLayout::new(columns[0], detail_sections[0], detail_sections[1])
    }

    fn log_rows_height(&self) -> usize {
        self.layout().logs_view.height.max(1) as usize
    }

    fn log_content_width(&self) -> usize {
        self.layout().logs_view.width.max(3) as usize
    }

    fn container_rows_height(&self) -> usize {
        self.layout().container_scrollbar.height.max(1) as usize
    }

    fn detail_rows_height(&self) -> usize {
        self.layout().detail_view.height.max(1) as usize
    }

    fn container_scroll_metrics(&self) -> ScrollMetrics {
        let layout = self.layout();
        ScrollMetrics::new(
            self.filtered_containers().len(),
            self.container_rows_height(),
            self.container_offset,
            layout.container_scrollbar,
        )
    }

    fn detail_scroll_metrics(&self) -> ScrollMetrics {
        let layout = self.layout();
        ScrollMetrics::new(
            self.selected_detail_items().len(),
            self.detail_rows_height(),
            self.detail_offset,
            layout.detail_scrollbar,
        )
    }

    fn log_scroll_metrics(&self) -> ScrollMetrics {
        let layout = self.layout();
        ScrollMetrics::new(
            self.logs.filtered_len(),
            self.log_rows_height(),
            self.logs.scroll_top,
            layout.logs_scrollbar,
        )
    }

    fn scrollbar_target_at(&self, layout: AppLayout, x: u16, y: u16) -> Option<ScrollbarTarget> {
        if contains(layout.container_scrollbar, x, y) {
            return Some(ScrollbarTarget::Containers);
        }
        if self.detail_tab == DetailTab::Logs && contains(layout.logs_scrollbar, x, y) {
            return Some(ScrollbarTarget::Logs);
        }
        if self.detail_tab != DetailTab::Logs && contains(layout.detail_scrollbar, x, y) {
            return Some(ScrollbarTarget::Detail);
        }
        None
    }

    fn begin_scrollbar_drag(&mut self, target: ScrollbarTarget, y: u16) -> Vec<UiCommand> {
        self.logs.finish_drag();
        self.focus = match target {
            ScrollbarTarget::Containers => Focus::Containers,
            ScrollbarTarget::Detail | ScrollbarTarget::Logs => Focus::Detail,
        };

        let metrics = self.scroll_metrics(target);
        let (thumb_top, thumb_len) = metrics.thumb_bounds();
        let grab_offset = if y >= thumb_top && y < thumb_top.saturating_add(thumb_len) {
            y.saturating_sub(thumb_top)
        } else {
            thumb_len / 2
        };
        self.scrollbar_drag = Some(ScrollbarDrag {
            target,
            grab_offset,
        });
        self.apply_scrollbar_drag(target, y, grab_offset);
        Vec::new()
    }

    fn update_scrollbar_drag(&mut self, y: u16) {
        let Some(drag) = self.scrollbar_drag else {
            return;
        };
        self.apply_scrollbar_drag(drag.target, y, drag.grab_offset);
    }

    fn apply_scrollbar_drag(&mut self, target: ScrollbarTarget, y: u16, grab_offset: u16) {
        let metrics = self.scroll_metrics(target);
        let (_, thumb_len) = metrics.thumb_bounds();
        let max_thumb_top = metrics
            .track
            .y
            .saturating_add(metrics.track.height.saturating_sub(thumb_len));
        let desired_top = y
            .saturating_sub(grab_offset)
            .clamp(metrics.track.y, max_thumb_top);
        let position = metrics.position_for_thumb_top(desired_top, thumb_len);

        match target {
            ScrollbarTarget::Containers => self.set_container_offset(position),
            ScrollbarTarget::Detail => self.set_detail_offset(position),
            ScrollbarTarget::Logs => self.logs.set_scroll_top(position, self.log_rows_height()),
        }
    }

    fn scroll_metrics(&self, target: ScrollbarTarget) -> ScrollMetrics {
        match target {
            ScrollbarTarget::Containers => self.container_scroll_metrics(),
            ScrollbarTarget::Detail => self.detail_scroll_metrics(),
            ScrollbarTarget::Logs => self.log_scroll_metrics(),
        }
    }

    fn ensure_container_visible(&mut self) {
        let Some(selected) = self.selected_visible_index() else {
            self.container_offset = 0;
            return;
        };
        let height = self.container_rows_height();
        self.container_offset =
            scroll_offset_for_selection(self.container_offset, selected, height);
    }

    fn ensure_detail_visible(&mut self) {
        let selected = self.selected_detail_index();
        self.detail_offset =
            scroll_offset_for_selection(self.detail_offset, selected, self.detail_rows_height());
    }

    fn select_container_by_mouse_row(&mut self, row: u16) -> Vec<UiCommand> {
        let layout = self.layout();
        let relative = row.saturating_sub(layout.container_view.y);
        if relative == 0 {
            return Vec::new();
        }
        let index = self.container_offset + (relative as usize - 1);
        let visible = self.filtered_containers();
        let Some(container) = visible.get(index) else {
            return Vec::new();
        };
        if self.selected_id.as_deref() == Some(container.id.as_str()) {
            return Vec::new();
        }
        self.selected_id = Some(container.id.clone());
        self.details = None;
        self.logs.begin_loading(self.selected_id.clone());
        self.ensure_container_visible();
        self.selected_context_commands()
    }

    fn select_tab_by_mouse_x(&mut self, x: u16, inner: Rect) -> Vec<UiCommand> {
        for (tab, start, end) in detail_tab_regions(inner) {
            if x >= start && x < end {
                self.detail_tab = tab;
                if self.detail_tab == DetailTab::Logs {
                    self.logs.ensure_visible(self.log_rows_height());
                } else {
                    self.ensure_detail_visible();
                }
                break;
            }
        }
        Vec::new()
    }

    fn select_detail_by_mouse_row(&mut self, row: u16) {
        let layout = self.layout();
        let relative = row.saturating_sub(layout.detail_view.y) as usize;
        let items = self.selected_detail_items();
        if items.is_empty() {
            return;
        }
        let index = (self.detail_offset + relative).min(items.len().saturating_sub(1));
        self.detail_cursor.insert(self.detail_tab, index);
        self.ensure_detail_visible();
    }

    fn select_log_row_from_mouse(&mut self, row: u16) {
        let layout = self.layout();
        let relative = row.saturating_sub(layout.logs_view.y) as usize;
        if relative >= self.log_rows_height() {
            return;
        }
        let visible = self.visible_log_rows();
        if let Some(entry) = visible.get(relative) {
            self.logs
                .start_mouse_selection(entry.entry_index, self.log_rows_height());
        }
    }

    fn update_log_drag(&mut self, row: u16) {
        let layout = self.layout();
        let max_row = self.log_rows_height().saturating_sub(1);
        let relative = row.saturating_sub(layout.logs_view.y).min(max_row as u16) as usize;
        let visible = self.visible_log_rows();
        if let Some(entry) = visible.get(relative) {
            self.logs
                .update_mouse_selection(entry.entry_index, self.log_rows_height());
        }
    }

    fn set_container_offset(&mut self, position: usize) {
        let max_top = self
            .filtered_containers()
            .len()
            .saturating_sub(self.container_rows_height());
        self.container_offset = position.min(max_top);
    }

    fn set_detail_offset(&mut self, position: usize) {
        let max_top = self
            .selected_detail_items()
            .len()
            .saturating_sub(self.detail_rows_height());
        self.detail_offset = position.min(max_top);
    }
}

pub struct LogView {
    pub(crate) container_id: Option<String>,
    pub(crate) entries: Vec<LogEntry>,
    pub(crate) loading: bool,
    pub(crate) filter_query: String,
    pub(crate) search_query: String,
    pub(crate) filter_mode: LogFilterMode,
    pub(crate) follow: bool,
    pub(crate) wrap: bool,
    pub(crate) show_timestamps: bool,
    pub(crate) selected: usize,
    pub(crate) max_entries: usize,
    pub(crate) scroll_top: usize,
    selection_anchor: Option<usize>,
    selection_end: Option<usize>,
    pub(crate) drag_active: bool,
}

impl LogView {
    fn new(show_timestamps: bool, max_entries: usize) -> Self {
        Self {
            container_id: None,
            entries: Vec::new(),
            loading: false,
            filter_query: String::new(),
            search_query: String::new(),
            filter_mode: LogFilterMode::Substring,
            follow: true,
            wrap: false,
            show_timestamps,
            selected: 0,
            max_entries,
            scroll_top: 0,
            selection_anchor: None,
            selection_end: None,
            drag_active: false,
        }
    }

    pub fn clear(&mut self) {
        self.container_id = None;
        self.entries.clear();
        self.loading = false;
        self.selected = 0;
        self.scroll_top = 0;
        self.selection_anchor = None;
        self.selection_end = None;
        self.drag_active = false;
    }

    fn reset(&mut self, container_id: String) {
        self.clear();
        self.container_id = Some(container_id);
        self.loading = true;
        self.follow = true;
    }

    fn begin_loading(&mut self, container_id: Option<String>) {
        self.clear();
        self.container_id = container_id;
        self.loading = self.container_id.is_some();
        self.follow = true;
    }

    fn finish_loading(&mut self) {
        self.loading = false;
    }

    fn append(&mut self, entries: Vec<LogEntry>) {
        if entries.is_empty() {
            return;
        }

        self.entries.extend(entries);
        if self.entries.len() > self.max_entries {
            let drain = self.entries.len() - self.max_entries;
            self.entries.drain(0..drain);
        }

        let filtered_len = self.filtered_entries().len();
        if self.follow {
            self.selected = filtered_len.saturating_sub(1);
        }
        self.selected = self.selected.min(filtered_len.saturating_sub(1));
    }

    fn selected_entry(&self) -> Option<&LogEntry> {
        let filtered = self.filtered_entries();
        let index = self.selected.min(filtered.len().saturating_sub(1));
        filtered.get(index).copied()
    }

    fn filtered_len(&self) -> usize {
        self.filtered_entries().len()
    }

    pub fn filtered_entries(&self) -> Vec<&LogEntry> {
        let filter_query = self.filter_query.trim();
        let filter_regex =
            if matches!(self.filter_mode, LogFilterMode::Regex) && !filter_query.is_empty() {
                RegexCache::compile(filter_query).ok()
            } else {
                None
            };

        self.entries
            .iter()
            .filter(|entry| {
                if filter_query.is_empty() {
                    return true;
                }

                let haystack = entry.display(self.show_timestamps);
                match self.filter_mode {
                    LogFilterMode::Substring => haystack
                        .to_ascii_lowercase()
                        .contains(&filter_query.to_ascii_lowercase()),
                    LogFilterMode::Regex => filter_regex
                        .as_ref()
                        .map(|regex| regex.is_match(&haystack))
                        .unwrap_or(false),
                }
            })
            .collect()
    }

    pub(crate) fn regex_error(&self) -> Option<String> {
        if matches!(self.filter_mode, LogFilterMode::Regex) && !self.filter_query.trim().is_empty()
        {
            return RegexCache::compile(&self.filter_query)
                .err()
                .map(|error| error.to_string());
        }
        None
    }

    fn move_selection(&mut self, delta: isize) {
        let filtered_len = self.filtered_entries().len();
        if filtered_len == 0 {
            self.selected = 0;
            return;
        }

        let max = filtered_len.saturating_sub(1) as isize;
        self.selected = ((self.selected as isize) + delta).clamp(0, max) as usize;
        self.follow = self.selected == max as usize;
        self.clear_selection();
    }

    fn jump_to_edge(&mut self, end: bool) {
        let filtered_len = self.filtered_entries().len();
        self.selected = if end {
            filtered_len.saturating_sub(1)
        } else {
            0
        };
        self.follow = end;
        self.clear_selection();
    }

    fn jump_to_match(&mut self, reverse: bool) {
        let query = self.search_query.trim();
        if query.is_empty() {
            return;
        }

        let filtered = self.filtered_entries();
        if filtered.is_empty() {
            return;
        }

        let search = query.to_ascii_lowercase();
        let next = if reverse {
            (0..=self.selected.min(filtered.len() - 1))
                .rev()
                .skip(1)
                .chain((0..filtered.len()).rev())
                .find(|index| {
                    filtered[*index]
                        .display(self.show_timestamps)
                        .to_ascii_lowercase()
                        .contains(&search)
                })
        } else {
            (self.selected.min(filtered.len() - 1)..filtered.len())
                .skip(1)
                .chain(0..filtered.len())
                .find(|index| {
                    filtered[*index]
                        .display(self.show_timestamps)
                        .to_ascii_lowercase()
                        .contains(&search)
                })
        };

        if let Some(index) = next {
            self.selected = index;
            self.follow = false;
            self.clear_selection();
        }
    }

    fn toggle_follow(&mut self) {
        self.follow = !self.follow;
        if self.follow {
            self.selected = self.filtered_entries().len().saturating_sub(1);
        }
        self.clear_selection();
    }

    fn toggle_filter_mode(&mut self) {
        self.filter_mode = match self.filter_mode {
            LogFilterMode::Substring => LogFilterMode::Regex,
            LogFilterMode::Regex => LogFilterMode::Substring,
        };
        let len = self.filtered_entries().len();
        self.selected = self.selected.min(len.saturating_sub(1));
        self.clear_selection();
    }

    fn set_filter_query(&mut self, value: String) {
        self.filter_query = value;
        let len = self.filtered_entries().len();
        self.selected = len.saturating_sub(1);
        self.scroll_top = self.selected;
        self.clear_selection();
    }

    fn set_search_query(&mut self, value: String) {
        self.search_query = value;
        self.jump_to_match(false);
    }

    fn ensure_visible(&mut self, height: usize) {
        let filtered_len = self.filtered_entries().len();
        if filtered_len == 0 {
            self.scroll_top = 0;
            self.selected = 0;
            return;
        }
        self.selected = self.selected.min(filtered_len.saturating_sub(1));
        let height = height.max(1);
        if self.follow {
            self.scroll_top = filtered_len.saturating_sub(height);
            self.selected = filtered_len.saturating_sub(1);
            return;
        }
        self.scroll_top = scroll_offset_for_selection(self.scroll_top, self.selected, height);
    }

    pub(crate) fn visible_rows(&self, height: usize, width: usize) -> Vec<VisibleLogRow> {
        if height == 0 {
            return Vec::new();
        }

        let filtered = self.filtered_entries();
        let mut rows = Vec::new();
        let width = width.max(1);

        for (entry_index, entry) in filtered.iter().enumerate().skip(self.scroll_top) {
            let rendered = entry.display(self.show_timestamps);
            if self.wrap {
                for segment in textwrap::wrap(&rendered, width) {
                    rows.push(VisibleLogRow {
                        entry_index,
                        text: segment.into_owned(),
                    });
                    if rows.len() >= height {
                        return rows;
                    }
                }
            } else {
                rows.push(VisibleLogRow {
                    entry_index,
                    text: rendered,
                });
                if rows.len() >= height {
                    return rows;
                }
            }
        }

        rows
    }

    pub(crate) fn selected_range(&self) -> Option<(usize, usize)> {
        match (self.selection_anchor, self.selection_end) {
            (Some(start), Some(end)) => Some((start.min(end), start.max(end))),
            _ => None,
        }
    }

    pub(crate) fn selected_text(&self) -> Option<String> {
        let filtered = self.filtered_entries();
        if filtered.is_empty() {
            return None;
        }

        if let Some((start, end)) = self.selected_range() {
            return Some(
                filtered[start.min(filtered.len() - 1)..=end.min(filtered.len() - 1)]
                    .iter()
                    .map(|entry| entry.display(self.show_timestamps))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }

        self.selected_entry()
            .map(|entry| entry.display(self.show_timestamps))
    }

    fn start_mouse_selection(&mut self, entry_index: usize, height: usize) {
        self.follow = false;
        self.selected = entry_index;
        self.selection_anchor = Some(entry_index);
        self.selection_end = Some(entry_index);
        self.drag_active = true;
        self.ensure_visible(height);
    }

    fn update_mouse_selection(&mut self, entry_index: usize, height: usize) {
        if !self.drag_active {
            return;
        }
        self.selected = entry_index;
        self.selection_end = Some(entry_index);
        self.ensure_visible(height);
    }

    fn finish_drag(&mut self) {
        self.drag_active = false;
    }

    fn set_scroll_top(&mut self, position: usize, height: usize) {
        let filtered_len = self.filtered_entries().len();
        if filtered_len == 0 {
            self.scroll_top = 0;
            self.selected = 0;
            return;
        }

        let height = height.max(1);
        let max_top = filtered_len.saturating_sub(height);
        self.follow = false;
        self.scroll_top = position.min(max_top);

        if self.selected < self.scroll_top {
            self.selected = self.scroll_top;
            self.clear_selection();
        } else if self.selected >= self.scroll_top.saturating_add(height) {
            self.selected = (self.scroll_top + height)
                .saturating_sub(1)
                .min(filtered_len - 1);
            self.clear_selection();
        }
    }

    fn clear_selection(&mut self) {
        self.selection_anchor = None;
        self.selection_end = None;
        self.drag_active = false;
    }
}

struct RegexCache;

impl RegexCache {
    fn compile(value: &str) -> Result<regex::Regex, regex::Error> {
        regex::Regex::new(value)
    }
}

#[derive(Clone, Copy)]
struct AppLayout {
    container_outer: Rect,
    container_view: Rect,
    container_scrollbar: Rect,
    detail_outer: Rect,
    detail_view: Rect,
    detail_scrollbar: Rect,
    detail_tabs_inner: Rect,
    logs_outer: Rect,
    logs_view: Rect,
    logs_scrollbar: Rect,
}

impl AppLayout {
    fn new(container_outer: Rect, detail_tabs_outer: Rect, detail_outer: Rect) -> Self {
        let container_inner = block_inner(container_outer);
        let detail_inner = block_inner(detail_outer);
        let (container_view, container_scrollbar) = split_right_gutter(container_inner, 1);
        let (detail_view, detail_scrollbar) = split_right_gutter(detail_inner, 1);
        let logs_region = trim_bottom(detail_inner, 1);
        let (logs_view, logs_scrollbar) = split_right_gutter(logs_region, 1);

        Self {
            container_outer,
            container_view,
            container_scrollbar: trim_top(container_scrollbar, 1),
            detail_outer,
            detail_view,
            detail_scrollbar,
            detail_tabs_inner: block_inner(detail_tabs_outer),
            logs_outer: detail_outer,
            logs_view,
            logs_scrollbar,
        }
    }
}

#[derive(Clone, Copy)]
struct ScrollMetrics {
    content_length: usize,
    viewport_length: usize,
    position: usize,
    track: Rect,
}

impl ScrollMetrics {
    fn new(content_length: usize, viewport_length: usize, position: usize, track: Rect) -> Self {
        Self {
            content_length,
            viewport_length: viewport_length.max(1),
            position,
            track,
        }
    }

    fn thumb_bounds(self) -> (u16, u16) {
        if self.track.height == 0 || self.content_length == 0 {
            return (self.track.y, self.track.height.min(1));
        }

        if self.content_length <= self.viewport_length {
            return (self.track.y, self.track.height);
        }

        let track_length = self.track.height;
        let thumb_length = (((self.viewport_length as f64 / self.content_length as f64)
            * track_length as f64)
            .round() as u16)
            .clamp(1, track_length);
        let max_position = self.content_length.saturating_sub(self.viewport_length);
        let travel = track_length.saturating_sub(thumb_length);
        let thumb_offset = if max_position == 0 || travel == 0 {
            0
        } else {
            ((self.position.min(max_position) as f64 / max_position as f64) * travel as f64).round()
                as u16
        };

        (self.track.y.saturating_add(thumb_offset), thumb_length)
    }

    fn position_for_thumb_top(self, thumb_top: u16, thumb_length: u16) -> usize {
        let max_position = self.content_length.saturating_sub(self.viewport_length);
        let travel = self.track.height.saturating_sub(thumb_length);
        if max_position == 0 || travel == 0 {
            return 0;
        }

        let relative_top = thumb_top.saturating_sub(self.track.y).min(travel);
        ((relative_top as f64 / travel as f64) * max_position as f64).round() as usize
    }
}

fn block_inner(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

fn split_right_gutter(area: Rect, gutter_width: u16) -> (Rect, Rect) {
    let gutter_width = gutter_width.min(area.width);
    let view_width = area.width.saturating_sub(gutter_width);

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
            width: gutter_width,
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

fn detail_tab_regions(inner: Rect) -> Vec<(DetailTab, u16, u16)> {
    if inner.width == 0 {
        return Vec::new();
    }

    let mut regions = Vec::with_capacity(DetailTab::ALL.len());
    let mut x = inner.x;
    let right = inner.x.saturating_add(inner.width);

    for (index, tab) in DetailTab::ALL.iter().copied().enumerate() {
        let start = x;
        let title_width = format!(" {} ", tab.title()).len() as u16;
        x = x.saturating_add(1);
        x = x.saturating_add(title_width);
        x = x.saturating_add(1);
        regions.push((tab, start, x.min(right)));

        if index + 1 != DetailTab::ALL.len() {
            x = x.saturating_add(1);
        }

        if x >= right {
            break;
        }
    }

    regions
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && y >= rect.y
        && x < rect.x.saturating_add(rect.width)
        && y < rect.y.saturating_add(rect.height)
}

fn scroll_offset_for_selection(current: usize, selected: usize, height: usize) -> usize {
    let height = height.max(1);
    if selected < current {
        selected
    } else if selected >= current + height {
        selected + 1 - height
    } else {
        current
    }
}

fn matches_binding(
    key: KeyEvent,
    configured: Option<&str>,
    default: &str,
    extra: Option<(&str, KeyModifiers)>,
) -> bool {
    let rendered = render_key_event(key);
    rendered.eq_ignore_ascii_case(configured.unwrap_or(default))
        || extra
            .map(|(code, modifiers)| {
                key.code == KeyCode::Char(code.chars().next().unwrap_or_default())
                    && key.modifiers.contains(modifiers)
            })
            .unwrap_or(false)
}

fn display_binding(configured: Option<&str>, default: &str) -> String {
    configured.unwrap_or(default).to_string()
}

fn render_key_event(key: KeyEvent) -> String {
    let mut parts = Vec::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".to_string());
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt".to_string());
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("shift".to_string());
    }

    let code = match key.code {
        KeyCode::Char(character)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            character.to_string()
        }
        KeyCode::Char(character) => character.to_ascii_lowercase().to_string(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "backtab".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Backspace => "backspace".into(),
        _ => return String::new(),
    };
    parts.push(code);
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{KeymapConfig, RuntimeConfig, ThemeName};

    fn test_runtime() -> RuntimeConfig {
        RuntimeConfig {
            theme: ThemeName::Graphite,
            show_stopped_by_default: false,
            log_backlog_lines: 400,
            show_timestamps: true,
            keymap: KeymapConfig::default(),
            docker_host: None,
            project_filter: None,
            startup_container_query: None,
        }
    }

    fn sample_container(index: usize) -> ContainerRecord {
        ContainerRecord {
            id: format!("container-{index}"),
            short_id: format!("c{index:02}"),
            name: format!("service-{index}"),
            image: "demo:latest".into(),
            command: "sleep infinity".into(),
            state: "running".into(),
            status: "Up".into(),
            project: Some("demo".into()),
            service: Some(format!("svc-{index}")),
            ports: Vec::new(),
            health: None,
            created: Some(index as i64),
        }
    }

    #[test]
    fn log_view_substring_filter_works() {
        let mut logs = LogView::new(true, 100);
        logs.append(vec![
            LogEntry::parse("stdout", "server listening\n"),
            LogEntry::parse("stdout", "database ready\n"),
        ]);
        logs.set_filter_query("server".into());
        assert_eq!(logs.filtered_entries().len(), 1);
        assert_eq!(
            logs.filtered_entries()[0].display(logs.show_timestamps),
            "server listening"
        );
    }

    #[test]
    fn log_view_regex_filter_works() {
        let mut logs = LogView::new(true, 100);
        logs.append(vec![
            LogEntry::parse("stdout", "GET /health 200\n"),
            LogEntry::parse("stdout", "GET /ready 500\n"),
        ]);
        logs.toggle_filter_mode();
        logs.set_filter_query(r"5\d\d$".into());
        assert_eq!(logs.filtered_entries().len(), 1);
    }

    #[test]
    fn log_view_mouse_selection_copies_range() {
        let mut logs = LogView::new(true, 100);
        logs.append(vec![
            LogEntry::parse("stdout", "first\n"),
            LogEntry::parse("stdout", "second\n"),
            LogEntry::parse("stdout", "third\n"),
        ]);
        logs.start_mouse_selection(0, 10);
        logs.update_mouse_selection(2, 10);
        assert_eq!(
            logs.selected_text().as_deref(),
            Some("first\nsecond\nthird")
        );
    }

    #[test]
    fn log_view_visible_rows_are_virtualized() {
        let mut logs = LogView::new(true, 100);
        logs.append(vec![
            LogEntry::parse("stdout", "alpha\n"),
            LogEntry::parse("stdout", "beta\n"),
            LogEntry::parse("stdout", "gamma\n"),
        ]);
        logs.follow = false;
        logs.selected = 2;
        logs.ensure_visible(2);
        let rows = logs.visible_rows(2, 40);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "beta");
        assert_eq!(rows[1].text, "gamma");
    }

    #[test]
    fn set_viewport_keeps_manual_scroll_when_size_is_unchanged() {
        let mut app = App::new(test_runtime(), None);
        app.containers = (0..20).map(sample_container).collect();
        app.selected_id = Some(app.containers[0].id.clone());

        let viewport = Rect::new(0, 0, 120, 40);
        app.set_viewport(viewport);
        app.container_offset = 7;
        app.set_viewport(viewport);

        assert_eq!(app.container_offset, 7);
    }
}
