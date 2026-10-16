//! Despliegues desde git (Fase 1): `deploy_app` y `remove_app`.
//!
//! El camino de `deploy_app` es fetch (tarball de GitHub) → build (Railpack
//! contra el BuildKit `sb-buildkit`) → start → healthcheck → switch (ruta de
//! Caddy) → cleanup. Las apps `cron` paran tras el build: guardan su horario
//! en `CRON_DIR` y las lanza `run_due_cron_jobs` cada minuto. Formatos, nombres en Docker y etapas de progreso están
//! fijados en `sb-wiki/Tecnica/Fase 1 - Contrato de despliegues.md`; si algo
//! cambia aquí, cambia antes allí.
//!
//! Reglas que no se negocian:
//! - Todo argumento externo se valida antes de usarlo y va siempre como un
//!   elemento del vector de argumentos, nunca por un shell.
//! - El token de GitHub solo vive en memoria y en la cabecera `Authorization`.
//! - Los hijos largos se lanzan con `kill_on_drop`: si la orden caduca, el
//!   core suelta el future y el build/`docker` muere con él.

use crate::cron;
use crate::proxy;
use sb_agent_core::command_intake::{CommandOutcome, CommandProgress, ProgressSender};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Command;

const BUILDKIT_CONTAINER: &str = "sb-buildkit";
const BUILDKIT_IMAGE: &str = "moby/buildkit:v0.23.2";
const PORT_RANGE: std::ops::RangeInclusive<u16> = 20000..=29999;
const MAX_TARBALL_BYTES: u64 = 1024 * 1024 * 1024;
const LOG_LINE_MAX_CHARS: usize = 2000;
const HEARTBEAT: Duration = Duration::from_secs(60);
const CRON_DIR: &str = "/var/lib/cromoforge/cron";

// ─────────────────────────────────────────────────────────────────────────────
// Payload y validación
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum AppKind {
    Web,
    Worker,
    Cron,
}

#[derive(Debug, Deserialize)]
struct AppSpec {
    id: String,
    name: String,
    #[serde(rename = "type")]
    kind: AppKind,
    #[serde(default = "default_app_port")]
    port: u16,
    #[serde(default = "default_healthcheck_path")]
    healthcheck_path: String,
    #[serde(default = "default_healthcheck_timeout")]
    healthcheck_timeout_secs: u64,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    start_command: Option<String>,
    /// Solo en `cron`: 5 campos, UTC.
    #[serde(default)]
    schedule: Option<String>,
}

/// Sin `Debug` a propósito: lleva el token de instalación de GitHub.
#[derive(Deserialize)]
struct Source {
    repo: String,
    sha: String,
    token: String,
}

/// Sin `Debug` a propósito: `source.token`, `env` y `build_env` son secretos.
#[derive(Deserialize)]
struct DeployRequest {
    deployment_id: String,
    app: AppSpec,
    #[serde(default)]
    source: Option<Source>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    build_env: BTreeMap<String, String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default = "default_keep_images")]
    keep_images: usize,
}

fn default_app_port() -> u16 {
    3000
}
fn default_healthcheck_path() -> String {
    "/".to_string()
}
fn default_healthcheck_timeout() -> u64 {
    60
}
fn default_keep_images() -> usize {
    3
}

/// UUID en minúsculas (`sbapp/<id>` va en un nombre de imagen, que no admite mayúsculas).
fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        })
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// `^[a-z0-9][a-z0-9-]{0,39}$`
fn is_app_name(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.chars()
            .enumerate()
            .all(|(i, c)| c.is_ascii_lowercase() || c.is_ascii_digit() || (i > 0 && c == '-'))
}

