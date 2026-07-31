//! Model and Update: application state plus the message-driven state machine.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;
use tokio::sync::mpsc::UnboundedSender;

use crate::docker::DockerCommand;

/// Cap on retained log lines so long-running streams stay bounded.
const MAX_LOG_LINES: usize = 2000;

/// How many lines one PgUp/PgDn press moves.
const SCROLL_STEP: usize = 10;

/// How long transient status messages stay in the header.
const STATUS_TTL: Duration = Duration::from_secs(6);
const ERROR_STATUS_TTL: Duration = Duration::from_secs(12);

/// Compose metadata read from a container's labels. Present only when the
/// container is managed by Docker Compose.
#[derive(Debug, Clone)]
pub struct ComposeInfo {
    pub project: String,
    pub service: String,
    pub working_dir: Option<String>,
    pub config_files: Vec<String>,
}

/// A snapshot of one container, derived from the Docker list endpoint.
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub ports: String,
    pub compose: Option<ComposeInfo>,
}

impl ContainerInfo {
    pub fn is_running(&self) -> bool {
        self.state == "running"
    }

    pub fn local_urls(&self) -> Vec<String> {
        let mut urls = Vec::new();
        for part in self.ports.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if let Some((public_part, _rest)) = part.split_once("->") {
                let public_port = public_part.trim();
                if !public_port.is_empty() && public_port.chars().all(|c| c.is_ascii_digit()) {
                    let proto = if part.contains("/tcp") { "tcp" } else { "udp" };
                    if proto == "tcp" {
                        let scheme = if public_port == "443" {
                            "https"
                        } else {
                            "http"
                        };
                        urls.push(format!("{}://localhost:{}", scheme, public_port));
                    }
                }
            }
        }
        urls.sort();
        urls.dedup();
        urls
    }
}

/// Live resource usage for the selected container.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatsInfo {
    pub cpu_percent: f64,
    pub mem_usage: u64,
    pub mem_limit: u64,
    pub net_rx: u64,
    pub net_tx: u64,
}

/// What the right pane is currently showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Details,
    Logs,
    Update,
}

/// Transient message rendered in the header bar.
#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub text: String,
    pub is_error: bool,
}

impl StatusMessage {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

/// Every event the update loop can react to.
pub enum Message {
    Key(KeyEvent),
    ContainersUpdated(Vec<ContainerInfo>),
    StatsUpdated(String, StatsInfo),
    LogLine(String, String),
    UpdateLine(String),
    UpdateFinished { success: bool, detail: String },
    Status(String),
    DockerError(String),
}

/// The Model: all state needed to render a frame.
pub struct App {
    pub containers: Vec<ContainerInfo>,
    pub list_state: ListState,
    pub view: View,
    pub logs: Vec<String>,
    pub stats: Option<StatsInfo>,
    pub status: Option<StatusMessage>,
    status_at: Option<Instant>,
    /// Lines scrolled back from the tail of the logs or update output.
    /// Zero means follow mode: newest lines pinned to the bottom.
    pub scroll_back: usize,
    pub update_output: Vec<String>,
    pub update_running: bool,
    pub update_target: Option<String>,
    pub should_quit: bool,
    cmd_tx: UnboundedSender<DockerCommand>,
}

impl App {
    pub fn new(cmd_tx: UnboundedSender<DockerCommand>) -> Self {
        Self {
            containers: Vec::new(),
            list_state: ListState::default(),
            view: View::Details,
            logs: Vec::new(),
            stats: None,
            status: None,
            status_at: None,
            scroll_back: 0,
            update_output: Vec::new(),
            update_running: false,
            update_target: None,
            should_quit: false,
            cmd_tx,
        }
    }

    pub fn selected(&self) -> Option<&ContainerInfo> {
        self.list_state
            .selected()
            .and_then(|i| self.containers.get(i))
    }

