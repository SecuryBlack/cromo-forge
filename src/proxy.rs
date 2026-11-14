//! Proxy de SB: un Caddy en contenedor (`sb-caddy`, `--network host`) que
//! publica las apps web en su dominio con HTTPS automático. Las rutas se
//! gestionan solo por la API de administración (`127.0.0.1:2019`), una por
//! app con `@id = "sbapp-<app.id>"`, así que nunca escribimos un Caddyfile.
//!
//! Órdenes: `proxy_status` y `proxy_install` (ver el contrato de la Fase 1
//! en `sb-wiki/Tecnica/Fase 1 - Contrato de despliegues.md`).
//!
//! Si CupraFlow está instalado (existe su socket local), él es el dueño de
//! Caddy: rutas, `proxy_status` y `proxy_install` van por él y aquí no se
//! toca Caddy. Si no, todo sigue como antes. Contrato en
//! `sb-wiki/Tecnica/CupraFlow v1 - contrato.md`.

use crate::deploy::{docker, local_http};
use sb_agent_core::command_intake::{CommandOutcome, CommandResponse, ProgressSender};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

pub const CADDY_CONTAINER: &str = "sb-caddy";
// Imagen fija: una actualización de Caddy es un cambio de código, no un `pull` sorpresa.
const CADDY_IMAGE: &str = "caddy:2.10.0";
const ADMIN: &str = "http://127.0.0.1:2019";
/// Nombre del servidor HTTP de SB dentro de la config de Caddy.
const SERVER: &str = "sb";

/// "running" | "stopped" | "absent", tal cual lo pide `proxy_status`.
async fn caddy_state() -> &'static str {
    match docker(["inspect", "-f", "{{.State.Running}}", CADDY_CONTAINER]).await {
        Ok(s) if s == "true" => "running",
        Ok(_) => "stopped",
        Err(_) => "absent",
    }
}

fn route_id(app_id: &str) -> String {
    format!("sbapp-{app_id}")
}

/// Dominio extra de una app. `redirect` (por defecto) responde 308 al
/// dominio principal conservando ruta y query; si no, sirve la app igual.
#[derive(Debug, Deserialize)]
pub struct Alias {
    pub domain: String,
    #[serde(default = "yes")]
    pub redirect: bool,
}

fn yes() -> bool {
    true
}

