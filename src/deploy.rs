//! Despliegues desde git (Fase 1): `deploy_app` y `remove_app`.
//!
//! El camino de `deploy_app` es fetch (tarball de GitHub) → build (Railpack
//! contra el BuildKit `sb-buildkit`) → start → healthcheck → switch (ruta de
//! Caddy) → cleanup. Formatos, nombres en Docker y etapas de progreso están
//! fijados en `sb-wiki/Tecnica/Fase 1 - Contrato de despliegues.md`; si algo
//! cambia aquí, cambia antes allí.
//!
//! Reglas que no se negocian:
//! - Todo argumento externo se valida antes de usarlo y va siempre como un
//!   elemento del vector de argumentos, nunca por un shell.
//! - El token de GitHub solo vive en memoria y en la cabecera `Authorization`.
//! - Los hijos largos se lanzan con `kill_on_drop`: si la orden caduca, el
//!   core suelta el future y el build/`docker` muere con él.

use crate::proxy;
use sb_agent_core::command_intake::{CommandOutcome, CommandProgress, ProgressSender};
use serde::Deserialize;
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

// ─────────────────────────────────────────────────────────────────────────────
// Payload y validación
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum AppKind {
    Web,
    Worker,
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
    s.len() == len && s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// `^[a-z0-9][a-z0-9-]{0,39}$`
fn is_app_name(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.chars().enumerate().all(|(i, c)| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || (i > 0 && c == '-')
        })
}

