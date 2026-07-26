//! Side effects: a background actor that owns the bollard client.
//!
//! The actor receives `DockerCommand`s from the update loop and reports
//! results back as `Message`s. Long-running work (start/stop/restart,
//! stats sampling, log streaming) is spawned onto separate tasks so the
//! command loop never blocks.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use bollard::container::{
    ListContainersOptions, LogsOptions, RestartContainerOptions, StartContainerOptions, Stats,
    StatsOptions, StopContainerOptions,
};
use bollard::models::{ContainerSummary, Port};
use bollard::Docker;
use futures_util::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::app::{ComposeInfo, ContainerInfo, Message, StatsInfo};

/// How often the container list is refreshed.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How many historical lines to request when a log stream starts.
const LOG_TAIL: &str = "200";

/// Commands the update loop can send to the Docker actor.
pub enum DockerCommand {
    Start { id: String, name: String },
    Stop { id: String, name: String },
    Restart { id: String, name: String },
    FetchStats { id: String },
    StreamLogs { id: String },
    StopLogs,
    ComposeUpdate { compose: ComposeInfo, name: String },
}

#[derive(Clone, Copy)]
enum ContainerAction {
    Start,
    Stop,
    Restart,
}

/// Spawn the Docker actor and its polling task.
pub fn spawn(msg_tx: UnboundedSender<Message>, mut cmd_rx: UnboundedReceiver<DockerCommand>) {
    tokio::spawn(async move {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(docker) => docker,
            Err(err) => {
                let _ = msg_tx.send(Message::DockerError(format!(
                    "Cannot connect to Docker daemon: {err}"
                )));
                return;
            }
        };

        // Background poller: refresh the container list on a fixed cadence.
        {
            let docker = docker.clone();
            let tx = msg_tx.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(POLL_INTERVAL);
                loop {
                    ticker.tick().await;
                    send_container_list(&docker, &tx).await;
                }
            });
        }

        // Command loop: dispatch work without blocking on it.
        let mut log_task: Option<JoinHandle<()>> = None;
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                DockerCommand::Start { id, name } => {
                    run_action(docker.clone(), msg_tx.clone(), ContainerAction::Start, id, name);
                }
                DockerCommand::Stop { id, name } => {
                    run_action(docker.clone(), msg_tx.clone(), ContainerAction::Stop, id, name);
                }
                DockerCommand::Restart { id, name } => {
                    run_action(docker.clone(), msg_tx.clone(), ContainerAction::Restart, id, name);
                }
                DockerCommand::FetchStats { id } => {
                    let docker = docker.clone();
                    let tx = msg_tx.clone();
                    tokio::spawn(async move {
                        fetch_stats(docker, tx, id).await;
                    });
                }
                DockerCommand::StreamLogs { id } => {
                    if let Some(task) = log_task.take() {
                        task.abort();
                    }
                    let docker = docker.clone();
                    let tx = msg_tx.clone();
                    log_task = Some(tokio::spawn(async move {
                        stream_logs(docker, tx, id).await;
                    }));
                }
                DockerCommand::StopLogs => {
                    if let Some(task) = log_task.take() {
                        task.abort();
                    }
                }
                DockerCommand::ComposeUpdate { compose, name } => {
                    let docker = docker.clone();
                    let tx = msg_tx.clone();
                    tokio::spawn(async move {
                        run_compose_update(docker, tx, compose, name).await;
                    });
                }
            }
        }
    });
}

async fn send_container_list(docker: &Docker, tx: &UnboundedSender<Message>) {
    let options = ListContainersOptions::<String> { all: true, ..Default::default() };
    match docker.list_containers(Some(options)).await {
        Ok(summaries) => {
            let mut containers: Vec<ContainerInfo> =
                summaries.into_iter().map(summary_to_info).collect();
            containers.sort_by(|a, b| a.name.cmp(&b.name));
            let _ = tx.send(Message::ContainersUpdated(containers));
        }
        Err(err) => {
            let _ = tx.send(Message::DockerError(format!("Failed to list containers: {err}")));
        }
    }
}

fn summary_to_info(summary: ContainerSummary) -> ContainerInfo {
    // Docker reports names with a leading slash, e.g. "/web".
    let name = summary
        .names
        .as_ref()
        .and_then(|names| names.first())
        .map(|n| n.trim_start_matches('/').to_string())
        .unwrap_or_else(|| "<unnamed>".to_string());
    let ports = summary.ports.as_deref().map(format_ports).unwrap_or_default();
    let compose = summary.labels.as_ref().and_then(compose_info);

    ContainerInfo {
        id: summary.id.unwrap_or_default(),
        name,
        image: summary.image.unwrap_or_default(),
        state: summary.state.unwrap_or_default(),
        status: summary.status.unwrap_or_default(),
        ports,
        compose,
    }
}

