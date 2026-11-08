//! Bases de datos sueltas: Postgres o Redis sin app (`database_run` y
//! `database_remove`). Escuchan solo en 127.0.0.1 del servidor; desde fuera
//! se entra con un túnel SSH. Imagen, usuario, volumen y comando los fija el
//! agente: la API solo elige motor, puerto y contraseña.
//!
//! Nombres en Docker: contenedor `sbdb-<id>`, volumen `sbdb-<id>-data`, los
//! dos con la etiqueta `sb.db=<id>`. Contrato en
//! `sb-wiki/Tecnica/Fase 1 - Contrato de despliegues.md`.

use crate::deploy::{app_lock, docker, is_uuid, log_rotation, write_private, WorkDir};
use sb_agent_core::command_intake::{CommandOutcome, ProgressSender};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Engine {
    Postgres,
    Redis,
}

impl Engine {
    fn image(self) -> &'static str {
        match self {
            Engine::Postgres => "postgres:17-alpine",
            Engine::Redis => "redis:7-alpine",
        }
    }

    fn port(self) -> u16 {
        match self {
            Engine::Postgres => 5432,
            Engine::Redis => 6379,
        }
    }

    fn data_dir(self) -> &'static str {
        match self {
            Engine::Postgres => "/var/lib/postgresql/data",
            Engine::Redis => "/data",
        }
    }

    /// Por `sh -c` con el entrypoint de la imagen (baja a su usuario). La
    /// contraseña de Redis llega por el env-file, nunca en los argumentos.
    fn command(self) -> &'static str {
        match self {
            Engine::Postgres => {
                "exec docker-entrypoint.sh postgres -c shared_preload_libraries=pg_stat_statements"
            }
            Engine::Redis => {
                "exec docker-entrypoint.sh redis-server --appendonly yes --requirepass \"$REDIS_PASSWORD\""
            }
        }
    }
}

#[derive(Deserialize)]
struct RunRequest {
    id: String,
    engine: Engine,
    port: u16,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct RemoveRequest {
    id: String,
}

fn container(id: &str) -> String {
    format!("sbdb-{id}")
}

fn volume(id: &str) -> String {
    format!("sbdb-{id}-data")
}

impl RunRequest {
    /// Solo hex/alfanumérico: va tal cual a una línea del env-file y a la URL.
    fn password(&self) -> Result<&str, String> {
        self.env
            .get("PASSWORD")
            .map(String::as_str)
            .filter(|p| {
                (16..=128).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric())
            })
            .ok_or_else(|| "invalid password".to_string())
    }

    fn validate(&self) -> Result<(), String> {
        if !is_uuid(&self.id) {
            return Err("invalid id".into());
        }
        if self.port < 1024 {
            return Err("invalid port".into());
        }
        self.password().map(|_| ())
    }

    fn env_file(&self) -> Result<String, String> {
        let pw = self.password()?;
        Ok(match self.engine {
            Engine::Postgres => {
                format!("POSTGRES_USER=app\nPOSTGRES_DB=app\nPOSTGRES_PASSWORD={pw}\n")
            }
            Engine::Redis => format!("REDIS_PASSWORD={pw}\n"),
        })
    }

    /// `docker run` sin la contraseña: va en `env_file`.
    fn run_args(&self, env_file: &Path, log: Vec<String>) -> Vec<String> {
        let label = format!("sb.db={}", self.id);
        let mut args: Vec<String> = vec![
            "run".into(),
            "-d".into(),
            "--pull".into(),
            "never".into(),
            "--name".into(),
            container(&self.id),
            "--restart".into(),
            "unless-stopped".into(),
            "--label".into(),
            label.clone(),
            // Solo la propia máquina: sin puerto abierto ni regla de firewall.
            "-p".into(),
            format!("127.0.0.1:{}:{}", self.port, self.engine.port()),
            "--mount".into(),
            format!(
                "type=volume,source={},target={},volume-label={label}",
                volume(&self.id),
                self.engine.data_dir()
            ),
            "--env-file".into(),
            env_file.to_string_lossy().into_owned(),
        ];
        args.extend(log);
        args.extend([
            "--entrypoint".into(),
            "/bin/sh".into(),
            self.engine.image().into(),
            "-c".into(),
            self.engine.command().into(),
        ]);
        args
    }
}

/// Arranca la base de datos (o la vuelve a arrancar si ya existe: así es
/// idempotente) y espera a que acepte conexiones.
pub async fn database_run(payload: Value, _tx: ProgressSender) -> CommandOutcome {
    let req: RunRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return CommandOutcome::failed(format!("invalid payload: {e}")),
    };
    if let Err(e) = req.validate() {
        return CommandOutcome::failed(e);
    }
    let lock = app_lock(&container(&req.id));
    let _guard = lock.lock().await;

    match run(&req).await {
        Ok(()) => CommandOutcome::ok(json!({ "id": req.id, "port": req.port }).to_string()),
        Err(e) => CommandOutcome::failed(e),
    }
}