/// `owner/name` con el juego de caracteres de GitHub.
fn is_repo(s: &str) -> bool {
    let Some((owner, name)) = s.split_once('/') else { return false };
    (1..=39).contains(&owner.len())
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && (1..=100).contains(&name.len())
        && name != "."
        && name != ".."
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn is_domain(s: &str) -> bool {
    s.len() <= 253
        && s.contains('.')
        && s.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
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
    (1..=512).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
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
        Self { tx, heartbeat: Mutex::new(None) }
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
        return Err(if err.is_empty() { format!("docker exited with {}", out.status) } else { err });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn lines(s: &str) -> Vec<String> {
    s.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
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
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
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
    let tail = if err_tail.is_empty() { out_tail } else { err_tail };
    Err(format!("{program} exited with {status}:\n{}", Vec::from(tail).join("\n")))
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
    let mem = host_mem_mb.map(|m| (m / 2).min(4096)).unwrap_or(1024).max(1024);
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
    fn new(deployment_id: &str) -> Result<Self, String> {
        let base = if cfg!(unix) { PathBuf::from("/var/tmp") } else { std::env::temp_dir() };
        let dir = base.join(format!("cromoforge-build-{deployment_id}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
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
    let url = format!("https://api.github.com/repos/{}/tarball/{}", src.repo, src.sha);
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
        return Err(format!("GitHub returned {} for {}@{}", resp.status(), src.repo, src.sha));
    }

    let tarball = work.join("source.tar.gz");
    let mut file = tokio::fs::File::create(&tarball).await.map_err(|e| format!("write tarball: {e}"))?;
    let mut total = 0u64;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("GitHub download failed: {}", e.without_url()))? {
        total += chunk.len() as u64;
        if total > MAX_TARBALL_BYTES {
            return Err("source tarball exceeds 1 GB".into());
        }
        file.write_all(&chunk).await.map_err(|e| format!("write tarball: {e}"))?;
    }
    file.flush().await.map_err(|e| format!("write tarball: {e}"))?;
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
    let kb = meminfo.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.split_whitespace().next()?;
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
            let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
            let (cpus, mem_mb) = buildkit_limits(cpus, host_mem_mb());
            p.log(&format!("Creating {BUILDKIT_CONTAINER} (cpus {cpus}, memory {mem_mb} MB)"));
            docker([
                "run", "-d",
                "--name", BUILDKIT_CONTAINER,
                "--privileged",
                "--restart", "unless-stopped",
                "--cpus", &cpus,
                "--memory", &format!("{mem_mb}m"),
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
    cmd.arg("build").arg(dir).args(["--name", image, "--progress", "plain"]);
    for (k, v) in build_env {
        cmd.arg("--env").arg(format!("{k}={v}"));
    }
    if let Some(start) = start_command {
        cmd.args(["--start-cmd", start]);
    }
    cmd.env("BUILDKIT_HOST", format!("docker-container://{BUILDKIT_CONTAINER}"));
    run_streamed(cmd, "railpack", p).await
}

/// Puertos de host ya publicados por contenedores en marcha.
async fn docker_published_ports() -> Result<Vec<u16>, String> {
    let out = docker(["ps", "--format", "{{.Ports}}"]).await?;
    Ok(out.lines().flat_map(parse_docker_ports).collect())
}

async fn start_container(req: &DeployRequest, image: &str, container: &str, host_port: Option<u16>) -> Result<(), String> {
    let app = &req.app;
    let mut args: Vec<String> = vec![
        "run".into(), "-d".into(),
        "--pull".into(), "never".into(),
        "--name".into(), container.into(),
        "--restart".into(), "unless-stopped".into(),
        "--label".into(), format!("sb.app={}", app.id),
        "--label".into(), format!("sb.deployment={}", req.deployment_id),
    ];
    // ponytail: `-e K=V` deja los valores un instante en /proc/<pid>/cmdline
    // de `docker run`; pasarlos por el entorno del hijo permitiría a una app
    // inyectar LD_PRELOAD/DOCKER_HOST en el CLI de Docker, que es peor.
    for (k, v) in &req.env {
        if host_port.is_some() && k == "PORT" {
            continue;
        }
        args.push("-e".into());
        args.push(format!("{k}={v}"));
    }
    if let Some(hp) = host_port {
        args.push("-p".into());
        args.push(format!("127.0.0.1:{hp}:{}", app.port));
        args.push("-e".into());
        args.push(format!("PORT={}", app.port));
    }
    args.push(image.into());
    docker(args).await.map(|_| ())
}

async fn healthcheck(req: &DeployRequest, container: &str, host_port: Option<u16>) -> Result<(), String> {
    let Some(hp) = host_port else {
        // Worker: vivo 30 s después y sin reinicios.
        tokio::time::sleep(Duration::from_secs(30)).await;
        let state = docker(["inspect", "-f", "{{.State.Running}} {{.RestartCount}}", container]).await?;
        return if state == "true 0" {
            Ok(())
        } else {
            Err(format!("container not stable after 30 s (running/restarts: {state})"))
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
            return Err(format!("healthcheck GET {} failed after {timeout} s: {last}", req.app.healthcheck_path));
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
        for line in String::from_utf8_lossy(&out.stdout).lines().chain(String::from_utf8_lossy(&out.stderr).lines()) {
            p.log(line);
        }
    }
}

async fn container_names(app_id: &str) -> Result<Vec<String>, String> {
    Ok(lines(&docker(["ps", "-a", "--filter", &format!("label=sb.app={app_id}"), "--format", "{{.Names}}"]).await?))
}

/// Para y borra los contenedores viejos de la app y deja las `keep`
/// imágenes más recientes. Los fallos aquí no tumban el despliegue (la
/// versión nueva ya está sirviendo), solo se registran.
async fn cleanup(app_id: &str, new_container: &str, image: &str, keep: usize, p: &Progress) -> Result<(), String> {
    let old: Vec<String> = container_names(app_id).await?.into_iter().filter(|c| c != new_container).collect();
    if !old.is_empty() {
        let _ = docker(["stop", "-t", "10"].into_iter().map(String::from).chain(old.iter().cloned())).await;
        docker(["rm", "-f"].into_iter().map(String::from).chain(old)).await?;
    }

    let repo = format!("sbapp/{app_id}");
    let listed = docker(["images", &repo, "--format", "{{.ID}} {{.Repository}}:{{.Tag}}"]).await?;
    let mut refs = Vec::new();
    for line in lines(&listed) {
        let Some((id, r)) = line.split_once(' ') else { continue };
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
    let created = docker(["image", "inspect", "--format", "{{.Created}}"].into_iter().map(String::from).chain(refs.iter().cloned())).await?;
    let images: Vec<(String, String)> = lines(&created).into_iter().zip(refs).collect();
    for r in images_to_remove(images, keep, image) {
        if let Err(e) = docker(["rmi", &r]).await {
            p.log(&format!("could not remove {r}: {e}"));
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Órdenes
// ─────────────────────────────────────────────────────────────────────────────

/// Un candado por `app.id`: dos órdenes de la misma app van en fila.
fn app_lock(app_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    // ponytail: el mapa no se poda nunca; una entrada por app, crece con las apps del servidor.
    LOCKS.get_or_init(Default::default).lock().unwrap().entry(app_id.to_string()).or_default().clone()
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
            stdout: json!({ "deployment_id": req.deployment_id, "failed_stage": stage }).to_string(),
            stderr: err,
            exit_code: 1,
        },
    }
}

async fn run_deploy(req: &DeployRequest, p: &Progress) -> Result<(String, String, Option<u16>), StageError> {
    let app = &req.app;

    let image = match (&req.source, &req.image) {
        (Some(src), _) => {
            p.stage("fetch", "Downloading source code");
            let work = WorkDir::new(&req.deployment_id).map_err(at("fetch"))?;
            let dir = fetch_source(src, &work.0).await.map_err(at("fetch"))?;

            p.stage("build", "Building image");
            ensure_buildkit(p).await.map_err(at("build"))?;
            let image = format!("sbapp/{}:{}", app.id, &src.sha[..12]);
            railpack_build(&dir, &image, &req.build_env, app.start_command.as_deref(), p)
                .await
                .map_err(at("build"))?;
            image
        }
        (None, Some(image)) => image.clone(),
        (None, None) => unreachable!("validated"),
    };

    p.stage("start", "Starting container");
    docker(["image", "inspect", "--format", "{{.Id}}", &image])
        .await
        .map_err(|_| ("start", format!("image {image} not found on this host")))?;
    let host_port = match app.kind {
        AppKind::Worker => None,
        AppKind::Web => {
            let published = docker_published_ports().await.map_err(at("start"))?;
            let free = |port: u16| {
                !published.contains(&port) && std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
            };
            Some(pick_port(free).ok_or_else(|| ("start", "no free port in 20000-29999".to_string()))?)
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

    Ok((image, container, host_port))
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
        let containers = container_names(&req.app_id).await?;
        if !containers.is_empty() {
            docker(["rm", "-f"].into_iter().map(String::from).chain(containers.iter().cloned())).await?;
        }
        let images = lines(&docker(["images", &format!("sbapp/{}", req.app_id), "--format", "{{.ID}}"]).await?);
        if !images.is_empty() {
            docker(["rmi", "-f"].into_iter().map(String::from).chain(images.iter().cloned())).await?;
        }
        if proxy::caddy_state().await == "running" {
            proxy::delete_route(&req.app_id).await?;
        }
        Ok::<_, String>(json!({
            "app_id": req.app_id,
            "containers_removed": containers.len(),
            "images_removed": images.len(),
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
            ("/app/type", json!("cron")),
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
            ("2026-10-01T00:00:00Z".to_string(), "sbapp/x:aaaaaaaaaaaa".to_string()),
            ("2026-10-04T00:00:00Z".to_string(), "sbapp/x:dddddddddddd".to_string()),
            ("2026-10-02T00:00:00Z".to_string(), "sbapp/x:bbbbbbbbbbbb".to_string()),
            ("2026-10-03T00:00:00Z".to_string(), "sbapp/x:cccccccccccc".to_string()),
        ];
        assert_eq!(images_to_remove(imgs.clone(), 2, "sbapp/x:dddddddddddd"), vec!["sbapp/x:bbbbbbbbbbbb", "sbapp/x:aaaaaaaaaaaa"]);
        // Vuelta atrás a una imagen vieja: no se borra aunque quede fuera de las `keep`.
        assert_eq!(images_to_remove(imgs.clone(), 1, "sbapp/x:aaaaaaaaaaaa"), vec!["sbapp/x:cccccccccccc", "sbapp/x:bbbbbbbbbbbb"]);
        assert!(images_to_remove(imgs, 5, "sbapp/x:aaaaaaaaaaaa").is_empty());
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