/// Extract Compose metadata from the labels Compose writes onto every
/// container it manages.
fn compose_info(labels: &HashMap<String, String>) -> Option<ComposeInfo> {
    let project = labels.get("com.docker.compose.project")?.clone();
    let service = labels.get("com.docker.compose.service")?.clone();
    let working_dir = labels.get("com.docker.compose.project.working_dir").cloned();
    let config_files = labels
        .get("com.docker.compose.project.config_files")
        .map(|files| {
            files
                .split(',')
                .map(|f| f.trim().to_string())
                .filter(|f| !f.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Some(ComposeInfo { project, service, working_dir, config_files })
}

fn format_ports(ports: &[Port]) -> String {
    let mut parts: Vec<String> = ports
        .iter()
        .map(|p| {
            let proto = p
                .typ
                .map(|t| t.to_string())
                .unwrap_or_else(|| "tcp".to_string());
            match p.public_port {
                Some(public) => format!("{}->{}/{}", public, p.private_port, proto),
                None => format!("{}/{}", p.private_port, proto),
            }
        })
        .collect();
    parts.sort();
    parts.dedup();
    parts.join(", ")
}

fn run_action(
    docker: Docker,
    tx: UnboundedSender<Message>,
    action: ContainerAction,
    id: String,
    name: String,
) {
    tokio::spawn(async move {
        let result = match action {
            ContainerAction::Start => {
                docker.start_container(&id, None::<StartContainerOptions<String>>).await
            }
            ContainerAction::Stop => docker.stop_container(&id, None::<StopContainerOptions>).await,
            ContainerAction::Restart => {
                docker.restart_container(&id, None::<RestartContainerOptions>).await
            }
        };
        let (done, verb) = match action {
            ContainerAction::Start => ("Started", "start"),
            ContainerAction::Stop => ("Stopped", "stop"),
            ContainerAction::Restart => ("Restarted", "restart"),
        };
        match result {
            Ok(()) => {
                let _ = tx.send(Message::Status(format!("{done} {name}")));
            }
            Err(err) => {
                let _ = tx.send(Message::DockerError(format!("Failed to {verb} {name}: {err}")));
            }
        }
        // Reflect the new state immediately instead of waiting for the poller.
        send_container_list(&docker, &tx).await;
    });
}

async fn fetch_stats(docker: Docker, tx: UnboundedSender<Message>, id: String) {
    // stream: false makes the daemon sample twice and return one result
    // with precpu populated, which is what the CPU math needs.
    let options = StatsOptions { stream: false, one_shot: false };
    let mut stream = docker.stats(&id, Some(options));
    if let Some(Ok(stats)) = stream.next().await {
        let _ = tx.send(Message::StatsUpdated(id, compute_stats(&stats)));
    }
}

fn compute_stats(stats: &Stats) -> StatsInfo {
    let cpu_delta =
        stats.cpu_stats.cpu_usage.total_usage as f64 - stats.precpu_stats.cpu_usage.total_usage as f64;
    let system_delta = stats.cpu_stats.system_cpu_usage.unwrap_or(0) as f64
        - stats.precpu_stats.system_cpu_usage.unwrap_or(0) as f64;
    let online_cpus = stats.cpu_stats.online_cpus.unwrap_or(1).max(1) as f64;
    let cpu_percent = if system_delta > 0.0 && cpu_delta >= 0.0 {
        cpu_delta / system_delta * online_cpus * 100.0
    } else {
        0.0
    };

    let (net_rx, net_tx) = stats
        .networks
        .as_ref()
        .map(|nets| {
            nets.values()
                .fold((0u64, 0u64), |(rx, tx), n| (rx + n.rx_bytes, tx + n.tx_bytes))
        })
        .unwrap_or((0, 0));

    StatsInfo {
        cpu_percent,
        mem_usage: stats.memory_stats.usage.unwrap_or(0),
        mem_limit: stats.memory_stats.limit.unwrap_or(0),
        net_rx,
        net_tx,
    }
}

async fn stream_logs(docker: Docker, tx: UnboundedSender<Message>, id: String) {
    let options = LogsOptions::<String> {
        follow: true,
        stdout: true,
        stderr: true,
        tail: LOG_TAIL.to_string(),
        ..Default::default()
    };
    let mut stream = docker.logs(&id, Some(options));
    while let Some(item) = stream.next().await {
        match item {
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.into_bytes()).to_string();
                for line in text.split('\n') {
                    let line = line.trim_end_matches('\r');
                    if line.is_empty() {
                        continue;
                    }
                    if tx.send(Message::LogLine(id.clone(), line.to_string())).is_err() {
                        return;
                    }
                }
            }
            Err(err) => {
                let _ = tx.send(Message::DockerError(format!("Log stream error: {err}")));
                break;
            }
        }
    }
}

/// Update a Compose service: pull its latest image, then recreate it.
/// Output from both steps streams into the UI as `UpdateLine` messages.
async fn run_compose_update(
    docker: Docker,
    tx: UnboundedSender<Message>,
    compose: ComposeInfo,
    name: String,
) {
    let steps: [Vec<String>; 2] = [
        compose_args(&compose, &["pull", &compose.service]),
        compose_args(&compose, &["up", "-d", "--build", "--no-deps", &compose.service]),
    ];

    for args in steps {
        let _ = tx.send(Message::UpdateLine(format!("$ docker {}", args.join(" "))));
        match run_streamed(&tx, &compose, &args).await {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let code = status.code().map_or("signal".to_string(), |c| c.to_string());
                let _ = tx.send(Message::UpdateFinished {
                    success: false,
                    detail: format!("Update of {name} failed (exit {code})"),
                });
                return;
            }
            Err(err) => {
                let _ = tx.send(Message::UpdateLine(format!("error: {err}")));
                let _ = tx.send(Message::UpdateFinished {
                    success: false,
                    detail: format!("Update of {name} failed: could not run docker compose"),
                });
                return;
            }
        }
    }

    let _ = tx.send(Message::UpdateFinished {
        success: true,
        detail: format!("Updated {name}"),
    });
    // Reflect the recreated container immediately.
    send_container_list(&docker, &tx).await;
}

