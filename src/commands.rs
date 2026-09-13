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
            message: Some(format!("Docker daemon unreachable or returned error: {stderr}")),
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