async fn run(req: &RunRequest) -> Result<(), String> {
    let name = container(&req.id);
    let state = docker(["inspect", "-f", "{{.State.Running}}", &name]).await;
    match state.as_deref() {
        Ok("true") => return Ok(()),
        Ok(_) => {
            docker(["start", &name]).await?;
        }
        Err(_) => {
            if std::net::TcpListener::bind(("127.0.0.1", req.port)).is_err() {
                return Err(format!(
                    "port {} is already in use on this server",
                    req.port
                ));
            }
            if let Err(e) = docker(["pull", "-q", req.engine.image()]).await {
                tracing::warn!(
                    "{}: pull failed, using the local image: {e}",
                    req.engine.image()
                );
            }
            let dir = WorkDir::new("db", &req.id)?;
            let env_file = dir.0.join("db.env");
            write_private(&env_file, &req.env_file()?)?;
            let result = docker(req.run_args(&env_file, log_rotation().await)).await;
            drop(dir);
            result?;
        }
    }
    wait_ready(req.engine, &name).await
}

/// Postgres tarda unos segundos en inicializar el volumen la primera vez.
async fn wait_ready(engine: Engine, name: &str) -> Result<(), String> {
    for _ in 0..90 {
        let ready = match engine {
            Engine::Postgres => docker(["exec", name, "pg_isready", "-q", "-U", "app"])
                .await
                .is_ok(),
            // Sin contraseña contesta NOAUTH: basta para saber que escucha.
            Engine::Redis => docker(["exec", name, "redis-cli", "ping"])
                .await
                .is_ok_and(|o| o.contains("PONG") || o.contains("NOAUTH")),
        };
        if ready {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let logs = docker(["logs", "--tail", "20", name])
        .await
        .unwrap_or_default();
    Err(format!("{name} did not become ready: {logs}"))
}

/// Quita el contenedor y su volumen, con los datos (el panel lo avisa antes).
pub async fn database_remove(payload: Value, _tx: ProgressSender) -> CommandOutcome {
    let req: RemoveRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return CommandOutcome::failed(format!("invalid payload: {e}")),
    };
    if !is_uuid(&req.id) {
        return CommandOutcome::failed("invalid id");
    }
    let lock = app_lock(&container(&req.id));
    let _guard = lock.lock().await;

    let _ = docker(["rm", "-f", &container(&req.id)]).await;
    match docker(["volume", "rm", &volume(&req.id)]).await {
        Ok(_) => {}
        Err(e) if e.to_lowercase().contains("no such volume") => {}
        Err(e) => return CommandOutcome::failed(e),
    }
    CommandOutcome::ok(json!({ "id": req.id }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0b5e3c2a-1d4f-4e6a-9b7c-8d9e0f1a2b3c";

    fn req(v: Value) -> Result<RunRequest, String> {
        let r: RunRequest = serde_json::from_value(v).map_err(|e| e.to_string())?;
        r.validate().map(|_| r)
    }

    #[test]
    fn run_args_bind_localhost_and_keep_the_password_out() {
        let pw = "a1b2c3d4e5f6a7b8c9d0e1f2";
        let r = req(
            json!({ "id": ID, "engine": "postgres", "port": 15432, "env": { "PASSWORD": pw } }),
        )
        .unwrap();
        let args = r.run_args(Path::new("/tmp/db.env"), vec![]);
        assert!(args.contains(&"127.0.0.1:15432:5432".to_string()));
        assert!(args.contains(&format!("sbdb-{ID}")));
        assert!(args.contains(&format!(
            "type=volume,source=sbdb-{ID}-data,target=/var/lib/postgresql/data,volume-label=sb.db={ID}"
        )));
        assert!(
            !args.iter().any(|a| a.contains(pw)),
            "la contraseña va en el env-file"
        );
        assert_eq!(
            r.env_file().unwrap(),
            format!("POSTGRES_USER=app\nPOSTGRES_DB=app\nPOSTGRES_PASSWORD={pw}\n")
        );

        let r =
            req(json!({ "id": ID, "engine": "redis", "port": 16379, "env": { "PASSWORD": pw } }))
                .unwrap();
        let args = r.run_args(Path::new("/tmp/db.env"), vec![]);
        assert!(args.contains(&"127.0.0.1:16379:6379".to_string()));
        assert!(!args.iter().any(|a| a.contains(pw)));
        assert!(args
            .last()
            .unwrap()
            .contains("--requirepass \"$REDIS_PASSWORD\""));
        assert_eq!(r.env_file().unwrap(), format!("REDIS_PASSWORD={pw}\n"));
    }

    #[test]
    fn bad_payloads_are_rejected() {
        let ok = json!({ "id": ID, "engine": "postgres", "port": 15432, "env": { "PASSWORD": "a".repeat(24) } });
        assert!(req(ok.clone()).is_ok());
        let cases: Vec<(&str, Value)> = vec![
            ("/id", json!("sbdb-x")),
            ("/engine", json!("mysql")),
            ("/port", json!(80)),
            ("/env/PASSWORD", json!("corta")),
            ("/env/PASSWORD", json!("con espacios y saltos\nde línea")),
            ("/env/PASSWORD", json!("a".repeat(129))),
        ];
        for (ptr, bad) in cases {
            let mut v = ok.clone();
            *v.pointer_mut(ptr).unwrap() = bad.clone();
            assert!(req(v).is_err(), "{ptr} = {bad} should be rejected");
        }
        let mut v = ok;
        v["env"] = json!({});
        assert!(req(v).is_err(), "sin contraseña");
    }
}
