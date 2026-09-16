use sb_agent_core::command_intake::{CommandOutcome, CommandRegistry};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerContainer {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub ports: String,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerPsEntry {
    #[serde(rename = "ID")]
    id: Option<String>,
    names: Option<String>,
    image: Option<String>,
    state: Option<String>,
    status: Option<String>,
    ports: Option<String>,
    created_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListContainersResponse {
    pub installed: bool,
    pub docker_running: bool,
    pub containers: Vec<DockerContainer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub fn register(registry: &CommandRegistry) {
    registry.register("docker_list_containers", |_payload, _progress| async move {
        tokio::task::spawn_blocking(list_containers)
            .await
            .unwrap_or_else(|e| CommandOutcome::failed(format!("task panicked: {e}")))
    });

    registry.register("docker_start_container", |payload, _progress| async move {
        tokio::task::spawn_blocking(move || start_container(payload))
            .await
            .unwrap_or_else(|e| CommandOutcome::failed(format!("task panicked: {e}")))
    });

    registry.register("docker_stop_container", |payload, _progress| async move {
        tokio::task::spawn_blocking(move || stop_container(payload))
            .await
            .unwrap_or_else(|e| CommandOutcome::failed(format!("task panicked: {e}")))
    });

    registry.register(
        "docker_restart_container",
        |payload, _progress| async move {
            tokio::task::spawn_blocking(move || restart_container(payload))
                .await
                .unwrap_or_else(|e| CommandOutcome::failed(format!("task panicked: {e}")))
        },
    );

    registry.register("docker_get_logs", |payload, _progress| async move {
        tokio::task::spawn_blocking(move || get_container_logs(payload))
            .await
            .unwrap_or_else(|e| CommandOutcome::failed(format!("task panicked: {e}")))
    });

    registry.register("update_now", |_payload, _progress| async move {
        update_now::handle().await
    });
}

fn list_containers() -> CommandOutcome {
    let has_docker = if cfg!(windows) {
        Command::new("where")
            .arg("docker")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    } else {
        Command::new("which")
            .arg("docker")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };

    if !has_docker {
        let resp = ListContainersResponse {
            installed: false,
            docker_running: false,
            containers: Vec::new(),
            message: Some("Docker CLI is not installed or not in PATH on this host".to_string()),
        };
        return CommandOutcome::ok(serde_json::to_string(&resp).unwrap_or_default());
    }

    let output = Command::new("docker")
        .args(["ps", "-a", "--format", "{{json .}}"])
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            let resp = ListContainersResponse {
                installed: true,
                docker_running: false,
                containers: Vec::new(),
                message: Some(format!("Failed to execute docker ps: {e}")),
            };
            return CommandOutcome::ok(serde_json::to_string(&resp).unwrap_or_default());
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let resp = ListContainersResponse {
            installed: true,
            docker_running: false,
            containers: Vec::new(),
            message: Some(format!(
                "Docker daemon unreachable or returned error: {stderr}"
            )),
        };
        return CommandOutcome::ok(serde_json::to_string(&resp).unwrap_or_default());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut containers = Vec::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Ok(entry) = serde_json::from_str::<DockerPsEntry>(trimmed) {
            containers.push(DockerContainer {
                id: entry.id.unwrap_or_default(),
                name: entry.names.unwrap_or_default(),
                image: entry.image.unwrap_or_default(),
                state: entry.state.unwrap_or_else(|| "unknown".to_string()),
                status: entry.status.unwrap_or_default(),
                ports: entry.ports.unwrap_or_default(),
                created_at: entry.created_at.unwrap_or_default(),
            });
        }
    }

    let resp = ListContainersResponse {
        installed: true,
        docker_running: true,
        containers,
        message: None,
    };

    CommandOutcome::ok(serde_json::to_string(&resp).unwrap_or_default())
}

#[derive(Debug, Deserialize)]
struct ContainerActionPayload {
    container_id: String,
    #[serde(default)]
    timeout_secs: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct GetLogsPayload {
    container_id: String,
    #[serde(default = "default_tail")]
    tail: u32,
    #[serde(default = "default_true")]
    timestamps: bool,
    #[serde(default)]
    since: Option<String>,
}

fn default_tail() -> u32 {
    200
}

fn default_true() -> bool {
    true
}

fn is_safe_container_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/')
}

fn is_safe_since(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == '-' || c == ':' || c == '.' || c == 'Z' || c == 'T'
        })
}

fn start_container(payload: serde_json::Value) -> CommandOutcome {
    let args: ContainerActionPayload = match serde_json::from_value(payload) {
        Ok(a) => a,
        Err(e) => return CommandOutcome::failed(format!("invalid payload: {e}")),
    };

    if !is_safe_container_id(&args.container_id) {
        return CommandOutcome::failed("invalid container_id");
    }

    let output = match Command::new("docker")
        .args(["start", &args.container_id])
        .output()
    {
        Ok(o) => o,
        Err(e) => return CommandOutcome::failed(format!("failed to execute docker start: {e}")),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return CommandOutcome::failed(format!("docker start failed: {stderr}"));
    }

    CommandOutcome::ok(
        json!({
            "success": true,
            "action": "start",
            "container_id": args.container_id
        })
        .to_string(),
    )
}

