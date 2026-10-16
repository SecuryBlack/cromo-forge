//! Proxy de SB: un Caddy en contenedor (`sb-caddy`, `--network host`) que
//! publica las apps web en su dominio con HTTPS automático. Las rutas se
//! gestionan solo por la API de administración (`127.0.0.1:2019`), una por
//! app con `@id = "sbapp-<app.id>"`, así que nunca escribimos un Caddyfile.
//!
//! Órdenes: `proxy_status` y `proxy_install` (ver el contrato de la Fase 1
//! en `sb-wiki/Tecnica/Fase 1 - Contrato de despliegues.md`).

use crate::deploy::{docker, local_http};
use sb_agent_core::command_intake::{CommandOutcome, ProgressSender};
use serde_json::{json, Value};
use std::time::Duration;

pub const CADDY_CONTAINER: &str = "sb-caddy";
// Imagen fija: una actualización de Caddy es un cambio de código, no un `pull` sorpresa.
const CADDY_IMAGE: &str = "caddy:2.10.0";
const ADMIN: &str = "http://127.0.0.1:2019";
/// Nombre del servidor HTTP de SB dentro de la config de Caddy.
const SERVER: &str = "sb";

/// "running" | "stopped" | "absent", tal cual lo pide `proxy_status`.
pub async fn caddy_state() -> &'static str {
    match docker(["inspect", "-f", "{{.State.Running}}", CADDY_CONTAINER]).await {
        Ok(s) if s == "true" => "running",
        Ok(_) => "stopped",
        Err(_) => "absent",
    }
}

fn route_id(app_id: &str) -> String {
    format!("sbapp-{app_id}")
}

/// Ruta de una app: host exacto → reverse_proxy al puerto local del contenedor.
pub fn route_json(app_id: &str, domain: &str, host_port: u16) -> Value {
    json!({
        "@id": route_id(app_id),
        "match": [{ "host": [domain] }],
        "handle": [{
            "handler": "reverse_proxy",
            "upstreams": [{ "dial": format!("127.0.0.1:{host_port}") }]
        }],
        "terminal": true
    })
}

/// Config inicial: admin solo en loopback y un servidor vacío en :443. El
/// HTTPS automático de Caddy añade solo el servidor de :80 (redirección y
/// reto ACME HTTP-01) en cuanto aparece una ruta con `host`.
fn initial_config() -> Value {
    json!({
        "admin": { "listen": "127.0.0.1:2019" },
        "apps": { "http": { "servers": { SERVER: { "listen": [":443"], "routes": [] } } } }
    })
}

