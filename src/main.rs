//! CromoForge — punto de entrada.
//!
//! Este es el esqueleto que valida `sb-agent-core` desde un agente
//! greenfield: config, logging, wrapper de servicio, updater y status
//! socket ya vienen del crate compartido. Los despliegues llegan como
//! órdenes (`deploy.rs`); el loop principal solo lanza las apps cron.

mod commands;
mod config;
mod cron;
mod deploy;
mod proxy;

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
    logging::init(AGENT_NAME, &log_dir, "info");

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
    status::spawn_server(
        status_handle.clone(),
        status::default_socket_path(AGENT_NAME),
    );

    let command_registry = sb_agent_core::command_intake::CommandRegistry::new();
    commands::register(&command_registry);
    sb_agent_core::command_intake::spawn_server(
        command_registry,
        sb_agent_core::command_intake::default_socket_path(AGENT_NAME),
        sb_agent_core::command_intake::SigningContext {
            server_token: cfg.token.clone().unwrap_or_default(),
            target_agent: AGENT_NAME.to_string(),
        },
    );

    updater::start_daily_check(updater::UpdaterConfig::new(
        "securyblack",
        "cromo-forge",
        "cromoforge",
        version,
    ));

    status_handle.set_state("running");
    status_handle.set_details(serde_json::json!({
        "cron_scheduler": "running",
        "source": cfg.source,
    }));

    // Planificador de apps cron: despierta al empezar cada minuto (UTC).
    loop {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        // +200 ms para no caer en el 59,999 del minuto anterior.
        let wait = std::time::Duration::from_millis(60_000 - now_ms % 60_000 + 200);
        tokio::select! {
            _ = tokio::time::sleep(wait) => deploy::run_due_cron_jobs(cron::Minute::now()),
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
