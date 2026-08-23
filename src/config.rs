//! Config propia de CromoForge — deliberadamente mínima todavía. El
//! reconciliador real (pull de imagen OCI, healthcheck, rollback) no está
//! implementado; este `struct` solo sostiene lo necesario para que el
//! esqueleto arranque, se loguee y exponga su status socket. Ver
//! `TODO.md` para el contrato de estado deseado, que es lo próximo por
//! diseñar antes de que `source` deje de ser un placeholder.

use serde::Deserialize;

#[allow(dead_code)] // usados por el reconciliador real, todavía sin implementar
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