    /// The Update function: fold one message into the model.
    pub fn update(&mut self, msg: Message) {
        match msg {
            Message::Key(key) => self.handle_key(key),
            Message::ContainersUpdated(list) => self.apply_container_list(list),
            Message::StatsUpdated(id, stats) => {
                if self.selected().is_some_and(|c| c.id == id) {
                    self.stats = Some(stats);
                }
            }
            Message::LogLine(id, line) => {
                if self.view == View::Logs && self.selected().is_some_and(|c| c.id == id) {
                    self.logs.push(line);
                    if self.logs.len() > MAX_LOG_LINES {
                        let excess = self.logs.len() - MAX_LOG_LINES;
                        self.logs.drain(..excess);
                    }
                }
            }
            Message::UpdateLine(line) => {
                self.update_output.push(line);
                if self.update_output.len() > MAX_LOG_LINES {
                    let excess = self.update_output.len() - MAX_LOG_LINES;
                    self.update_output.drain(..excess);
                }
            }
            Message::UpdateFinished { success, detail } => {
                self.update_running = false;
                self.set_status(if success {
                    StatusMessage::info(detail)
                } else {
                    StatusMessage::error(detail)
                });
            }
            Message::Status(text) => self.set_status(StatusMessage::info(text)),
            Message::DockerError(text) => self.set_status(StatusMessage::error(text)),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Enter => self.toggle_view(),
            KeyCode::Esc => match self.view {
                View::Logs => self.leave_logs(),
                View::Update => {
                    // The update task keeps running in the background;
                    // its output stays buffered for when the user returns.
                    self.view = View::Details;
                    self.scroll_back = 0;
                    self.request_stats();
                }
                View::Details => {}
            },
            KeyCode::Char('s') => self.toggle_start_stop(),
            KeyCode::Char('r') => self.restart_selected(),
            KeyCode::Char('u') => self.trigger_update(),
            KeyCode::PageUp => self.scroll_up(SCROLL_STEP),
            KeyCode::PageDown => self.scroll_down(SCROLL_STEP),
            KeyCode::Home => self.scroll_up(usize::MAX / 2),
            KeyCode::End => self.scroll_back = 0,
            _ => {}
        }
    }

    /// Scroll back through logs or update output. The view clamps the
    /// offset to the buffer size when it renders.
    fn scroll_up(&mut self, lines: usize) {
        if self.view != View::Details {
            self.scroll_back = self.scroll_back.saturating_add(lines);
        }
    }

    fn scroll_down(&mut self, lines: usize) {
        self.scroll_back = self.scroll_back.saturating_sub(lines);
    }

    fn set_status(&mut self, status: StatusMessage) {
        self.status_at = Some(Instant::now());
        self.status = Some(status);
    }

    /// Drop stale status messages; called on the steady polling cadence.
    fn expire_status(&mut self) {
        if let (Some(status), Some(at)) = (&self.status, self.status_at) {
            let ttl = if status.is_error {
                ERROR_STATUS_TTL
            } else {
                STATUS_TTL
            };
            if at.elapsed() > ttl {
                self.status = None;
                self.status_at = None;
            }
        }
    }

    fn apply_container_list(&mut self, list: Vec<ContainerInfo>) {
        self.expire_status();
        let previous_id = self.selected().map(|c| c.id.clone());
        self.containers = list;

        // Keep the same container selected across refreshes when possible,
        // otherwise clamp the index into the new list.
        let index = previous_id
            .and_then(|id| self.containers.iter().position(|c| c.id == id))
            .or_else(|| {
                if self.containers.is_empty() {
                    None
                } else {
                    let current = self.list_state.selected().unwrap_or(0);
                    Some(current.min(self.containers.len() - 1))
                }
            });
        self.list_state.select(index);

        // Piggyback a stats refresh on the polling cadence.
        self.request_stats();
    }

    fn move_selection(&mut self, delta: i64) {
        if self.containers.is_empty() {
            return;
        }
        let len = self.containers.len() as i64;
        let current = self.list_state.selected().unwrap_or(0) as i64;
        let next = (current + delta).clamp(0, len - 1) as usize;
        if Some(next) == self.list_state.selected() {
            return;
        }

        self.list_state.select(Some(next));
        self.stats = None;
        if self.view == View::Logs {
            // Follow the new selection with a fresh log stream.
            self.logs.clear();
            self.scroll_back = 0;
            if let Some(c) = self.selected() {
                let id = c.id.clone();
                self.send(DockerCommand::StreamLogs { id });
            }
        } else {
            self.request_stats();
        }
    }

    fn toggle_view(&mut self) {
        let Some(c) = self.selected() else { return };
        let id = c.id.clone();
        let name = c.name.clone();
        match self.view {
            View::Details | View::Update => {
                self.view = View::Logs;
                self.logs.clear();
                self.scroll_back = 0;
                self.set_status(StatusMessage::info(format!("Streaming logs for {name}")));
                self.send(DockerCommand::StreamLogs { id });
            }
            View::Logs => self.leave_logs(),
        }
    }