fn stop_container(payload: serde_json::Value) -> CommandOutcome {
    let args: ContainerActionPayload = match serde_json::from_value(payload) {
        Ok(a) => a,
        Err(e) => return CommandOutcome::failed(format!("invalid payload: {e}")),
    };

    if !is_safe_container_id(&args.container_id) {
        return CommandOutcome::failed("invalid container_id");
    }

    let timeout = args.timeout_secs.unwrap_or(10).to_string();
    let output = match Command::new("docker")
        .args(["stop", "-t", &timeout, &args.container_id])
        .output()
    {
        Ok(o) => o,
        Err(e) => return CommandOutcome::failed(format!("failed to execute docker stop: {e}")),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return CommandOutcome::failed(format!("docker stop failed: {stderr}"));
    }

    CommandOutcome::ok(
        json!({
            "success": true,
            "action": "stop",
            "container_id": args.container_id
        })
        .to_string(),
    )
}

fn restart_container(payload: serde_json::Value) -> CommandOutcome {
    let args: ContainerActionPayload = match serde_json::from_value(payload) {
        Ok(a) => a,
        Err(e) => return CommandOutcome::failed(format!("invalid payload: {e}")),
    };

    if !is_safe_container_id(&args.container_id) {
        return CommandOutcome::failed("invalid container_id");
    }

    let timeout = args.timeout_secs.unwrap_or(10).to_string();
    let output = match Command::new("docker")
        .args(["restart", "-t", &timeout, &args.container_id])
        .output()
    {
        Ok(o) => o,
        Err(e) => return CommandOutcome::failed(format!("failed to execute docker restart: {e}")),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return CommandOutcome::failed(format!("docker restart failed: {stderr}"));
    }

    CommandOutcome::ok(
        json!({
            "success": true,
            "action": "restart",
            "container_id": args.container_id
        })
        .to_string(),
    )
}

fn get_container_logs(payload: serde_json::Value) -> CommandOutcome {
    let args: GetLogsPayload = match serde_json::from_value(payload) {
        Ok(a) => a,
        Err(e) => return CommandOutcome::failed(format!("invalid payload: {e}")),
    };

    if !is_safe_container_id(&args.container_id) {
        return CommandOutcome::failed("invalid container_id");
    }

    let tail_str = args.tail.clamp(1, 2000).to_string();
    let mut cmd = Command::new("docker");
    cmd.args(["logs", "--tail", &tail_str]);
    if args.timestamps {
        cmd.arg("--timestamps");
    }
    if let Some(ref since) = args.since {
        if is_safe_since(since) {
            cmd.args(["--since", since]);
        }
    }
    cmd.arg(&args.container_id);

    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => return CommandOutcome::failed(format!("failed to execute docker logs: {e}")),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return CommandOutcome::failed(format!("docker logs failed: {stderr}"));
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let stderr_str = String::from_utf8_lossy(&output.stderr);

    let combined_logs = if args.timestamps {
        let mut lines: Vec<&str> = stdout_str
            .lines()
            .chain(stderr_str.lines())
            .filter(|l| !l.trim().is_empty())
            .collect();
        lines.sort();
        lines.join("\n")
    } else {
        let mut res = String::with_capacity(stdout_str.len() + stderr_str.len());
        res.push_str(&stdout_str);
        if !stderr_str.is_empty() {
            if !res.is_empty() && !res.ends_with('\n') {
                res.push('\n');
            }
            res.push_str(&stderr_str);
        }
        res
    };

    let total_lines = combined_logs.lines().count();

    CommandOutcome::ok(
        json!({
            "container_id": args.container_id,
            "tail": args.tail,
            "timestamps": args.timestamps,
            "total_lines": total_lines,
            "logs": combined_logs
        })
        .to_string(),
    )
}

mod update_now {
    use super::*;

    pub async fn handle() -> CommandOutcome {
        let cfg = sb_agent_core::updater::UpdaterConfig::new(
            "securyblack",
            "cromo-forge",
            "cromoforge",
            env!("CARGO_PKG_VERSION"),
        );

        let result =
            tokio::task::spawn_blocking(move || sb_agent_core::updater::check_now(&cfg)).await;

        match result {
            Ok(Ok(true)) => {
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    std::process::exit(0);
                });
                CommandOutcome::ok(
                    json!({ "updated": true, "previous_version": env!("CARGO_PKG_VERSION") })
                        .to_string(),
                )
            }
            Ok(Ok(false)) => CommandOutcome::ok(
                json!({ "updated": false, "current_version": env!("CARGO_PKG_VERSION") })
                    .to_string(),
            ),
            Ok(Err(e)) => CommandOutcome::failed(format!("update check failed: {e}")),
            Err(e) => CommandOutcome::failed(format!("update task panicked: {e}")),
        }
    }
}
