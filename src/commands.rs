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
    /// Proyecto y servicio de Docker Compose, o la app de SB: el panel agrupa por ellos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose_project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose_service: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sb_app: Option<String>,
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
    labels: Option<String>,
}

/// `docker ps` da las etiquetas como `k=v,k=v`. Un valor con comas (los `config_files` de
/// Compose) se parte, pero las tres que leemos nunca las llevan.
fn label(labels: &str, key: &str) -> Option<String> {
    labels
        .split(',')
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
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

    registry.register("docker_container_stats", |_payload, _progress| async move {
        tokio::task::spawn_blocking(container_stats)
            .await
            .unwrap_or_else(|e| CommandOutcome::failed(format!("task panicked: {e}")))
    });

    registry.register("update_now", |_payload, _progress| async move {
        update_now::handle().await
    });

    registry.register(
        "postgres_test_connection",
        |payload, _progress| async move {
            tokio::task::spawn_blocking(move || postgres_test_connection(payload))
                .await
                .unwrap_or_else(|e| CommandOutcome::failed(format!("task panicked: {e}")))
        },
    );

    registry.register(
        "postgres_collect_metrics",
        |payload, _progress| async move {
            tokio::task::spawn_blocking(move || postgres_collect_metrics(payload))
                .await
                .unwrap_or_else(|e| CommandOutcome::failed(format!("task panicked: {e}")))
        },
    );

    registry.register("postgres_action", |payload, _progress| async move {
        tokio::task::spawn_blocking(move || postgres_action(payload))
            .await
            .unwrap_or_else(|e| CommandOutcome::failed(format!("task panicked: {e}")))
    });

    // Despliegues desde git (Fase 1). Asíncronos: sus hijos mueren si la orden caduca.
    registry.register("deploy_app", crate::deploy::deploy_app);
    registry.register("remove_app", crate::deploy::remove_app);
    registry.register("proxy_status", crate::proxy::proxy_status);
    registry.register("proxy_install", crate::proxy::proxy_install);
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
            let labels = entry.labels.unwrap_or_default();
            containers.push(DockerContainer {
                compose_project: label(&labels, "com.docker.compose.project"),
                compose_service: label(&labels, "com.docker.compose.service"),
                sb_app: label(&labels, "sb.app"),
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
    #[serde(default)]
    container_id: String,
    /// Vista conjunta: los logs de varios contenedores, mezclados por hora.
    #[serde(default)]
    container_ids: Vec<String>,
    #[serde(default = "default_tail")]
    tail: u32,
    #[serde(default = "default_true")]
    timestamps: bool,
    /// Inclusivo, como `docker logs --since`. En modo `structured` se quitan las líneas
    /// con esa misma hora: son las que el panel ya tiene.
    #[serde(default)]
    since: Option<String>,
    /// `lines` con hora, stdout/stderr y contenedor, en vez de un texto.
    #[serde(default)]
    structured: bool,
}

const MAX_MERGED_CONTAINERS: usize = 20;

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

    let ids = if args.container_ids.is_empty() {
        vec![args.container_id.clone()]
    } else {
        args.container_ids.clone()
    };
    if ids.len() > MAX_MERGED_CONTAINERS || !ids.iter().all(|id| is_safe_container_id(id)) {
        return CommandOutcome::failed("invalid container_id");
    }

    let tail = args.tail.clamp(1, 2000);
    let since = args.since.as_deref().filter(|s| is_safe_since(s));
    let mut batches = Vec::with_capacity(ids.len());
    for id in &ids {
        // Siempre con hora: es lo que permite intercalar stdout, stderr y contenedores.
        let mut cmd = Command::new("docker");
        cmd.args(["logs", "--timestamps", "--tail", &tail.to_string()]);
        if let Some(since) = since {
            cmd.args(["--since", since]);
        }
        let output = match cmd.arg(id).output() {
            Ok(o) => o,
            Err(e) => return CommandOutcome::failed(format!("failed to execute docker logs: {e}")),
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return CommandOutcome::failed(format!("docker logs failed: {stderr}"));
        }
        batches.push(parse_docker_logs(
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
            id,
        ));
    }

    let lines = merge_log_lines(batches, since.filter(|_| args.structured), tail as usize);

    if args.structured {
        let last_ts = lines.last().map(|l| l.ts.as_str()).or(since).unwrap_or("");
        return CommandOutcome::ok(
            json!({
                "total_lines": lines.len(),
                "last_ts": last_ts,
                "lines": lines.iter().map(|l| json!({
                    "ts": l.ts,
                    "stream": l.stream,
                    "container": l.container,
                    "text": l.text,
                })).collect::<Vec<_>>(),
            })
            .to_string(),
        );
    }

    let combined_logs = lines
        .iter()
        .map(|l| {
            if args.timestamps && !l.ts.is_empty() {
                format!("{} {}", l.ts, l.text)
            } else {
                l.text.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    CommandOutcome::ok(
        json!({
            "container_id": args.container_id,
            "tail": args.tail,
            "timestamps": args.timestamps,
            "total_lines": lines.len(),
            "logs": combined_logs
        })
        .to_string(),
    )
}

#[derive(Debug)]
struct LogLine {
    ts: String,
    /// `ts` con la fracción a 9 dígitos: Docker los recorta y `.5Z` ordenaría tras `.45Z`.
    key: String,
    stream: &'static str,
    container: String,
    text: String,
}

fn parse_docker_logs(stdout: &str, stderr: &str, container: &str) -> Vec<LogLine> {
    let tag = |out: &str, stream: &'static str| -> Vec<LogLine> {
        out.lines()
            .map(|raw| {
                let raw = raw.strip_suffix('\r').unwrap_or(raw);
                let (ts, text) = match raw.split_once(' ') {
                    Some((ts, text)) if looks_like_ts(ts) => (ts, text),
                    _ if looks_like_ts(raw) => (raw, ""),
                    _ => ("", raw),
                };
                LogLine {
                    ts: ts.to_string(),
                    key: ts_key(ts),
                    stream,
                    container: container.to_string(),
                    text: text.to_string(),
                }
            })
            .collect()
    };
    let mut lines = tag(stdout, "stdout");
    lines.extend(tag(stderr, "stderr"));
    lines
}

fn looks_like_ts(s: &str) -> bool {
    s.len() >= 20 && s.as_bytes()[4] == b'-' && s.as_bytes()[10] == b'T' && s.ends_with('Z')
}

fn ts_key(ts: &str) -> String {
    let Some(body) = ts.strip_suffix('Z') else {
        return ts.to_string();
    };
    let (secs, frac) = body.split_once('.').unwrap_or((body, ""));
    format!("{secs}.{frac:0<9}Z")
}

/// Ordena por hora (estable: a igual hora se respeta el orden de Docker), quita lo que el
/// panel ya tiene (`since`) y se queda con las `tail` últimas.
fn merge_log_lines(batches: Vec<Vec<LogLine>>, since: Option<&str>, tail: usize) -> Vec<LogLine> {
    let mut lines: Vec<LogLine> = batches.into_iter().flatten().collect();
    if let Some(since) = since {
        lines.retain(|l| l.ts != since);
    }
    lines.sort_by(|a, b| a.key.cmp(&b.key));
    let skip = lines.len().saturating_sub(tail);
    lines.drain(..skip);
    lines
}

#[derive(Debug, Deserialize)]
struct DockerStatsEntry {
    #[serde(rename = "ID")]
    id: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "CPUPerc")]
    cpu_perc: Option<String>,
    #[serde(rename = "MemPerc")]
    mem_perc: Option<String>,
    #[serde(rename = "MemUsage")]
    mem_usage: Option<String>,
    #[serde(rename = "NetIO")]
    net_io: Option<String>,
    #[serde(rename = "BlockIO")]
    block_io: Option<String>,
    #[serde(rename = "PIDs")]
    pids: Option<String>,
}

/// `12.34%` → 12.34; `--` (contenedor parado) → None.
fn percent(s: &Option<String>) -> Option<f64> {
    s.as_deref()?.trim().trim_end_matches('%').parse().ok()
}

/// Una foto de `docker stats` de los contenedores en marcha. Tarda un par de segundos,
/// que es lo que Docker necesita para calcular la CPU.
fn container_stats() -> CommandOutcome {
    let output = match Command::new("docker")
        .args(["stats", "--no-stream", "--format", "{{json .}}"])
        .output()
    {
        Ok(o) => o,
        Err(e) => return CommandOutcome::failed(format!("failed to execute docker stats: {e}")),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return CommandOutcome::failed(format!("docker stats failed: {stderr}"));
    }

    let containers: Vec<_> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<DockerStatsEntry>(l.trim()).ok())
        .map(|e| {
            json!({
                "id": e.id.clone().unwrap_or_default(),
                "name": e.name.clone().unwrap_or_default(),
                "cpu_percent": percent(&e.cpu_perc),
                "mem_percent": percent(&e.mem_perc),
                "mem_usage": e.mem_usage.unwrap_or_default(),
                "net_io": e.net_io.unwrap_or_default(),
                "block_io": e.block_io.unwrap_or_default(),
                "pids": e.pids.and_then(|p| p.parse::<u32>().ok()),
            })
        })
        .collect();

    CommandOutcome::ok(json!({ "containers": containers }).to_string())
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

// ─────────────────────────────────────────────────────────────────────────────
// PostgreSQL Manager & Performance Insights
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PostgresCommandArgs {
    pub container_name: Option<String>,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_database")]
    pub database: String,
    #[serde(default = "default_username")]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    5432
}
fn default_database() -> String {
    "postgres".to_string()
}
fn default_username() -> String {
    "securyblack_monitor".to_string()
}

fn execute_psql(args: &PostgresCommandArgs, sql: &str) -> Result<String, String> {
    if let Some(ref c) = args.container_name {
        let trimmed = c.trim();
        if !trimmed.is_empty() {
            let mut cmd = Command::new("docker");
            cmd.arg("exec");
            if !args.password.is_empty() {
                cmd.arg("-e").arg(format!("PGPASSWORD={}", args.password));
            }
            cmd.arg(trimmed)
                .arg("psql")
                .arg("-U")
                .arg(&args.username)
                .arg("-d")
                .arg(&args.database)
                .arg("-t")
                .arg("-A")
                .arg("-c")
                .arg(sql);

            let out = cmd
                .output()
                .map_err(|e| format!("Failed to execute docker exec: {e}"))?;

            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(if err.is_empty() {
                    "Postgres command failed without stderr".to_string()
                } else {
                    err
                });
            }
            return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }

    let mut cmd = Command::new("psql");
    cmd.arg("-h")
        .arg(&args.host)
        .arg("-p")
        .arg(args.port.to_string())
        .arg("-U")
        .arg(&args.username)
        .arg("-d")
        .arg(&args.database)
        .arg("-t")
        .arg("-A")
        .arg("-c")
        .arg(sql);

    if !args.password.is_empty() {
        cmd.env("PGPASSWORD", &args.password);
    }

    let out = cmd
        .output()
        .map_err(|e| format!("Failed to execute psql on host: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            "Host psql command failed without stderr".to_string()
        } else {
            err
        });
    }

    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn postgres_test_connection(payload: serde_json::Value) -> CommandOutcome {
    let args: PostgresCommandArgs = match serde_json::from_value(payload) {
        Ok(a) => a,
        Err(e) => return CommandOutcome::failed(format!("Invalid arguments: {e}")),
    };

    match execute_psql(&args, "SELECT version();") {
        Ok(v) => CommandOutcome::ok(
            json!({
                "success": true,
                "version": v,
                "message": "Connection successful"
            })
            .to_string(),
        ),
        Err(e) => CommandOutcome::ok(
            json!({
                "success": false,
                "error": e,
                "message": "Failed to connect to PostgreSQL"
            })
            .to_string(),
        ),
    }
}

fn postgres_collect_metrics(payload: serde_json::Value) -> CommandOutcome {
    let args: PostgresCommandArgs = match serde_json::from_value(payload) {
        Ok(a) => a,
        Err(e) => return CommandOutcome::failed(format!("Invalid arguments: {e}")),
    };

    // 1. Version
    let version = match execute_psql(&args, "SELECT version();") {
        Ok(v) => v,
        Err(e) => {
            return CommandOutcome::ok(
                json!({
                    "success": false,
                    "error": e,
                    "message": "PostgreSQL unreachable"
                })
                .to_string(),
            );
        }
    };

    // 2. Conexiones
    let conn_sql = "SELECT count(*) || '|' || count(*) FILTER (WHERE state = 'active') || '|' || count(*) FILTER (WHERE state = 'idle') || '|' || count(*) FILTER (WHERE state = 'idle in transaction') || '|' || (SELECT setting::int FROM pg_settings WHERE name = 'max_connections') FROM pg_stat_activity;";
    let conn_stats = execute_psql(&args, conn_sql).unwrap_or_default();
    let conn_parts: Vec<&str> = conn_stats.split('|').collect();
    let total_conn: i64 = conn_parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let active_conn: i64 = conn_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let idle_conn: i64 = conn_parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let idle_in_tx: i64 = conn_parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let max_conn: i64 = conn_parts
        .get(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);

    // 3. Tamaño de base de datos y Cache Hit Ratio
    let size_sql = "SELECT pg_database_size(current_database()) || '|' || coalesce(round(sum(blks_hit) * 100.0 / nullif(sum(blks_hit + blks_read), 0), 2), 100.0) FROM pg_stat_database WHERE datname = current_database();";
    let size_stats = execute_psql(&args, size_sql).unwrap_or_default();
    let size_parts: Vec<&str> = size_stats.split('|').collect();
    let db_bytes: i64 = size_parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let cache_hit_pct: f64 = size_parts
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100.0);

    // 4. Slow queries (from pg_stat_statements, excluding backup COPY streams)
    let slow_queries_sql = "SELECT coalesce(json_agg(row_to_json(t)), '[]'::json) FROM (SELECT query, calls, round(total_exec_time::numeric, 2) as total_exec_time, round(mean_exec_time::numeric, 2) as mean_exec_time, round(min_exec_time::numeric, 2) as min_exec_time, round(max_exec_time::numeric, 2) as max_exec_time, rows FROM pg_stat_statements WHERE query NOT ILIKE 'COPY %' ORDER BY total_exec_time DESC LIMIT 10) t;";
    let (has_stat_statements, slow_queries_json) = match execute_psql(&args, slow_queries_sql) {
        Ok(raw) => {
            let parsed: serde_json::Value =
                serde_json::from_str(&raw).unwrap_or_else(|_| json!([]));
            (true, parsed)
        }
        Err(_) => (false, json!([])),
    };

    // 5. Table bloat & dead tuples
    let bloat_sql = "SELECT coalesce(json_agg(row_to_json(t)), '[]'::json) FROM (SELECT schemaname || '.' || relname as table_name, n_live_tup, n_dead_tup, coalesce(round(100.0 * n_dead_tup / nullif(n_live_tup + n_dead_tup, 0), 2), 0) as dead_pct, pg_total_relation_size(relid) as total_bytes, to_char(last_vacuum, 'YYYY-MM-DD HH24:MI:SS') as last_vacuum, to_char(last_autovacuum, 'YYYY-MM-DD HH24:MI:SS') as last_autovacuum, to_char(last_analyze, 'YYYY-MM-DD HH24:MI:SS') as last_analyze FROM pg_stat_user_tables ORDER BY n_dead_tup DESC LIMIT 10) t;";
    let tables_bloat_json: serde_json::Value = match execute_psql(&args, bloat_sql) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| json!([])),
        Err(_) => json!([]),
    };

    // 6. Configuración / Settings de PostgreSQL
    let settings_sql = "SELECT coalesce(json_agg(row_to_json(t)), '[]'::json) FROM (SELECT name, setting, unit FROM pg_settings WHERE name IN ('shared_buffers', 'work_mem', 'maintenance_work_mem', 'effective_cache_size', 'max_connections', 'wal_buffers')) t;";
    let settings_json: serde_json::Value = match execute_psql(&args, settings_sql) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| json!([])),
        Err(_) => json!([]),
    };

    // 7. Active queries (pg_stat_activity)
    let active_queries_sql = "SELECT coalesce(json_agg(row_to_json(t)), '[]'::json) FROM (SELECT pid, usename, client_addr::text as client_ip, state, round(extract(epoch from (now() - query_start))::numeric, 2) as duration_secs, wait_event_type, wait_event, query FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND state <> 'idle' ORDER BY query_start ASC LIMIT 20) t;";
    let active_queries_json: serde_json::Value = match execute_psql(&args, active_queries_sql) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| json!([])),
        Err(_) => json!([]),
    };

    // 8. Available & installed extensions
    let extensions_sql = "SELECT coalesce(json_agg(row_to_json(t)), '[]'::json) FROM (SELECT name, default_version, installed_version, comment FROM pg_available_extensions WHERE name IN ('pg_stat_statements', 'pgcrypto', 'uuid-ossp', 'vector', 'timescaledb', 'citext', 'pg_trgm', 'postgis', 'hstore', 'unaccent') ORDER BY name) t;";
    let extensions_json: serde_json::Value = match execute_psql(&args, extensions_sql) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| json!([])),
        Err(_) => json!([]),
    };

    // 9. Unused indexes (> 1 MB and 0 scans)
    let unused_indexes_sql = "SELECT coalesce(json_agg(row_to_json(t)), '[]'::json) FROM (SELECT schemaname || '.' || relname as table_name, indexrelname as index_name, pg_size_pretty(pg_relation_size(i.indexrelid)) as index_size_pretty, pg_relation_size(i.indexrelid) as index_size_bytes, idx_scan FROM pg_stat_user_indexes ui JOIN pg_index i ON ui.indexrelid = i.indexrelid WHERE NOT indisunique AND idx_scan = 0 AND pg_relation_size(i.indexrelid) > 1024 * 1024 ORDER BY pg_relation_size(i.indexrelid) DESC LIMIT 15) t;";
    let unused_indexes_json: serde_json::Value = match execute_psql(&args, unused_indexes_sql) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| json!([])),
        Err(_) => json!([]),
    };

    CommandOutcome::ok(
        json!({
            "success": true,
            "version": version,
            "database_name": args.database,
            "connections": {
                "total": total_conn,
                "active": active_conn,
                "idle": idle_conn,
                "idle_in_transaction": idle_in_tx,
                "max": max_conn,
                "usage_pct": if max_conn > 0 { (total_conn as f64 / max_conn as f64 * 100.0).round() } else { 0.0 }
            },
            "storage": {
                "size_bytes": db_bytes,
                "cache_hit_ratio": cache_hit_pct
            },
            "pg_stat_statements_installed": has_stat_statements,
            "slow_queries": slow_queries_json,
            "tables_bloat": tables_bloat_json,
            "settings": settings_json,
            "active_queries": active_queries_json,
            "installed_extensions": extensions_json,
            "unused_indexes": unused_indexes_json
        })
        .to_string(),
    )
}

