//! Config propia de CromoForge — deliberadamente mínima. Los despliegues
//! llegan como órdenes (`deploy.rs`), así que aquí solo vive lo necesario
//! para arrancar, firmar órdenes con el token y exponer el status socket.

use serde::Deserialize;

#[allow(dead_code)] // `api_url` y `version` no se leen todavía
#[derive(Debug, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub version: Option<String>,
    /// Token de servidor SecuryBlack (mismo token que usan los demás agentes).
    #[serde(default)]
    pub token: Option<String>,
    /// "securyblack" | "git" | "local" — de dónde sale el estado deseado.
    /// Solo existe como campo por ahora; ninguna fuente está implementada.
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default = "default_api_url")]
    pub api_url: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
}

fn default_source() -> String {
    "local".to_string()
}

fn default_api_url() -> String {
    "https://api.securyblack.com".to_string()
}

fn default_poll_interval() -> u64 {
    30
}