/// Crea o sustituye la ruta de la app. PATCH sobre `/id/...` reemplaza la
/// ruta existente en sitio; si no existe aún, se añade al final.
pub async fn upsert_route(app_id: &str, domain: &str, host_port: u16) -> Result<(), String> {
    let client = local_http(Duration::from_secs(10))?;
    let body = route_json(app_id, domain, host_port).to_string();

    let patched = client
        .patch(format!("{ADMIN}/id/{}", route_id(app_id)))
        .header("Content-Type", "application/json")
        .body(body.clone())
        .send()
        .await;
    if matches!(&patched, Ok(r) if r.status().is_success()) {
        return Ok(());
    }

    let resp = client
        .post(format!("{ADMIN}/config/apps/http/servers/{SERVER}/routes"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Caddy admin API unreachable: {e}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        Err(format!(
            "Caddy admin API returned {status}: {}",
            resp.text().await.unwrap_or_default()
        ))
    }
}

/// Borra la ruta de la app. Que no exista no es un error.
pub async fn delete_route(app_id: &str) -> Result<(), String> {
    let client = local_http(Duration::from_secs(10))?;
    let resp = client
        .delete(format!("{ADMIN}/id/{}", route_id(app_id)))
        .send()
        .await
        .map_err(|e| format!("Caddy admin API unreachable: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status.is_success() || status == 404 || text.contains("unknown object ID") {
        Ok(())
    } else {
        Err(format!("Caddy admin API returned {status}: {text}"))
    }
}

/// Procesos escuchando en :80 y :443 según `ss -ltnpH`. "unknown" si `ss`
/// no puede ver el proceso (sin privilegios).
fn parse_ss(output: &str) -> (Option<String>, Option<String>) {
    let mut port80 = None;
    let mut port443 = None;
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(local) = fields.get(3) else { continue };
        let slot = match local.rsplit(':').next() {
            Some("80") => &mut port80,
            Some("443") => &mut port443,
            _ => continue,
        };
        if slot.is_none() {
            let process = line
                .split("users:((\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or("unknown");
            *slot = Some(process.to_string());
        }
    }
    (port80, port443)
}

async fn listening_ports() -> Result<(Option<String>, Option<String>), String> {
    let out = tokio::process::Command::new("ss")
        .args(["-ltnpH"])
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("failed to execute ss: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ss failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(parse_ss(&String::from_utf8_lossy(&out.stdout)))
}

/// Estado de `proxy_status`. Un puerto ocupado por nuestro propio Caddy
/// cuenta como libre (`null`).
async fn status_json() -> Result<Value, String> {
    let caddy = caddy_state().await;
    let (p80, p443) = listening_ports().await?;
    let ours = |p: Option<String>| p.filter(|name| !(caddy == "running" && name == "caddy"));
    Ok(json!({ "caddy": caddy, "port80": ours(p80), "port443": ours(p443) }))
}

pub async fn proxy_status(_payload: Value, _progress: ProgressSender) -> CommandOutcome {
    match status_json().await {
        Ok(v) => CommandOutcome::ok(v.to_string()),
        Err(e) => CommandOutcome::failed(e),
    }
}

/// Espera a que la API de administración responda y, si Caddy arrancó sin
/// config (primera vez; luego `--resume` recupera la autoguardada), carga
/// la inicial.
async fn ensure_config() -> Result<(), String> {
    let client = local_http(Duration::from_secs(5))?;
    let mut last = String::new();
    for _ in 0..30 {
        // Antes del primer `/load` Caddy escucha en `localhost:2019` (resuelve también a 127.0.0.1).
        match client.get(format!("{ADMIN}/config/")).send().await {
            Ok(r) if r.status().is_success() => {
                let body = r.text().await.unwrap_or_default();
                if body.trim() != "null" {
                    return Ok(());
                }
                let resp = client
                    .post(format!("{ADMIN}/load"))
                    .header("Content-Type", "application/json")
                    .body(initial_config().to_string())
                    .send()
                    .await
                    .map_err(|e| format!("Caddy /load failed: {e}"))?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    return Err(format!(
                        "Caddy /load returned {status}: {}",
                        resp.text().await.unwrap_or_default()
                    ));
                }
                return Ok(());
            }
            Ok(r) => last = format!("HTTP {}", r.status()),
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(format!("Caddy admin API did not come up: {last}"))
}

async fn install() -> Result<Value, String> {
    match caddy_state().await {
        "running" => {}
        state => {
            let (p80, p443) = listening_ports().await?;
            if p80.is_some() || p443.is_some() {
                let busy: Vec<String> = [("80", p80), ("443", p443)]
                    .into_iter()
                    .filter_map(|(port, p)| p.map(|p| format!("port {port} in use by {p}")))
                    .collect();
                return Err(busy.join(", "));
            }
            if state == "stopped" {
                docker(["start", CADDY_CONTAINER]).await?;
            } else {
                docker([
                    "run",
                    "-d",
                    "--name",
                    CADDY_CONTAINER,
                    "--network",
                    "host",
                    "--restart",
                    "unless-stopped",
                    "-v",
                    "sb-caddy-data:/data",
                    // La config autoguardada vive en el mismo volumen que los certificados.
                    "-e",
                    "XDG_CONFIG_HOME=/data/config",
                    CADDY_IMAGE,
                    "caddy",
                    "run",
                    "--resume",
                ])
                .await?;
            }
        }
    }
    ensure_config().await?;
    status_json().await
}

pub async fn proxy_install(_payload: Value, _progress: ProgressSender) -> CommandOutcome {
    match install().await {
        Ok(v) => CommandOutcome::ok(v.to_string()),
        Err(e) => CommandOutcome::failed(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_json_shape() {
        let r = route_json(
            "11111111-2222-3333-4444-555555555555",
            "app.ejemplo.com",
            20001,
        );
        assert_eq!(r["@id"], "sbapp-11111111-2222-3333-4444-555555555555");
        assert_eq!(r["match"][0]["host"][0], "app.ejemplo.com");
        assert_eq!(r["handle"][0]["handler"], "reverse_proxy");
        assert_eq!(r["handle"][0]["upstreams"][0]["dial"], "127.0.0.1:20001");
    }

    #[test]
    fn initial_config_admin_on_loopback() {
        let c = initial_config();
        assert_eq!(c["admin"]["listen"], "127.0.0.1:2019");
        assert!(c["apps"]["http"]["servers"]["sb"]["routes"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn parse_ss_finds_80_and_443() {
        let out = "LISTEN 0 4096 0.0.0.0:80 0.0.0.0:* users:((\"nginx\",pid=12,fd=6))\n\
                   LISTEN 0 4096 [::]:443 [::]:* users:((\"caddy\",pid=99,fd=7))\n\
                   LISTEN 0 4096 127.0.0.1:8080 0.0.0.0:* users:((\"app\",pid=1,fd=3))\n\
                   LISTEN 0 4096 127.0.0.1:2080 0.0.0.0:*\n";
        assert_eq!(parse_ss(out), (Some("nginx".into()), Some("caddy".into())));
        assert_eq!(
            parse_ss("LISTEN 0 1 *:80 *:*\n"),
            (Some("unknown".into()), None)
        );
        assert_eq!(parse_ss(""), (None, None));
    }
}