#[derive(Debug, Deserialize)]
pub struct PostgresActionArgs {
    #[serde(flatten)]
    pub conn: PostgresCommandArgs,
    pub action: String,
    pub pid: Option<i64>,
    pub extension_name: Option<String>,
    pub table_name: Option<String>,
}

fn is_safe_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

fn postgres_action(payload: serde_json::Value) -> CommandOutcome {
    let args: PostgresActionArgs = match serde_json::from_value(payload) {
        Ok(a) => a,
        Err(e) => return CommandOutcome::failed(format!("Invalid arguments: {e}")),
    };

    match args.action.as_str() {
        "cancel_query" => {
            let pid = match args.pid {
                Some(p) => p,
                None => return CommandOutcome::failed("pid is required for cancel_query"),
            };
            let sql = format!("SELECT pg_cancel_backend({pid});");
            match execute_psql(&args.conn, &sql) {
                Ok(res) => CommandOutcome::ok(
                    json!({ "success": true, "action": "cancel_query", "pid": pid, "result": res })
                        .to_string(),
                ),
                Err(e) => {
                    CommandOutcome::failed(format!("Failed to cancel query for PID {pid}: {e}"))
                }
            }
        }
        "terminate_connection" => {
            let pid = match args.pid {
                Some(p) => p,
                None => return CommandOutcome::failed("pid is required for terminate_connection"),
            };
            let sql = format!("SELECT pg_terminate_backend({pid});");
            match execute_psql(&args.conn, &sql) {
                Ok(res) => CommandOutcome::ok(
                    json!({ "success": true, "action": "terminate_connection", "pid": pid, "result": res }).to_string(),
                ),
                Err(e) => CommandOutcome::failed(format!("Failed to terminate connection for PID {pid}: {e}")),
            }
        }
        "install_extension" => {
            let ext = match args.extension_name {
                Some(ref e) if is_safe_identifier(e) => e.as_str(),
                _ => return CommandOutcome::failed("Valid extension_name is required"),
            };
            let sql = format!("CREATE EXTENSION IF NOT EXISTS \"{ext}\";");
            match execute_psql(&args.conn, &sql) {
                Ok(_) => CommandOutcome::ok(
                    json!({ "success": true, "action": "install_extension", "extension": ext })
                        .to_string(),
                ),
                Err(e) => CommandOutcome::failed(format!("Failed to install extension {ext}: {e}")),
            }
        }
        "vacuum_table" => {
            let table = match args.table_name {
                Some(ref t) if is_safe_identifier(t) => t.as_str(),
                _ => return CommandOutcome::failed("Valid table_name is required"),
            };
            let sql = format!("VACUUM (ANALYZE) {table};");
            match execute_psql(&args.conn, &sql) {
                Ok(_) => CommandOutcome::ok(
                    json!({ "success": true, "action": "vacuum_table", "table": table })
                        .to_string(),
                ),
                Err(e) => CommandOutcome::failed(format!("Failed to vacuum table {table}: {e}")),
            }
        }
        "reindex_table" => {
            let table = match args.table_name {
                Some(ref t) if is_safe_identifier(t) => t.as_str(),
                _ => return CommandOutcome::failed("Valid table_name is required"),
            };
            let sql = format!("REINDEX TABLE CONCURRENTLY {table};");
            match execute_psql(&args.conn, &sql) {
                Ok(_) => CommandOutcome::ok(
                    json!({ "success": true, "action": "reindex_table", "table": table })
                        .to_string(),
                ),
                Err(e) => CommandOutcome::failed(format!("Failed to reindex table {table}: {e}")),
            }
        }
        other => CommandOutcome::failed(format!("Unsupported action: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_compose_and_sb_labels() {
        let labels = "com.docker.compose.project=monitor,com.docker.compose.project.config_files=/a.yml,/b.yml,com.docker.compose.service=dozzle,sb.app=42";
        assert_eq!(
            label(labels, "com.docker.compose.project").as_deref(),
            Some("monitor")
        );
        assert_eq!(
            label(labels, "com.docker.compose.service").as_deref(),
            Some("dozzle")
        );
        assert_eq!(label(labels, "sb.app").as_deref(), Some("42"));
        assert_eq!(label(labels, "sb.deployment"), None);
        assert_eq!(label("", "sb.app"), None);
    }

    #[test]
    fn fraction_is_padded_so_order_is_numeric() {
        assert!(ts_key("2026-10-22T10:00:00.45Z") < ts_key("2026-10-22T10:00:00.5Z"));
        assert!(ts_key("2026-10-22T10:00:00Z") < ts_key("2026-10-22T10:00:00.000000001Z"));
    }

    #[test]
    fn interleaves_streams_and_containers_by_time() {
        let a = parse_docker_logs(
            "2026-10-22T10:00:00.1Z out 1\n2026-10-22T10:00:00.3Z out 2\r\n",
            "2026-10-22T10:00:00.25Z err 1\n",
            "api",
        );
        let b = parse_docker_logs("2026-10-22T10:00:00.2Z worker 1\n", "", "worker");
        let merged = merge_log_lines(vec![a, b], None, 100);
        let got: Vec<_> = merged
            .iter()
            .map(|l| (l.container.as_str(), l.stream, l.text.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("api", "stdout", "out 1"),
                ("worker", "stdout", "worker 1"),
                ("api", "stderr", "err 1"),
                ("api", "stdout", "out 2"),
            ]
        );
    }

    #[test]
    fn drops_lines_already_seen_and_keeps_the_tail() {
        let lines = parse_docker_logs(
            "2026-10-22T10:00:01Z seen\n2026-10-22T10:00:02Z new 1\n2026-10-22T10:00:03Z new 2\n",
            "",
            "api",
        );
        let merged = merge_log_lines(vec![lines], Some("2026-10-22T10:00:01Z"), 1);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "new 2");
    }

    #[test]
    fn keeps_lines_without_timestamp_and_empty_lines() {
        let lines = parse_docker_logs("2026-10-22T10:00:01Z\nplain\n", "", "api");
        assert_eq!(
            (lines[0].ts.as_str(), lines[0].text.as_str()),
            ("2026-10-22T10:00:01Z", "")
        );
        assert_eq!(
            (lines[1].ts.as_str(), lines[1].text.as_str()),
            ("", "plain")
        );
    }

    #[test]
    fn parses_docker_percentages() {
        assert_eq!(percent(&Some("12.34%".into())), Some(12.34));
        assert_eq!(percent(&Some("--".into())), None);
        assert_eq!(percent(&None), None);
    }
}