/// `owner/name` con el juego de caracteres de GitHub.
fn is_repo(s: &str) -> bool {
    let Some((owner, name)) = s.split_once('/') else {
        return false;
    };
    (1..=39).contains(&owner.len())
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && (1..=100).contains(&name.len())
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn is_domain(s: &str) -> bool {
    s.len() <= 253
        && s.contains('.')
        && s.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// `^[A-Za-z_][A-Za-z0-9_]*$`
fn is_env_key(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && s.len() <= 256
}

fn is_healthcheck_path(s: &str) -> bool {
    s.starts_with('/') && s.len() <= 1024 && s.chars().all(|c| c.is_ascii_graphic())
}

/// Tokens de instalación (`ghs_...`): van en una cabecera, así que nada de
/// espacios ni saltos de línea.
fn is_token(s: &str) -> bool {
    (1..=512).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

impl DeployRequest {
    fn validate(&mut self) -> Result<(), String> {
        let app = &mut self.app;
        if !is_uuid(&self.deployment_id) {
            return Err("invalid deployment_id".into());
        }
        if !is_uuid(&app.id) {
            return Err("invalid app.id".into());
        }
        if !is_app_name(&app.name) {
            return Err("invalid app.name".into());
        }
        if app.port == 0 {
            return Err("invalid app.port".into());
        }
        if !is_healthcheck_path(&app.healthcheck_path) {
            return Err("invalid app.healthcheck_path".into());
        }
        app.healthcheck_timeout_secs = app.healthcheck_timeout_secs.clamp(1, 600);
        if let Some(d) = &mut app.domain {
            *d = d.to_ascii_lowercase();
            if !is_domain(d) {
                return Err("invalid app.domain".into());
            }
        }
        if let Some(cmd) = &app.start_command {
            if cmd.is_empty() || cmd.len() > 4096 || cmd.contains('\0') {
                return Err("invalid app.start_command".into());
            }
        }
        if app.kind == AppKind::Cron {
            let schedule = app
                .schedule
                .as_deref()
                .ok_or("app.schedule is required for cron apps")?;
            cron::Schedule::parse(schedule).map_err(|e| format!("invalid app.schedule: {e}"))?;
        }
        match (&self.source, &self.image) {
            (Some(src), None) => {
                if !is_repo(&src.repo) {
                    return Err("invalid source.repo".into());
                }
                if !is_lower_hex(&src.sha, 40) {
                    return Err("invalid source.sha".into());
                }
                if !is_token(&src.token) {
                    return Err("invalid source.token".into());
                }
            }
            // La vuelta atrás solo puede usar imágenes que construimos nosotros
            // para esta misma app (y `docker run --pull never` lo remata).
            (None, Some(image)) => {
                let tag = image.strip_prefix(&format!("sbapp/{}:", app.id));
                if !tag.is_some_and(|t| is_lower_hex(t, 12)) {
                    return Err("invalid image".into());
                }
            }
            _ => return Err("exactly one of source or image is required".into()),
        }
        for (k, v) in self.env.iter().chain(&self.build_env) {
            if !is_env_key(k) {
                return Err(format!("invalid env key: {k}"));
            }
            if v.contains('\0') {
                return Err(format!("invalid env value for {k}"));
            }
        }
        // `--env-file` es una línea por variable, sin escapes posibles.
        if let Some((k, _)) = self.env.iter().find(|(_, v)| v.contains(['\n', '\r'])) {
            return Err(format!("env value for {k} contains a newline"));
        }
        self.keep_images = self.keep_images.clamp(1, 50);
        Ok(())
    }

    fn container_name(&self) -> String {
        format!("sbapp-{}-{}", self.app.id, &self.deployment_id[..8])
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Progreso
// ─────────────────────────────────────────────────────────────────────────────

/// Emite `CommandProgress`. Cada cambio de etapa reinicia un latido que
/// repite la etapa cada 60 s hasta la siguiente (o hasta que esto se suelte).
struct Progress {
    tx: ProgressSender,
    heartbeat: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Progress {
    fn new(tx: ProgressSender) -> Self {
        Self {
            tx,
            heartbeat: Mutex::new(None),
        }
    }

    fn send(tx: &ProgressSender, stage: &str, message: &str) {
        let _ = tx.send(CommandProgress {
            command_id: String::new(), // lo sella el core
            stage: stage.to_string(),
            message: message.to_string(),
            percent: -1,
        });
    }

    fn stage(&self, stage: &'static str, message: &'static str) {
        Self::send(&self.tx, stage, message);
        let tx = self.tx.clone();
        let hb = tokio::spawn(async move {
            loop {
                tokio::time::sleep(HEARTBEAT).await;
                Self::send(&tx, stage, message);
            }
        });
        if let Some(old) = self.heartbeat.lock().unwrap().replace(hb) {
            old.abort();
        }
    }

    fn log(&self, line: &str) {
        let line: String = line.chars().take(LOG_LINE_MAX_CHARS).collect();
        Self::send(&self.tx, "build_log", &line);
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if let Some(hb) = self.heartbeat.lock().unwrap().take() {
            hb.abort();
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Procesos y HTTP
// ─────────────────────────────────────────────────────────────────────────────

/// `docker <args>` y su stdout recortado; el stderr de Docker es el error.
pub async fn docker<I, S>(args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("failed to execute docker: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("docker exited with {}", out.status)
        } else {
            err
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn lines(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Cliente para 127.0.0.1 (healthcheck y API de Caddy): sin proxy del
/// entorno y sin seguir redirecciones fuera de la máquina.
pub fn local_http(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// Lee un stream línea a línea, manda cada una como `build_log` y devuelve
/// las últimas 20 para el mensaje de error.
async fn pump<R: AsyncRead + Unpin>(stream: R, p: &Progress) -> VecDeque<String> {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    let mut tail = VecDeque::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf).trim_end().to_string();
        if line.is_empty() {
            continue;
        }
        p.log(&line);
        tail.push_back(line);
        if tail.len() > 20 {
            tail.pop_front();
        }
    }
    tail
}

/// Corre `cmd` volcando stdout y stderr como `build_log`.
async fn run_streamed(mut cmd: Command, program: &str, p: &Progress) -> Result<(), String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{program} is not installed on this host"),
        _ => format!("failed to execute {program}: {e}"),
    })?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (out_tail, err_tail, status) = tokio::join!(pump(stdout, p), pump(stderr, p), child.wait());
    let status = status.map_err(|e| format!("{program}: {e}"))?;
    if status.success() {
        return Ok(());
    }
    let tail = if err_tail.is_empty() {
        out_tail
    } else {
        err_tail
    };
    Err(format!(
        "{program} exited with {status}:\n{}",
        Vec::from(tail).join("\n")
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Lógica pura (con tests)
// ─────────────────────────────────────────────────────────────────────────────

/// Primer puerto del rango 20000-29999 que no esté publicado por Docker ni
/// escuchando en la máquina.
fn pick_port(is_free: impl Fn(u16) -> bool) -> Option<u16> {
    PORT_RANGE.into_iter().find(|&p| is_free(p))
}

/// Puertos de host en la columna `{{.Ports}}` de `docker ps`
/// (`127.0.0.1:20001->3000/tcp, :::8080->80/tcp`).
fn parse_docker_ports(ports: &str) -> Vec<u16> {
    ports
        .split(',')
        .filter_map(|m| m.split_once("->"))
        .filter_map(|(host, _)| host.rsplit(':').next()?.trim().parse::<u16>().ok())
        .collect()
}

/// Límites del BuildKit: la mitad de las CPU y min(50 % de la RAM, 4 GB),
/// nunca por debajo de 1 GB. Devuelve (`--cpus`, MB de `--memory`).
fn buildkit_limits(cpus: usize, host_mem_mb: Option<u64>) -> (String, u64) {
    let cpus = (cpus.max(1) as f64) / 2.0;
    let mem = host_mem_mb
        .map(|m| (m / 2).min(4096))
        .unwrap_or(1024)
        .max(1024);
    (format!("{cpus}"), mem)
}

/// Qué referencias borrar: se conservan las `keep` más recientes y siempre
/// la que está desplegada. `images` es (fecha RFC 3339 de `Created`, ref).
fn images_to_remove(mut images: Vec<(String, String)>, keep: usize, current: &str) -> Vec<String> {
    images.sort_by(|a, b| b.0.cmp(&a.0));
    images
        .into_iter()
        .enumerate()
        .filter(|(i, (_, r))| *i >= keep && r != current)
        .map(|(_, (_, r))| r)
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Etapas
// ─────────────────────────────────────────────────────────────────────────────

/// Directorio de trabajo del build; se borra al soltarse (también si la
/// orden caduca). En `/var/tmp` y no en `/tmp`, que suele ser tmpfs.
struct WorkDir(PathBuf);

impl WorkDir {
    /// `/var/tmp/cromoforge-<kind>-<deployment_id>`, modo 0700.
    fn new(kind: &str, deployment_id: &str) -> Result<Self, String> {
        let base = if cfg!(unix) {
            PathBuf::from("/var/tmp")
        } else {
            std::env::temp_dir()
        };
        let dir = base.join(format!("cromoforge-{kind}-{deployment_id}"));
        let _ = std::fs::remove_dir_all(&dir);
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
            .create(&dir)
            .map_err(|e| format!("create {}: {e}", dir.display()))?;
        Ok(Self(dir))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Descarga el tarball del commit y lo extrae. Devuelve el directorio raíz
/// del repo (GitHub mete todo en `owner-repo-sha/`).
async fn fetch_source(src: &Source, work: &Path) -> Result<PathBuf, String> {
    let url = format!(
        "https://api.github.com/repos/{}/tarball/{}",
        src.repo, src.sha
    );
    let client = reqwest::Client::builder()
        .user_agent(concat!("cromoforge/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(900))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    // reqwest quita `Authorization` al redirigir a otro host (codeload.github.com).
    let mut resp = client
        .get(&url)
        .bearer_auth(&src.token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| format!("GitHub download failed: {}", e.without_url()))?;
    if !resp.status().is_success() {
        return Err(format!(
            "GitHub returned {} for {}@{}",
            resp.status(),
            src.repo,
            src.sha
        ));
    }

    let tarball = work.join("source.tar.gz");
    let mut file = tokio::fs::File::create(&tarball)
        .await
        .map_err(|e| format!("write tarball: {e}"))?;
    let mut total = 0u64;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("GitHub download failed: {}", e.without_url()))?
    {
        total += chunk.len() as u64;
        if total > MAX_TARBALL_BYTES {
            return Err("source tarball exceeds 1 GB".into());
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("write tarball: {e}"))?;
    }
    file.flush()
        .await
        .map_err(|e| format!("write tarball: {e}"))?;
    drop(file);

    let dest = work.join("src");
    tokio::task::spawn_blocking(move || -> Result<PathBuf, String> {
        let f = std::fs::File::open(&tarball).map_err(|e| format!("open tarball: {e}"))?;
        // `unpack` rechaza rutas que salgan de `dest`.
        tar::Archive::new(flate2::read::GzDecoder::new(f))
            .unpack(&dest)
            .map_err(|e| format!("extract tarball: {e}"))?;
        let mut dirs = std::fs::read_dir(&dest)
            .map_err(|e| format!("read {}: {e}", dest.display()))?
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()));
        match (dirs.next(), dirs.next()) {
            (Some(d), None) => Ok(d.path()),
            _ => Err("unexpected tarball layout".into()),
        }
    })
    .await
    .map_err(|e| format!("extract task panicked: {e}"))?
}

fn host_mem_mb() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))?
        .split_whitespace()
        .next()?;
    kb.parse::<u64>().ok().map(|kb| kb / 1024)
}

/// Crea o arranca `sb-buildkit` y espera a que `buildkitd` responda.
async fn ensure_buildkit(p: &Progress) -> Result<(), String> {
    match docker(["inspect", "-f", "{{.State.Running}}", BUILDKIT_CONTAINER]).await {
        Ok(s) if s == "true" => return Ok(()),
        Ok(_) => {
            docker(["start", BUILDKIT_CONTAINER]).await?;
        }
        Err(_) => {
            let cpus = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1);
            let (cpus, mem_mb) = buildkit_limits(cpus, host_mem_mb());
            p.log(&format!(
                "Creating {BUILDKIT_CONTAINER} (cpus {cpus}, memory {mem_mb} MB)"
            ));
            docker([
                "run",
                "-d",
                "--name",
                BUILDKIT_CONTAINER,
                "--privileged",
                "--restart",
                "unless-stopped",
                "--cpus",
                &cpus,
                "--memory",
                &format!("{mem_mb}m"),
                // Volumen con nombre: la caché sobrevive a recrear el contenedor
                // y no queda un volumen anónimo huérfano llenando el disco.
                "-v",
                "sb-buildkit-state:/var/lib/buildkit",
                BUILDKIT_IMAGE,
            ])
            .await?;
        }
    }
    let mut last = String::new();
    for _ in 0..30 {
        match docker(["exec", BUILDKIT_CONTAINER, "buildctl", "debug", "workers"]).await {
            Ok(_) => return Ok(()),
            Err(e) => last = e,
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(format!("{BUILDKIT_CONTAINER} not ready: {last}"))
}

/// ponytail: flags sacados de railpack.com (CLI reference + getting started);
/// según la doc, el CLI hace `docker load` del resultado con el nombre de
/// `--name`. Verificar en la prueba del paso 0 en el HomeLab.
async fn railpack_build(
    dir: &Path,
    image: &str,
    build_env: &BTreeMap<String, String>,
    start_command: Option<&str>,
    p: &Progress,
) -> Result<(), String> {
    let mut cmd = Command::new("railpack");
    cmd.arg("build")
        .arg(dir)
        .args(["--name", image, "--progress", "plain"]);
    for (k, v) in build_env {
        cmd.arg("--env").arg(format!("{k}={v}"));
    }
    if let Some(start) = start_command {
        cmd.args(["--start-cmd", start]);
    }
    cmd.env(
        "BUILDKIT_HOST",
        format!("docker-container://{BUILDKIT_CONTAINER}"),
    );
    run_streamed(cmd, "railpack", p).await
}

/// Puertos de host ya publicados por contenedores en marcha.
async fn docker_published_ports() -> Result<Vec<u16>, String> {
    let out = docker(["ps", "--format", "{{.Ports}}"]).await?;
    Ok(out.lines().flat_map(parse_docker_ports).collect())
}

async fn start_container(
    req: &DeployRequest,
    image: &str,
    container: &str,
    host_port: Option<u16>,
) -> Result<(), String> {
    let app = &req.app;
    let mut args: Vec<String> = vec![
        "run".into(),
        "-d".into(),
        "--pull".into(),
        "never".into(),
        "--name".into(),
        container.into(),
        "--restart".into(),
        "unless-stopped".into(),
        "--label".into(),
        format!("sb.app={}", app.id),
        "--label".into(),
        format!("sb.deployment={}", req.deployment_id),
    ];
    // `env` va por un fichero 0600 que se borra al volver `docker run`: ni en
    // la lista de procesos (argv) ni en el entorno del propio CLI de Docker
    // (donde una app podría colar LD_PRELOAD o DOCKER_HOST).
    let env_dir = WorkDir::new("env", &req.deployment_id)?;
    let env_file = env_dir.0.join("app.env");
    write_private(&env_file, &env_file_content(&req.env, host_port.is_some()))?;
    args.push("--env-file".into());
    args.push(env_file.to_string_lossy().into_owned());
    if let Some(hp) = host_port {
        args.push("-p".into());
        args.push(format!("127.0.0.1:{hp}:{}", app.port));
        args.push("-e".into());
        args.push(format!("PORT={}", app.port));
    }
    args.push(image.into());
    let result = docker(args).await.map(|_| ());
    drop(env_dir);
    result
}

/// Contenido de `--env-file`: `K=V` por línea, literal (Docker no interpreta
/// comillas ni escapes). Los saltos de línea ya se rechazan al validar. En
/// las web `PORT` lo fija el contrato y va aparte.
fn env_file_content(env: &BTreeMap<String, String>, skip_port: bool) -> String {
    env.iter()
        .filter(|(k, _)| !(skip_port && k.as_str() == "PORT"))
        .map(|(k, v)| format!("{k}={v}\n"))
        .collect()
}

fn write_private(path: &Path, content: &str) -> Result<(), String> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(path)
        .and_then(|mut f| f.write_all(content.as_bytes()))
        .map_err(|e| format!("write {}: {e}", path.display()))
}

async fn healthcheck(
    req: &DeployRequest,
    container: &str,
    host_port: Option<u16>,
) -> Result<(), String> {
    let Some(hp) = host_port else {
        // Worker: vivo 30 s después y sin reinicios.
        tokio::time::sleep(Duration::from_secs(30)).await;
        let state = docker([
            "inspect",
            "-f",
            "{{.State.Running}} {{.RestartCount}}",
            container,
        ])
        .await?;
        return if state == "true 0" {
            Ok(())
        } else {
            Err(format!(
                "container not stable after 30 s (running/restarts: {state})"
            ))
        };
    };

    let url = format!("http://127.0.0.1:{hp}{}", req.app.healthcheck_path);
    let timeout = req.app.healthcheck_timeout_secs;
    let client = local_http(Duration::from_secs(5))?;
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        let last = match client.get(&url).send().await {
            Ok(r) if !r.status().is_server_error() => return Ok(()),
            Ok(r) => format!("HTTP {}", r.status()),
            Err(e) => e.without_url().to_string(),
        };
        if Instant::now() >= deadline {
            return Err(format!(
                "healthcheck GET {} failed after {timeout} s: {last}",
                req.app.healthcheck_path
            ));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Vuelca las últimas líneas del contenedor como `build_log` antes de borrarlo.
async fn dump_container_logs(container: &str, p: &Progress) {
    if let Ok(out) = Command::new("docker")
        .args(["logs", "--tail", "50", container])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
    {
        for line in String::from_utf8_lossy(&out.stdout)
            .lines()
            .chain(String::from_utf8_lossy(&out.stderr).lines())
        {
            p.log(line);
        }
    }
}

async fn container_names(app_id: &str) -> Result<Vec<String>, String> {
    Ok(lines(
        &docker([
            "ps",
            "-a",
            "--filter",
            &format!("label=sb.app={app_id}"),
            "--format",
            "{{.Names}}",
        ])
        .await?,
    ))
}

/// Para y borra los contenedores viejos de la app y deja las `keep`
/// imágenes más recientes. Los fallos aquí no tumban el despliegue (la
/// versión nueva ya está sirviendo), solo se registran.
async fn cleanup(
    app_id: &str,
    new_container: &str,
    image: &str,
    keep: usize,
    p: &Progress,
) -> Result<(), String> {
    let old: Vec<String> = container_names(app_id)
        .await?
        .into_iter()
        .filter(|c| c != new_container)
        .collect();
    if !old.is_empty() {
        let _ = docker(
            ["stop", "-t", "10"]
                .into_iter()
                .map(String::from)
                .chain(old.iter().cloned()),
        )
        .await;
        docker(["rm", "-f"].into_iter().map(String::from).chain(old)).await?;
    }

    let repo = format!("sbapp/{app_id}");
    let listed = docker([
        "images",
        &repo,
        "--format",
        "{{.ID}} {{.Repository}}:{{.Tag}}",
    ])
    .await?;
    let mut refs = Vec::new();
    for line in lines(&listed) {
        let Some((id, r)) = line.split_once(' ') else {
            continue;
        };
        if r.ends_with(":<none>") {
            // Restos de reconstruir el mismo sha: ya no los referencia nadie.
            let _ = docker(["rmi", id]).await;
        } else {
            refs.push(r.to_string());
        }
    }
    if refs.len() <= keep {
        return Ok(());
    }
    let created = docker(
        ["image", "inspect", "--format", "{{.Created}}"]
            .into_iter()
            .map(String::from)
            .chain(refs.iter().cloned()),
    )
    .await?;
    let images: Vec<(String, String)> = lines(&created).into_iter().zip(refs).collect();
    for r in images_to_remove(images, keep, image) {
        if let Err(e) = docker(["rmi", &r]).await {
            p.log(&format!("could not remove {r}: {e}"));
        }
    }
    Ok(())
}

/// Tamaño total en KB de la salida de `df -Pk <dir>`.
fn parse_df_total_kb(out: &str) -> Option<u64> {
    out.lines().nth(1)?.split_whitespace().nth(1)?.parse().ok()
}

/// `--keep-storage` en MB: 10 % del disco, mínimo 2 GB.
fn prune_keep_mb(disk_kb: u64) -> u64 {
    (disk_kb / 1024 / 10).max(2048)
}

/// Recorta la caché de BuildKit según el disco donde vive el data root de Docker.
async fn prune_buildkit() -> Result<(), String> {
    let root = docker(["info", "-f", "{{.DockerRootDir}}"]).await?;
    let out = Command::new("df")
        .args(["-Pk", &root])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("failed to execute df: {e}"))?;
    let disk_kb = parse_df_total_kb(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| format!("could not read disk size of {root}"))?;
    let keep = prune_keep_mb(disk_kb).to_string();
    docker([
        "exec",
        BUILDKIT_CONTAINER,
        "buildctl",
        "prune",
        "--keep-storage",
        &keep,
    ])
    .await
    .map(|_| ())
}

// ─────────────────────────────────────────────────────────────────────────────
// Cron
// ─────────────────────────────────────────────────────────────────────────────

/// `<CRON_DIR>/<app.id>.json`, 0600. Sin `Debug`: `env` son secretos.
#[derive(Serialize, Deserialize)]
struct CronJob {
    image: String,
    schedule: String,
    start_command: Option<String>,
    env: BTreeMap<String, String>,
}

fn cron_file(dir: &Path, app_id: &str) -> PathBuf {
    dir.join(format!("{app_id}.json"))
}

fn cron_container(app_id: &str) -> String {
    format!("sbapp-{app_id}-run")
}

/// Escribe en un `.tmp` 0600 y lo renombra encima: el planificador nunca ve
/// un fichero a medias.
fn save_cron_job(dir: &Path, app_id: &str, job: &CronJob) -> Result<(), String> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .map_err(|e| format!("create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("chmod {}: {e}", dir.display()))?;
    }
    let path = cron_file(dir, app_id);
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let content = serde_json::to_string(job).map_err(|e| format!("serialize cron job: {e}"))?;
    write_private(&tmp, &content)?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("write {}: {e}", path.display()))
}

fn load_cron_job(dir: &Path, app_id: &str) -> Result<Option<CronJob>, String> {
    let path = cron_file(dir, app_id);
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s)
            .map(Some)
            .map_err(|e| format!("parse {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Apps de `dir` cuyo horario coincide con `now`.
fn due_cron_apps(dir: &Path, now: cron::Minute) -> Vec<String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!("read {}: {e}", dir.display());
            return Vec::new();
        }
    };
    let mut due = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(app_id) = name.strip_suffix(".json").filter(|id| is_uuid(id)) else {
            continue;
        };
        let schedule = load_cron_job(dir, app_id)
            .and_then(|job| job.map(|j| cron::Schedule::parse(&j.schedule)).transpose());
        match schedule {
            Ok(Some(s)) if s.matches(now) => due.push(app_id.to_string()),
            Ok(_) => {}
            Err(e) => tracing::warn!(app_id, "cron job skipped: {e}"),
        }
    }
    due
}

/// Lanza en segundo plano los cron que tocan en `now`.
pub fn run_due_cron_jobs(now: cron::Minute) {
    for app_id in due_cron_apps(Path::new(CRON_DIR), now) {
        tokio::spawn(async move {
            if let Err(e) = run_cron_job(&app_id).await {
                tracing::warn!(app_id, "cron run failed: {e}");
            }
        });
    }
}

async fn run_cron_job(app_id: &str) -> Result<(), String> {
    // ponytail: comparte candado con deploy_app, así que un build largo
    // retrasa la ejecución hasta que acabe. Separar candados si molesta.
    let lock = app_lock(app_id);
    let _guard = lock.lock().await;
    // Se relee bajo el candado: `remove_app` o un despliegue pueden haberlo cambiado.
    let Some(job) = load_cron_job(Path::new(CRON_DIR), app_id)? else {
        return Ok(());
    };

    let name = cron_container(app_id);
    if docker(["inspect", "-f", "{{.State.Running}}", &name])
        .await
        .as_deref()
        == Ok("true")
    {
        tracing::warn!(app_id, "previous cron run still running, skipping this one");
        return Ok(());
    }
    let _ = docker(["rm", "-f", &name]).await;

    let env_dir = WorkDir::new("cron", app_id)?;
    let env_file = env_dir.0.join("app.env");
    write_private(&env_file, &env_file_content(&job.env, false))?;
    let env_path = env_file.to_string_lossy().into_owned();
    let label = format!("sb.app={app_id}");
    let result = docker([
        "run",
        "-d",
        "--pull",
        "never",
        "--name",
        &name,
        "--label",
        &label,
        "--env-file",
        &env_path,
        &job.image,
    ])
    .await
    .map(|_| ());
    drop(env_dir);
    if result.is_ok() {
        tracing::info!(app_id, "cron run started");
    }
    result
}

// ─────────────────────────────────────────────────────────────────────────────
// Órdenes
// ─────────────────────────────────────────────────────────────────────────────

/// Un candado por `app.id`: dos órdenes de la misma app van en fila.
fn app_lock(app_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    // ponytail: el mapa no se poda nunca; una entrada por app, crece con las apps del servidor.
    LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(app_id.to_string())
        .or_default()
        .clone()
}

type StageError = (&'static str, String);

fn at(stage: &'static str) -> impl FnOnce(String) -> StageError {
    move |e| (stage, e)
}

pub async fn deploy_app(payload: Value, tx: ProgressSender) -> CommandOutcome {
    let mut req: DeployRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return CommandOutcome::failed(format!("invalid payload: {e}")),
    };
    if let Err(e) = req.validate() {
        return CommandOutcome::failed(e);
    }

    let lock = app_lock(&req.app.id);
    let _guard = lock.lock().await;
    let p = Progress::new(tx);

    match run_deploy(&req, &p).await {
        // `container` y `host_port` son nulos en los cron (no se arranca nada).
        Ok((image, container, host_port)) => CommandOutcome::ok(
            json!({
                "deployment_id": req.deployment_id,
                "image": image,
                "container": container,
                "host_port": host_port,
            })
            .to_string(),
        ),
        Err((stage, err)) => CommandOutcome {
            success: false,
            stdout: json!({ "deployment_id": req.deployment_id, "failed_stage": stage })
                .to_string(),
            stderr: err,
            exit_code: 1,
        },
    }
}

async fn run_deploy(
    req: &DeployRequest,
    p: &Progress,
) -> Result<(String, Option<String>, Option<u16>), StageError> {
    let app = &req.app;

    let image = match (&req.source, &req.image) {
        (Some(src), _) => {
            p.stage("fetch", "Downloading source code");
            let work = WorkDir::new("build", &req.deployment_id).map_err(at("fetch"))?;
            let dir = fetch_source(src, &work.0).await.map_err(at("fetch"))?;

            p.stage("build", "Building image");
            ensure_buildkit(p).await.map_err(at("build"))?;
            let image = format!("sbapp/{}:{}", app.id, &src.sha[..12]);
            railpack_build(
                &dir,
                &image,
                &req.build_env,
                app.start_command.as_deref(),
                p,
            )
            .await
            .map_err(at("build"))?;
            if let Err(e) = prune_buildkit().await {
                tracing::warn!(app_id = %app.id, "buildkit prune failed: {e}");
                p.log(&format!("BuildKit cache prune failed: {e}"));
            }
            image
        }
        (None, Some(image)) => image.clone(),
        (None, None) => unreachable!("validated"),
    };

    p.stage("start", "Starting container");
    docker(["image", "inspect", "--format", "{{.Id}}", &image])
        .await
        .map_err(|_| ("start", format!("image {image} not found on this host")))?;
    if app.kind == AppKind::Cron {
        let job = CronJob {
            image: image.clone(),
            schedule: app.schedule.clone().unwrap_or_default(),
            start_command: app.start_command.clone(),
            env: req.env.clone(),
        };
        save_cron_job(Path::new(CRON_DIR), &app.id, &job).map_err(at("start"))?;
        p.stage("cleanup", "Removing old images");
        // Se salva el contenedor de la última ejecución (puede estar en marcha).
        if let Err(e) = cleanup(
            &app.id,
            &cron_container(&app.id),
            &image,
            req.keep_images,
            p,
        )
        .await
        {
            p.log(&format!("cleanup: {e}"));
        }
        return Ok((image, None, None));
    }
    let host_port = match app.kind {
        AppKind::Worker | AppKind::Cron => None,
        AppKind::Web => {
            let published = docker_published_ports().await.map_err(at("start"))?;
            let free = |port: u16| {
                !published.contains(&port)
                    && std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
            };
            Some(
                pick_port(free)
                    .ok_or_else(|| ("start", "no free port in 20000-29999".to_string()))?,
            )
        }
    };
    let container = req.container_name();
    // Por si un intento anterior de este mismo despliegue dejó el contenedor.
    let _ = docker(["rm", "-f", &container]).await;
    if let Err(e) = start_container(req, &image, &container, host_port).await {
        let _ = docker(["rm", "-f", &container]).await;
        return Err(("start", e));
    }

    p.stage("healthcheck", "Checking the new version");
    if let Err(e) = healthcheck(req, &container, host_port).await {
        dump_container_logs(&container, p).await;
        let _ = docker(["rm", "-f", &container]).await;
        return Err(("healthcheck", e));
    }

    if let (Some(hp), Some(domain)) = (host_port, &app.domain) {
        p.stage("switch", "Switching traffic");
        if proxy::caddy_state().await == "running" {
            if let Err(e) = proxy::upsert_route(&app.id, domain, hp).await {
                let _ = docker(["rm", "-f", &container]).await;
                return Err(("switch", e));
            }
        } else {
            p.log("Proxy not installed, route not changed");
        }
    }

    p.stage("cleanup", "Stopping the previous version");
    tokio::time::sleep(Duration::from_secs(10)).await;
    if let Err(e) = cleanup(&app.id, &container, &image, req.keep_images, p).await {
        p.log(&format!("cleanup: {e}"));
    }

    Ok((image, Some(container), host_port))
}

#[derive(Deserialize)]
struct RemoveRequest {
    app_id: String,
}

pub async fn remove_app(payload: Value, _tx: ProgressSender) -> CommandOutcome {
    let req: RemoveRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return CommandOutcome::failed(format!("invalid payload: {e}")),
    };
    if !is_uuid(&req.app_id) {
        return CommandOutcome::failed("invalid app_id");
    }
    let lock = app_lock(&req.app_id);
    let _guard = lock.lock().await;

    let result = async {
        match std::fs::remove_file(cron_file(Path::new(CRON_DIR), &req.app_id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("remove cron file: {e}"))
            }
            _ => {}
        }
        let containers = container_names(&req.app_id).await?;
        if !containers.is_empty() {
            docker(
                ["rm", "-f"]
                    .into_iter()
                    .map(String::from)
                    .chain(containers.iter().cloned()),
            )
            .await?;
        }
        // Por etiqueta, no por ID: si otra app tiene la misma imagen (mismo
        // repo y commit), solo se le quitan las etiquetas de esta.
        let listed = docker([
            "images",
            &format!("sbapp/{}", req.app_id),
            "--format",
            "{{.ID}} {{.Repository}}:{{.Tag}}",
        ])
        .await?;
        let mut refs = Vec::new();
        for line in lines(&listed) {
            let Some((id, r)) = line.split_once(' ') else {
                continue;
            };
            if r.ends_with(":<none>") {
                let _ = docker(["rmi", id]).await;
            } else {
                refs.push(r.to_string());
            }
        }
        if !refs.is_empty() {
            docker(
                ["rmi"]
                    .into_iter()
                    .map(String::from)
                    .chain(refs.iter().cloned()),
            )
            .await?;
        }
        if proxy::caddy_state().await == "running" {
            proxy::delete_route(&req.app_id).await?;
        }
        Ok::<_, String>(json!({
            "app_id": req.app_id,
            "containers_removed": containers.len(),
            "images_removed": refs.len(),
        }))
    }
    .await;

    match result {
        Ok(v) => CommandOutcome::ok(v.to_string()),
        Err(e) => CommandOutcome::failed(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_ID: &str = "0b7c5a52-6a7e-4b8e-9d0b-3f2a1c4d5e6f";

    fn payload() -> Value {
        json!({
            "deployment_id": "9f8e7d6c-5b4a-4321-8765-0123456789ab",
            "app": {
                "id": APP_ID,
                "name": "mi-web",
                "type": "web",
                "port": 3000,
                "healthcheck_path": "/",
                "healthcheck_timeout_secs": 60,
                "domain": "App.Ejemplo.com",
                "start_command": null
            },
            "source": { "repo": "owner/name", "sha": "a".repeat(40), "token": "ghs_abc123" },
            "image": null,
            "build_env": { "NODE_ENV": "production" },
            "env": { "DATABASE_URL": "postgres://x" },
            "keep_images": 3
        })
    }

    fn validate(v: Value) -> Result<DeployRequest, String> {
        let mut r: DeployRequest = serde_json::from_value(v).map_err(|e| e.to_string())?;
        r.validate().map(|_| r)
    }

    #[test]
    fn valid_payload_passes_and_names_follow_contract() {
        let r = validate(payload()).unwrap();
        assert_eq!(r.app.domain.as_deref(), Some("app.ejemplo.com"));
        assert_eq!(r.container_name(), format!("sbapp-{APP_ID}-9f8e7d6c"));
        assert_eq!(r.app.kind, AppKind::Web);
    }

    #[test]
    fn rejects_bad_fields() {
        let cases: Vec<(&str, Value)> = vec![
            ("/app/name", json!("Mi-Web")),
            ("/app/name", json!("-web")),
            ("/app/name", json!("a".repeat(41))),
            ("/app/id", json!("not-a-uuid")),
            ("/app/id", json!(APP_ID.to_uppercase())),
            ("/deployment_id", json!("x")),
            ("/source/sha", json!("A".repeat(40))),
            ("/source/sha", json!("abc")),
            ("/source/repo", json!("owner/name/extra")),
            ("/source/repo", json!("owner/..")),
            ("/source/repo", json!("own er/name")),
            ("/source/token", json!("ghs_abc\nX-Evil: 1")),
            ("/app/domain", json!("app.ejemplo.com/x")),
            ("/app/domain", json!("localhost")),
            ("/app/domain", json!("-a.com")),
            ("/app/healthcheck_path", json!("health")),
            ("/app/healthcheck_path", json!("/a b")),
            ("/app/port", json!(0)),
            ("/app/type", json!("cron")), // sin schedule
            ("/app/type", json!("job")),
            ("/image", json!(format!("sbapp/{APP_ID}:abcdefabcdef"))),
        ];
        for (ptr, bad) in cases {
            let mut v = payload();
            *v.pointer_mut(ptr).unwrap() = bad.clone();
            assert!(validate(v).is_err(), "{ptr} = {bad} should be rejected");
        }

        for key in ["1ABC", "A-B", "A B", ""] {
            let mut v = payload();
            v["env"] = json!({ key: "x" });
            assert!(validate(v).is_err(), "env key {key:?} should be rejected");
        }
    }

    #[test]
    fn rollback_image_must_belong_to_app() {
        let mut v = payload();
        v["source"] = Value::Null;
        v["image"] = json!(format!("sbapp/{APP_ID}:abcdefabcdef"));
        assert!(validate(v.clone()).is_ok());
        v["image"] = json!("nginx:latest");
        assert!(validate(v.clone()).is_err());
        v["image"] = json!("sbapp/00000000-0000-0000-0000-000000000000:abcdefabcdef");
        assert!(validate(v.clone()).is_err());
        v["image"] = Value::Null;
        assert!(validate(v).is_err(), "neither source nor image");
    }

    #[test]
    fn port_selection_skips_used_ports() {
        assert_eq!(pick_port(|_| true), Some(20000));
        let used = [20000, 20001, 20003];
        assert_eq!(pick_port(|p| !used.contains(&p)), Some(20002));
        assert_eq!(pick_port(|_| false), None);
    }

    #[test]
    fn parses_docker_ports_column() {
        assert_eq!(parse_docker_ports("127.0.0.1:20001->3000/tcp"), vec![20001]);
        assert_eq!(
            parse_docker_ports("0.0.0.0:8080->80/tcp, :::8080->80/tcp, 5432/tcp"),
            vec![8080, 8080]
        );
        assert!(parse_docker_ports("").is_empty());
    }

    #[test]
    fn buildkit_limits_follow_spec() {
        assert_eq!(buildkit_limits(4, Some(16384)), ("2".to_string(), 4096));
        assert_eq!(buildkit_limits(3, Some(4096)), ("1.5".to_string(), 2048));
        assert_eq!(buildkit_limits(1, Some(1024)), ("0.5".to_string(), 1024));
        assert_eq!(buildkit_limits(2, None), ("1".to_string(), 1024));
    }

    #[test]
    fn image_retention_keeps_newest_and_current() {
        let imgs = vec![
            (
                "2026-10-01T00:00:00Z".to_string(),
                "sbapp/x:aaaaaaaaaaaa".to_string(),
            ),
            (
                "2026-10-04T00:00:00Z".to_string(),
                "sbapp/x:dddddddddddd".to_string(),
            ),
            (
                "2026-10-02T00:00:00Z".to_string(),
                "sbapp/x:bbbbbbbbbbbb".to_string(),
            ),
            (
                "2026-10-03T00:00:00Z".to_string(),
                "sbapp/x:cccccccccccc".to_string(),
            ),
        ];
        assert_eq!(
            images_to_remove(imgs.clone(), 2, "sbapp/x:dddddddddddd"),
            vec!["sbapp/x:bbbbbbbbbbbb", "sbapp/x:aaaaaaaaaaaa"]
        );
        // Vuelta atrás a una imagen vieja: no se borra aunque quede fuera de las `keep`.
        assert_eq!(
            images_to_remove(imgs.clone(), 1, "sbapp/x:aaaaaaaaaaaa"),
            vec!["sbapp/x:cccccccccccc", "sbapp/x:bbbbbbbbbbbb"]
        );
        assert!(images_to_remove(imgs, 5, "sbapp/x:aaaaaaaaaaaa").is_empty());
    }

    #[test]
    fn env_file_is_literal_and_rejects_newlines() {
        let env: BTreeMap<String, String> = [
            ("A", r#"x=y "quoted" $HOME 'z' \n"#),
            ("EMPTY", ""),
            ("PORT", "9999"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert_eq!(
            env_file_content(&env, true),
            "A=x=y \"quoted\" $HOME 'z' \\n\nEMPTY=\n"
        );
        assert_eq!(
            env_file_content(&env, false),
            "A=x=y \"quoted\" $HOME 'z' \\n\nEMPTY=\nPORT=9999\n"
        );

        for bad in ["a\nB=evil", "a\r"] {
            let mut v = payload();
            v["env"] = json!({ "K": bad });
            assert!(validate(v).is_err(), "{bad:?} should be rejected");
        }
        // En `build_env` (argumentos de railpack) un salto de línea no rompe nada.
        let mut v = payload();
        v["build_env"] = json!({ "K": "a\nb" });
        assert!(validate(v).is_ok());
    }

    #[test]
    fn env_file_is_private_and_removed() {
        let dir = WorkDir::new("env-test", "00000000-0000-0000-0000-000000000000").unwrap();
        let path = dir.0.join("app.env");
        write_private(&path, "K=V\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "K=V\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        let root = dir.0.clone();
        drop(dir);
        assert!(!root.exists());
    }

    #[test]
    fn cron_apps_need_a_valid_schedule() {
        let mut v = payload();
        v["app"]["type"] = json!("cron");
        v["app"]["domain"] = Value::Null;
        v["app"]["schedule"] = json!("*/5 * * * *");
        let r = validate(v.clone()).unwrap();
        assert_eq!(r.app.kind, AppKind::Cron);
        for bad in ["* * * *", "61 * * * *", ""] {
            v["app"]["schedule"] = json!(bad);
            assert!(validate(v.clone()).is_err(), "{bad:?} should be rejected");
        }
        v["app"]["schedule"] = Value::Null;
        assert!(validate(v).is_err());
    }

    #[test]
    fn cron_file_is_private_overwritten_and_found_when_due() {
        let dir = WorkDir::new("cron-test", "00000000-0000-0000-0000-000000000000").unwrap();
        let cron_dir = dir.0.join("cron");
        let mut job = CronJob {
            image: format!("sbapp/{APP_ID}:aaaaaaaaaaaa"),
            schedule: "30 2 * * *".into(),
            start_command: Some("node job.js".into()),
            env: [("K".to_string(), "V".to_string())].into(),
        };
        save_cron_job(&cron_dir, APP_ID, &job).unwrap();
        job.image = format!("sbapp/{APP_ID}:bbbbbbbbbbbb");
        save_cron_job(&cron_dir, APP_ID, &job).unwrap();

        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(cron_file(&cron_dir, APP_ID)).unwrap())
                .unwrap();
        assert_eq!(
            stored,
            json!({
                "image": format!("sbapp/{APP_ID}:bbbbbbbbbbbb"),
                "schedule": "30 2 * * *",
                "start_command": "node job.js",
                "env": { "K": "V" },
            })
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(cron_file(&cron_dir, APP_ID))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&cron_dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }

        // Ficheros ajenos y horarios rotos se ignoran.
        std::fs::write(cron_dir.join("notes.json"), "{}").unwrap();
        std::fs::write(
            cron_dir.join("11111111-1111-1111-1111-111111111111.json"),
            "{",
        )
        .unwrap();
        let at = |minute, hour| cron::Minute {
            minute,
            hour,
            dom: 15,
            month: 10,
            dow: 4,
        };
        assert_eq!(
            due_cron_apps(&cron_dir, at(30, 2)),
            vec![APP_ID.to_string()]
        );
        assert!(due_cron_apps(&cron_dir, at(31, 2)).is_empty());
        assert!(due_cron_apps(&dir.0.join("missing"), at(30, 2)).is_empty());
    }

    #[test]
    fn buildkit_prune_keeps_10_percent_min_2gb() {
        let df = "Filesystem     1024-blocks      Used Available Capacity Mounted on\n/dev/sda1        102400000  51200000  51200000      50% /\n";
        assert_eq!(parse_df_total_kb(df), Some(102_400_000));
        assert_eq!(parse_df_total_kb("garbage"), None);
        assert_eq!(prune_keep_mb(102_400_000), 10_000);
        assert_eq!(prune_keep_mb(10_485_760), 2048, "10 GB disk -> 2 GB floor");
    }

    #[tokio::test]
    async fn build_log_lines_are_truncated() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let p = Progress::new(tx);
        p.log(&"é".repeat(5000));
        let msg = rx.try_recv().unwrap();
        assert_eq!(msg.stage, "build_log");
        assert_eq!(msg.message.chars().count(), 2000);
    }
}
