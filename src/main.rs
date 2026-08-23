//! CromoForge — punto de entrada.
//!
//! Este es el esqueleto que valida `sb-agent-core` desde un agente
//! greenfield: config, logging, wrapper de servicio, updater y status
//! socket ya vienen del crate compartido. Lo que falta aquí a propósito es
//! el reconciliador de verdad (pull de imagen OCI, healthcheck, rollback) —
//! eso es la siguiente fase, una vez esté cerrado el contrato de estado
//! deseado (ver TODO.md). Por ahora el loop principal solo demuestra que el
//! ciclo de vida completo (arranca, carga config, expone status, apaga
//! limpio) funciona de punta a punta.

mod config;

use config::Config;
use sb_agent_core::{config as core_config, logging, service, status, updater};
use tokio::sync::oneshot;
use tracing::info;

const AGENT_NAME: &str = "cromoforge";
const SERVICE_DISPLAY_NAME: &str = "CromoForge";

async fn run(mut shutdown: oneshot::Receiver<()>) {
    let log_dir = core_config::default_config_path(AGENT_NAME)
        .parent()
        .expect("config path always has a parent")
        .to_path_buf();
    logging::init(AGENT_NAME, &log_dir);

    let version = env!("CARGO_PKG_VERSION");
    info!("CromoForge v{version} starting");

    let config_path = core_config::default_config_path(AGENT_NAME);
    let cfg: Config = core_config::load(&config_path).unwrap_or_else(|e| {
        tracing::error!("failed to load config: {e}");
        std::process::exit(1);
    });
    let _ = core_config::sync_version_field(&config_path, version);

    info!(source = %cfg.source, poll_interval_secs = cfg.poll_interval_secs, "config loaded");

    let status_handle = status::StatusHandle::new(AGENT_NAME, version);
    status::spawn_server(status_handle.clone(), status::default_socket_path(AGENT_NAME));

    updater::start_daily_check(updater::UpdaterConfig::new(
        "securyblack",
        "cromo-forge",
        "cromoforge",
        version,
    ));

    status_handle.set_state("running");
    status_handle.set_details(serde_json::json!({
        "reconciler": "not implemented yet",
        "source": cfg.source,
    }));

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(cfg.poll_interval_secs));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                // Placeholder: aquí va el reconciliador (leer estado deseado,
                // comparar con el observado, converger). Todavía no existe.
                tracing::debug!("tick — reconciler not implemented yet");
            }
            _ = &mut shutdown => {
                info!("shutdown signal received, stopping");
                status_handle.set_state("stopping");
                break;
            }
        }
    }
}

fn check_version_arg() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && (args[1] == "--version" || args[1] == "-V") {
        println!("cromoforge {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }
}

#[cfg(windows)]
fn main() {
    check_version_arg();
    match service::windows::run_service(SERVICE_DISPLAY_NAME, |rx| run(rx)) {
        Ok(_) => {}
        Err(e) if service::windows::is_not_started_by_scm(&e) => {
            service::run_console(run);
        }
        Err(e) => {
            eprintln!("[cromoforge] service error: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    check_version_arg();
    service::run_console(run);
}