/// Build a `docker compose` argument list scoped to the container's own
/// project and config files, so the update targets exactly the stack that
/// created it.
fn compose_args(compose: &ComposeInfo, tail: &[&str]) -> Vec<String> {
    let mut args = vec!["compose".to_string(), "-p".to_string(), compose.project.clone()];
    for file in &compose.config_files {
        args.push("-f".to_string());
        args.push(file.clone());
    }
    args.extend(tail.iter().map(|s| s.to_string()));
    args
}

async fn run_streamed(
    tx: &UnboundedSender<Message>,
    compose: &ComposeInfo,
    args: &[String],
) -> std::io::Result<std::process::ExitStatus> {
    let mut cmd = Command::new("docker");
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = &compose.working_dir {
        cmd.current_dir(dir);
    }

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_task = stdout.map(|out| tokio::spawn(forward_lines(out, tx.clone())));
    let err_task = stderr.map(|err| tokio::spawn(forward_lines(err, tx.clone())));

    let status = child.wait().await;
    if let Some(task) = out_task {
        let _ = task.await;
    }
    if let Some(task) = err_task {
        let _ = task.await;
    }
    status
}

async fn forward_lines<R: AsyncRead + Unpin>(reader: R, tx: UnboundedSender<Message>) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        // Progress bars redraw with carriage returns; keep the final state.
        let cleaned = line.rsplit('\r').next().unwrap_or("").trim_end();
        if cleaned.is_empty() {
            continue;
        }
        if tx.send(Message::UpdateLine(cleaned.to_string())).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn compose_info_parses_full_labels() {
        let labels = labels(&[
            ("com.docker.compose.project", "camino"),
            ("com.docker.compose.service", "web"),
            ("com.docker.compose.project.working_dir", "/srv/camino"),
            (
                "com.docker.compose.project.config_files",
                "/srv/camino/compose.yaml,/srv/camino/compose.override.yaml",
            ),
        ]);
        let info = compose_info(&labels).expect("should parse");
        assert_eq!(info.project, "camino");
        assert_eq!(info.service, "web");
        assert_eq!(info.working_dir.as_deref(), Some("/srv/camino"));
        assert_eq!(
            info.config_files,
            vec!["/srv/camino/compose.yaml", "/srv/camino/compose.override.yaml"]
        );
    }

    #[test]
    fn compose_info_requires_project_and_service() {
        assert!(compose_info(&labels(&[("com.docker.compose.project", "x")])).is_none());
        assert!(compose_info(&labels(&[("com.docker.compose.service", "x")])).is_none());
        assert!(compose_info(&labels(&[("some.other.label", "x")])).is_none());
    }

    #[test]
    fn compose_args_scopes_to_project_and_files() {
        let compose = ComposeInfo {
            project: "camino".to_string(),
            service: "web".to_string(),
            working_dir: None,
            config_files: vec!["/srv/camino/compose.yaml".to_string()],
        };
        assert_eq!(
            compose_args(&compose, &["pull", "web"]),
            vec!["compose", "-p", "camino", "-f", "/srv/camino/compose.yaml", "pull", "web"]
        );
    }
}