/// Ruta de una app: sus hosts → reverse_proxy al puerto local del contenedor.
/// Los alias con redirección van en una subruta delante, dentro de la misma
/// ruta, así que la app sigue teniendo un solo `@id`. Todos los hosts están
/// en el `match` de fuera, que es de donde Caddy saca los certificados.
fn route_json(app_id: &str, domain: &str, aliases: &[Alias], host_port: u16) -> Value {
    let proxy = json!({
        "handler": "reverse_proxy",
        "upstreams": [{ "dial": format!("127.0.0.1:{host_port}") }]
    });
    let redirects: Vec<&str> = aliases
        .iter()
        .filter(|a| a.redirect)
        .map(|a| a.domain.as_str())
        .collect();
    let handle = if redirects.is_empty() {
        proxy
    } else {
        json!({
            "handler": "subroute",
            "routes": [
                {
                    "match": [{ "host": redirects }],
                    "handle": [{
                        "handler": "static_response",
                        "status_code": 308,
                        "headers": { "Location": [format!("https://{domain}{{http.request.uri}}")] }
                    }],
                    "terminal": true
                },
                { "handle": [proxy] }
            ]
        })
    };
    let hosts: Vec<&str> = std::iter::once(domain)
        .chain(aliases.iter().map(|a| a.domain.as_str()))
        .collect();
    json!({
        "@id": route_id(app_id),
        "match": [{ "host": hosts }],
        "handle": [handle],
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
async fn upsert_route(
    app_id: &str,
    domain: &str,
    aliases: &[Alias],
    host_port: u16,
) -> Result<(), String> {
    let client = local_http(Duration::from_secs(10))?;
    let body = route_json(app_id, domain, aliases, host_port).to_string();

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
async fn delete_route(app_id: &str) -> Result<(), String> {
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

/// Otros servidores de la app (`peers` de `deploy_app`) y el healthcheck que
/// CupraFlow comprueba en el local cuando los hay.
pub struct Peers<'a> {
    pub peers: &'a [String],
    pub health_path: &'a str,
}

/// Ruta en el formato de CupraFlow. Sin compañeros, Caddy acaba con el mismo
/// JSON que `route_json`.
fn cupraflow_route(
    app_id: &str,
    domain: &str,
    aliases: &[Alias],
    host_port: u16,
    peers: &Peers,
) -> Value {
    let hosts: Vec<&str> = std::iter::once(domain)
        .chain(aliases.iter().map(|a| a.domain.as_str()))
        .collect();
    let redirect_hosts: Vec<&str> = aliases
        .iter()
        .filter(|a| a.redirect)
        .map(|a| a.domain.as_str())
        .collect();
    let mut route = json!({
        "id": app_id,
        "hosts": hosts,
        "redirect_hosts": redirect_hosts,
        "upstreams": [{ "dial": format!("127.0.0.1:{host_port}") }],
    });
    if !peers.peers.is_empty() {
        route["peers"] = json!(peers.peers);
        route["health"] = json!({ "path": peers.health_path });
    }
    route
}

/// Resultado de una orden de CupraFlow: su `stdout` (JSON) o su `stderr`.
#[cfg_attr(not(unix), allow(dead_code))]
fn cupraflow_result(resp: CommandResponse) -> Result<Value, String> {
    if !resp.success {
        return Err(format!("CupraFlow: {}", resp.stderr.trim()));
    }
    serde_json::from_str(&resp.stdout).map_err(|e| format!("CupraFlow: bad reply: {e}"))
}

/// Manda la orden al socket local de CupraFlow. `None` si no está instalado
/// (no hay socket); con socket y sin respuesta es un error: no se vuelve a
/// Caddy por cuenta propia, para que las rutas no tengan dos dueños.
#[cfg(unix)]
async fn via_cupraflow(
    command_type: &str,
    payload: Value,
    timeout_secs: u32,
) -> Option<Result<Value, String>> {
    use sb_agent_core::command_intake::CommandEnvelope;
    use std::sync::atomic::{AtomicU64, Ordering};
    const AGENT: &str = "cupraflow-local";
    static SEQ: AtomicU64 = AtomicU64::new(0);

    if !cupraflow_installed() {
        return None;
    }
    // CupraFlow no repite un `command_id` ya visto: tiene que ser único.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let envelope = CommandEnvelope {
        command_id: format!(
            "cromoforge-{}-{nanos}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ),
        command_type: command_type.to_string(),
        payload,
        timeout_secs,
        ..Default::default()
    };
    let sent = tokio::task::spawn_blocking(move || {
        sb_agent_core::command_intake_client::send_command(AGENT, &envelope, |_| {})
    })
    .await;
    Some(match sent {
        Ok(Ok(resp)) => cupraflow_result(resp),
        Ok(Err(e)) => Err(format!("CupraFlow not responding: {e}")),
        Err(e) => Err(format!("CupraFlow not responding: {e}")),
    })
}

/// CupraFlow está instalado si existe su socket local (solo Linux).
pub fn cupraflow_installed() -> bool {
    #[cfg(unix)]
    {
        sb_agent_core::command_intake::default_socket_path("cupraflow-local").exists()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(not(unix))]
async fn via_cupraflow(_: &str, _: Value, _: u32) -> Option<Result<Value, String>> {
    None
}

/// Publica la ruta de la app. `Ok(false)`: Caddy no está en marcha y no se
/// ha tocado nada. Los compañeros solo los entiende CupraFlow (>= 0.3).
pub async fn set_route(
    app_id: &str,
    domain: &str,
    aliases: &[Alias],
    host_port: u16,
    peers: &Peers<'_>,
) -> Result<bool, String> {
    let route = cupraflow_route(app_id, domain, aliases, host_port, peers);
    if let Some(r) = via_cupraflow("route_set", json!({ "route": route }), 30).await {
        return r.map(|v| v["applied"] == true);
    }
    if !peers.peers.is_empty() {
        tracing::warn!(app_id, "peers need CupraFlow: route set without them");
    }
    if caddy_state().await != "running" {
        return Ok(false);
    }
    upsert_route(app_id, domain, aliases, host_port)
        .await
        .map(|_| true)
}

/// Quita la ruta de la app. `Ok(false)`: Caddy no está en marcha.
pub async fn remove_route(app_id: &str) -> Result<bool, String> {
    if let Some(r) = via_cupraflow("route_remove", json!({ "id": app_id }), 30).await {
        return r.map(|v| v["applied"] == true);
    }
    if caddy_state().await != "running" {
        return Ok(false);
    }
    delete_route(app_id).await.map(|_| true)
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
    let result = match via_cupraflow("proxy_status", json!({}), 30).await {
        Some(r) => r,
        None => status_json().await,
    };
    match result {
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
    let result = match via_cupraflow("proxy_install", json!({}), 300).await {
        Some(r) => r,
        None => install().await,
    };
    match result {
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
            &[],
            20001,
        );
        assert_eq!(r["@id"], "sbapp-11111111-2222-3333-4444-555555555555");
        assert_eq!(r["match"][0]["host"][0], "app.ejemplo.com");
        assert_eq!(r["handle"][0]["handler"], "reverse_proxy");
        assert_eq!(r["handle"][0]["upstreams"][0]["dial"], "127.0.0.1:20001");
    }

    #[test]
    fn route_json_with_aliases() {
        let aliases = [
            Alias {
                domain: "www.ejemplo.com".into(),
                redirect: true,
            },
            Alias {
                domain: "otro.com".into(),
                redirect: false,
            },
        ];
        let r = route_json("x", "ejemplo.com", &aliases, 20001);
        assert_eq!(
            r["match"][0]["host"],
            json!(["ejemplo.com", "www.ejemplo.com", "otro.com"])
        );
        let routes = &r["handle"][0]["routes"];
        assert_eq!(routes[0]["match"][0]["host"], json!(["www.ejemplo.com"]));
        assert_eq!(routes[0]["handle"][0]["status_code"], 308);
        assert_eq!(
            routes[0]["handle"][0]["headers"]["Location"][0],
            "https://ejemplo.com{http.request.uri}"
        );
        assert_eq!(
            routes[1]["handle"][0]["upstreams"][0]["dial"],
            "127.0.0.1:20001"
        );
        // Solo alias que sirven la app: sin subruta.
        let r = route_json("x", "ejemplo.com", &aliases[1..], 20001);
        assert_eq!(r["handle"][0]["handler"], "reverse_proxy");
    }

    #[test]
    fn cupraflow_route_matches_route_json() {
        let aliases = [
            Alias {
                domain: "www.ejemplo.com".into(),
                redirect: true,
            },
            Alias {
                domain: "otro.com".into(),
                redirect: false,
            },
        ];
        let none = Peers {
            peers: &[],
            health_path: "/",
        };
        let r = cupraflow_route("app-1", "ejemplo.com", &aliases, 20001, &none);
        assert_eq!(
            r,
            json!({
                "id": "app-1",
                "hosts": ["ejemplo.com", "www.ejemplo.com", "otro.com"],
                "redirect_hosts": ["www.ejemplo.com"],
                "upstreams": [{ "dial": "127.0.0.1:20001" }],
            })
        );
        // Mismos hosts que la ruta que se escribe directamente en Caddy.
        assert_eq!(
            route_json("app-1", "ejemplo.com", &aliases, 20001)["match"][0]["host"],
            r["hosts"]
        );
    }

    #[test]
    fn cupraflow_route_with_peers() {
        let peers = [
            "203.0.113.7".to_string(),
            "nodo2.ejemplo.com:8443".to_string(),
        ];
        let p = Peers {
            peers: &peers,
            health_path: "/health",
        };
        let r = cupraflow_route("app-1", "ejemplo.com", &[], 20001, &p);
        assert_eq!(r["peers"], json!(["203.0.113.7", "nodo2.ejemplo.com:8443"]));
        assert_eq!(r["health"], json!({ "path": "/health" }));
    }

    #[test]
    fn cupraflow_result_parses_reply() {
        let resp = |success: bool, stdout: &str, stderr: &str| CommandResponse {
            command_id: "x".into(),
            success,
            stdout: stdout.into(),
            stderr: stderr.into(),
            exit_code: 0,
            duration_ms: 1,
        };
        let ok = cupraflow_result(resp(true, r#"{"id":"a","applied":false}"#, "")).unwrap();
        assert_eq!(ok["applied"], false);
        assert_eq!(
            cupraflow_result(resp(
                false,
                "",
                "invalid route
"
            )),
            Err("CupraFlow: invalid route".into())
        );
        assert!(cupraflow_result(resp(true, "not json", "")).is_err());
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
