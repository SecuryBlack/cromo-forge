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

    registry.register("postgres_test_connection", |payload, _progress| async move {
        tokio::task::spawn_blocking(move || postgres_test_connection(payload))
            .await
            .unwrap_or_else(|e| CommandOutcome::failed(format!("task panicked: {e}")))
    });

    registry.register("postgres_collect_metrics", |payload, _progress| async move {
        tokio::task::spawn_blocking(move || postgres_collect_metrics(payload))
            .await
            .unwrap_or_else(|e| CommandOutcome::failed(format!("task panicked: {e}")))
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
    let total_conn: i64 = conn_parts.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
    let active_conn: i64 = conn_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let idle_conn: i64 = conn_parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let idle_in_tx: i64 = conn_parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let max_conn: i64 = conn_parts.get(4).and_then(|s| s.parse().ok()).unwrap_or(100);

    // 3. Tamaño de base de datos y Cache Hit Ratio
    let size_sql = "SELECT pg_database_size(current_database()) || '|' || coalesce(round(sum(blks_hit) * 100.0 / nullif(sum(blks_hit + blks_read), 0), 2), 100.0) FROM pg_stat_database WHERE datname = current_database();";
    let size_stats = execute_psql(&args, size_sql).unwrap_or_default();
    let size_parts: Vec<&str> = size_stats.split('|').collect();
    let db_bytes: i64 = size_parts.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
    let cache_hit_pct: f64 = size_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(100.0);

    // 4. Slow queries (from pg_stat_statements)
    let slow_queries_sql = "SELECT coalesce(json_agg(row_to_json(t)), '[]'::json) FROM (SELECT query, calls, round(total_exec_time::numeric, 2) as total_exec_time, round(mean_exec_time::numeric, 2) as mean_exec_time, round(min_exec_time::numeric, 2) as min_exec_time, round(max_exec_time::numeric, 2) as max_exec_time, rows FROM pg_stat_statements ORDER BY total_exec_time DESC LIMIT 10) t;";
    let (has_stat_statements, slow_queries_json) = match execute_psql(&args, slow_queries_sql) {
        Ok(raw) => {
            let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!([]));
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
            "settings": settings_json
        })
        .to_string(),
    )
}