    fn leave_logs(&mut self) {
        self.view = View::Details;
        self.stats = None;
        self.status = None;
        self.scroll_back = 0;
        self.send(DockerCommand::StopLogs);
        self.request_stats();
    }

    fn toggle_start_stop(&mut self) {
        let Some(c) = self.selected() else { return };
        let id = c.id.clone();
        let name = c.name.clone();
        if c.is_running() {
            self.set_status(StatusMessage::info(format!("Stopping {name}...")));
            self.send(DockerCommand::Stop { id, name });
        } else {
            self.set_status(StatusMessage::info(format!("Starting {name}...")));
            self.send(DockerCommand::Start { id, name });
        }
    }

    fn restart_selected(&mut self) {
        let Some(c) = self.selected() else { return };
        let id = c.id.clone();
        let name = c.name.clone();
        self.set_status(StatusMessage::info(format!("Restarting {name}...")));
        self.send(DockerCommand::Restart { id, name });
    }

    fn trigger_update(&mut self) {
        let Some(c) = self.selected() else { return };

        // One update at a time: re-pressing u jumps back to the output view.
        if self.update_running {
            self.view = View::Update;
            self.set_status(StatusMessage::info("An update is already in progress"));
            return;
        }

        match &c.compose {
            Some(compose) => {
                let compose = compose.clone();
                let name = c.name.clone();
                self.update_output.clear();
                self.update_target = Some(format!("{}/{}", compose.project, compose.service));
                self.update_running = true;
                self.view = View::Update;
                self.scroll_back = 0;
                self.set_status(StatusMessage::info(format!(
                    "Compose Update triggered for {name}"
                )));
                self.send(DockerCommand::ComposeUpdate { compose, name });
            }
            None => {
                self.set_status(StatusMessage::error(format!(
                    "{} is not managed by Compose: update unavailable",
                    c.name
                )));
            }
        }
    }

    /// Ask the Docker actor for fresh stats when they would be visible.
    fn request_stats(&mut self) {
        if self.view != View::Details {
            return;
        }
        if let Some(c) = self.selected() {
            if c.is_running() {
                let id = c.id.clone();
                self.send(DockerCommand::FetchStats { id });
            }
        }
    }

    fn send(&self, cmd: DockerCommand) {
        // A closed channel means the actor is gone; the app is shutting down
        let _ = self.cmd_tx.send(cmd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn make_container(id: &str, name: &str, running: bool) -> ContainerInfo {
        ContainerInfo {
            id: id.to_string(),
            name: name.to_string(),
            image: "nginx:latest".to_string(),
            state: if running {
                "running".to_string()
            } else {
                "stopped".to_string()
            },
            status: "Up 2 hours".to_string(),
            ports: "80/tcp".to_string(),
            compose: None,
        }
    }

    #[test]
    fn test_app_initial_state() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let app = App::new(tx);
        assert!(app.containers.is_empty());
        assert_eq!(app.list_state.selected(), None);
        assert_eq!(app.view, View::Details);
        assert!(app.logs.is_empty());
        assert!(app.stats.is_none());
        assert!(app.status.is_none());
        assert_eq!(app.scroll_back, 0);
        assert!(app.update_output.is_empty());
        assert!(!app.update_running);
        assert!(app.update_target.is_none());
        assert!(!app.should_quit);
    }

    #[test]
    fn test_app_quit_keys() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let mut app = App::new(tx.clone());
        let q_key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty());
        app.update(Message::Key(q_key));
        assert!(app.should_quit);

        let mut app = App::new(tx);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.update(Message::Key(ctrl_c));
        assert!(app.should_quit);
    }

    #[test]
    fn test_app_navigation_and_selection() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(tx);

        let down_key = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty());
        app.update(Message::Key(down_key));
        assert_eq!(app.list_state.selected(), None);

        let containers = vec![
            make_container("1", "web1", true),
            make_container("2", "web2", false),
            make_container("3", "web3", true),
        ];
        app.update(Message::ContainersUpdated(containers));

        assert_eq!(app.list_state.selected(), Some(0));
        assert_eq!(app.selected().unwrap().id, "1");

        while rx.try_recv().is_ok() {}

        let down_key = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty());
        app.update(Message::Key(down_key));
        assert_eq!(app.list_state.selected(), Some(1));
        assert_eq!(app.selected().unwrap().id, "2");
        assert!(rx.try_recv().is_err());

        app.update(Message::Key(down_key));
        assert_eq!(app.list_state.selected(), Some(2));
        assert_eq!(app.selected().unwrap().id, "3");
        if let Ok(DockerCommand::FetchStats { id }) = rx.try_recv() {
            assert_eq!(id, "3");
        } else {
            panic!("Expected FetchStats for container 3");
        }

        app.update(Message::Key(down_key));
        assert_eq!(app.list_state.selected(), Some(2));

        let up_key = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::empty());
        app.update(Message::Key(up_key));
        assert_eq!(app.list_state.selected(), Some(1));

        app.update(Message::Key(up_key));
        assert_eq!(app.list_state.selected(), Some(0));
        if let Ok(DockerCommand::FetchStats { id }) = rx.try_recv() {
            assert_eq!(id, "1");
        } else {
            panic!("Expected FetchStats for container 1");
        }

        app.update(Message::Key(up_key));
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn test_app_container_list_update() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(tx);

        let containers = vec![
            make_container("1", "web1", true),
            make_container("2", "web2", false),
        ];
        app.update(Message::ContainersUpdated(containers));

        app.list_state.select(Some(1));

        let updated_containers = vec![
            make_container("3", "web3", true),
            make_container("2", "web2", false),
            make_container("1", "web1", true),
        ];
        app.update(Message::ContainersUpdated(updated_containers));
        assert_eq!(app.list_state.selected(), Some(1));
        assert_eq!(app.selected().unwrap().id, "2");

        let updated_containers_2 = vec![make_container("3", "web3", true)];
        app.update(Message::ContainersUpdated(updated_containers_2));
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn test_app_status_expiration() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(tx);

        app.update(Message::Status("Hello info".to_string()));
        assert!(app.status.is_some());
        assert_eq!(app.status.as_ref().unwrap().text, "Hello info");
        assert!(!app.status.as_ref().unwrap().is_error);

        app.expire_status();
        assert!(app.status.is_some());

        app.status_at = Some(Instant::now() - Duration::from_secs(7));
        app.expire_status();
        assert!(app.status.is_none());

        app.update(Message::DockerError("Some error".to_string()));
        assert!(app.status.is_some());
        assert!(app.status.as_ref().unwrap().is_error);

        app.status_at = Some(Instant::now() - Duration::from_secs(7));
        app.expire_status();
        assert!(app.status.is_some());

        app.status_at = Some(Instant::now() - Duration::from_secs(13));
        app.expire_status();
        assert!(app.status.is_none());
    }

    #[test]
    fn test_app_toggle_view_logs() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(tx);

        let enter_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
        app.update(Message::Key(enter_key));
        assert_eq!(app.view, View::Details);

        let containers = vec![make_container("1", "web1", true)];
        app.update(Message::ContainersUpdated(containers));
        while rx.try_recv().is_ok() {}

        app.update(Message::Key(enter_key));
        assert_eq!(app.view, View::Logs);
        if let Ok(DockerCommand::StreamLogs { id }) = rx.try_recv() {
            assert_eq!(id, "1");
        } else {
            panic!("Expected StreamLogs");
        }

        app.update(Message::Key(enter_key));
        assert_eq!(app.view, View::Details);
        if let Ok(DockerCommand::StopLogs) = rx.try_recv() {
        } else {
            panic!("Expected StopLogs");
        }

        app.update(Message::Key(enter_key));
        assert_eq!(app.view, View::Logs);
        while rx.try_recv().is_ok() {}

        let esc_key = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
        app.update(Message::Key(esc_key));
        assert_eq!(app.view, View::Details);
        if let Ok(DockerCommand::StopLogs) = rx.try_recv() {
        } else {
            panic!("Expected StopLogs");
        }
    }

    #[test]
    fn test_app_start_stop_restart() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(tx);

        let containers = vec![
            make_container("1", "web1", true),
            make_container("2", "web2", false),
        ];
        app.update(Message::ContainersUpdated(containers));
        while rx.try_recv().is_ok() {}

        let s_key = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::empty());
        app.update(Message::Key(s_key));
        if let Ok(DockerCommand::Stop { id, name }) = rx.try_recv() {
            assert_eq!(id, "1");
            assert_eq!(name, "web1");
        } else {
            panic!("Expected DockerCommand::Stop");
        }

        let r_key = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::empty());
        app.update(Message::Key(r_key));
        if let Ok(DockerCommand::Restart { id, name }) = rx.try_recv() {
            assert_eq!(id, "1");
            assert_eq!(name, "web1");
        } else {
            panic!("Expected DockerCommand::Restart");
        }

        app.list_state.select(Some(1));
        while rx.try_recv().is_ok() {}

        app.update(Message::Key(s_key));
        if let Ok(DockerCommand::Start { id, name }) = rx.try_recv() {
            assert_eq!(id, "2");
            assert_eq!(name, "web2");
        } else {
            panic!("Expected DockerCommand::Start");
        }
    }

    #[test]
    fn test_app_compose_update() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(tx);

        let compose = ComposeInfo {
            project: "proj".to_string(),
            service: "svc".to_string(),
            working_dir: None,
            config_files: vec!["compose.yaml".to_string()],
        };

        let containers = vec![
            ContainerInfo {
                id: "1".to_string(),
                name: "web1".to_string(),
                image: "nginx".to_string(),
                state: "running".to_string(),
                status: "Up".to_string(),
                ports: "".to_string(),
                compose: Some(compose.clone()),
            },
            make_container("2", "web2", true),
        ];

        app.update(Message::ContainersUpdated(containers));
        while rx.try_recv().is_ok() {}

        let u_key = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::empty());
        app.update(Message::Key(u_key));
        assert_eq!(app.view, View::Update);
        assert!(app.update_running);
        assert_eq!(app.update_target, Some("proj/svc".to_string()));
        if let Ok(DockerCommand::ComposeUpdate {
            compose: c_info,
            name,
        }) = rx.try_recv()
        {
            assert_eq!(c_info.project, "proj");
            assert_eq!(name, "web1");
        } else {
            panic!("Expected ComposeUpdate command");
        }

        app.update(Message::UpdateLine("pulling...".to_string()));
        assert_eq!(app.update_output, vec!["pulling...".to_string()]);

        app.update(Message::UpdateFinished {
            success: true,
            detail: "Updated successfully".to_string(),
        });
        assert!(!app.update_running);
        assert_eq!(app.status.as_ref().unwrap().text, "Updated successfully");

        let esc_key = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
        app.update(Message::Key(esc_key));
        assert_eq!(app.view, View::Details);

        app.list_state.select(Some(1));
        while rx.try_recv().is_ok() {}
        app.update(Message::Key(u_key));
        assert_eq!(app.view, View::Details);
        assert!(app.status.as_ref().unwrap().is_error);
    }

    #[test]
    fn test_app_scrolling() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(tx);

        app.view = View::Logs;

        let pgup = KeyEvent::new(KeyCode::PageUp, KeyModifiers::empty());
        let pgdn = KeyEvent::new(KeyCode::PageDown, KeyModifiers::empty());
        let home = KeyEvent::new(KeyCode::Home, KeyModifiers::empty());
        let end = KeyEvent::new(KeyCode::End, KeyModifiers::empty());

        app.update(Message::Key(pgup));
        assert_eq!(app.scroll_back, SCROLL_STEP);

        app.update(Message::Key(pgup));
        assert_eq!(app.scroll_back, SCROLL_STEP * 2);

        app.update(Message::Key(pgdn));
        assert_eq!(app.scroll_back, SCROLL_STEP);

        app.update(Message::Key(home));
        assert!(app.scroll_back > 1000);

        app.update(Message::Key(end));
        assert_eq!(app.scroll_back, 0);
    }

    #[test]
    fn test_container_local_urls() {
        let mut c = ContainerInfo {
            id: "1".to_string(),
            name: "web".to_string(),
            image: "nginx".to_string(),
            state: "running".to_string(),
            status: "Up".to_string(),
            ports: "".to_string(),
            compose: None,
        };

        assert!(c.local_urls().is_empty());

        c.ports = "8080->80/tcp".to_string();
        assert_eq!(c.local_urls(), vec!["http://localhost:8080".to_string()]);

        c.ports = "8080->80/udp".to_string();
        assert!(c.local_urls().is_empty());

        c.ports = "443->443/tcp".to_string();
        assert_eq!(c.local_urls(), vec!["https://localhost:443".to_string()]);

        c.ports = "9000->9000/tcp, 53->53/udp, 80->80/tcp".to_string();
        assert_eq!(
            c.local_urls(),
            vec![
                "http://localhost:80".to_string(),
                "http://localhost:9000".to_string()
            ]
        );
    }
}
