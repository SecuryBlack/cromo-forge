# CromoForge

Agente de despliegue, GitOps y entrega continua de contenedores OCI para servidores Linux y Windows, escrito en Rust. Reconcilia el contenedor en ejecución de un servidor contra un estado deseado — con rollback automático si el healthcheck falla.

[![Website](https://img.shields.io/badge/Website-cromoforge.dev-6366F1?style=flat-square)](https://cromoforge.dev)
[![Ecosystem](https://img.shields.io/badge/Ecosystem-SecuryBlack-33E1BF?style=flat-square)](https://securyblack.com)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)](https://www.rust-lang.org/)

> **Parte del ecosistema SecuryBlack:**
> [OxiPulse (Métricas)](https://github.com/securyblack/oxi-pulse) · [FerroSentry (Seguridad)](https://github.com/securyblack/ferro-sentry) · [CupraFlow (Alta Disponibilidad)](https://github.com/securyblack/cupra-flow) · **CromoForge (GitOps)** · [TitanVault (Backups)](https://github.com/securyblack/titan-vault) · [SecuryBlack Cloud](https://securyblack.com)

---

## 🏷️ Nombre

- **Nombre del producto:** CromoForge
- **Binario:** `cromoforge`
- **Servicio:** `cromoforge` (Linux) / `CromoForge` (Windows)

Cuarto miembro de la familia de agentes SB: [OxiPulse](https://github.com/SecuryBlack/oxi-pulse) (métricas), [FerroSentry](https://github.com/SecuryBlack/ferro-sentry) (seguridad), [CupraFlow](https://github.com/SecuryBlack/cupra-flow) (red/balanceo), [Nexus Agent](https://github.com/SecuryBlack/nexus-agent) (túnel/orquestación) y **CromoForge (despliegue)**.

---

## 🏗️ Arquitectura (decidida, ver TODO.md para el detalle)

- **Build:** el artefacto es una imagen OCI en un registry. En producción, CromoForge solo hace `pull` + `up` + healthcheck + rollback — nunca construye en el host del cliente.
- **Control:** reconciliador (estado deseado → observado → converge), con fuente pluggable (`securyblack` vía túnel Conduit, `git` puro GitOps, `local` con `cromoforge.toml`).
- **Secretos:** build args (no sensibles) separados de runtime secrets (sellados con X25519, SecuryBlack no puede leerlos).
- **v1:** una app, un contenedor, un servidor. Sin réplicas ni multi-nodo. Rollback automático es la pieza a clavar.

---

## 🧱 Por qué nace sobre `sb-agent-core`

CromoForge es el primer consumidor real del [crate compartido](https://github.com/SecuryBlack/sb-agent-core): config, logging, wrapper de servicio, auto-update y status socket vienen de ahí en vez de reimplementarse. Al ser greenfield, valida esa API antes de retrofitear los otros cuatro agentes — si algo del diseño de `sb-agent-core` no encaja, se descubre aquí, no en cinco repos a la vez.

`cromoforge status` / `cromoforge top` (pendiente de CLI) leerán el mismo status socket que expone el binario ahora mismo (`/run/sb-agent/cromoforge.sock` en Linux, `\\.\pipe\sb-agent-cromoforge` en Windows).

---

## 🚧 Lo que falta antes de que esto despliegue algo

1. Contrato de estado deseado (el manifiesto: mismo esquema para las 3 fuentes).
2. Mensajes de deploy sobre el Conduit Protocol — el `.proto` actual de Nexus Agent (`DeployCommand`, `GitPullAction`, `DockerBuildAction`, `DockerComposeAction`) asume build-en-host y Docker Compose, **incompatible** con lo decidido aquí. Se rediseña al mover el proto a `sb-conduit`.
3. El reconciliador en sí: cliente de Docker API, pull de imagen, healthcheck, rollback.
4. Modelo de datos en `api-internal` (reutilizar `agents` con `agent_type = "cromoforge"` + tablas de apps/deploys/secretos sellados).

Ver el desglose completo, con las decisiones ya cerradas y las abiertas, en [`TODO.md`](TODO.md).

---

## 🌐 Ecosistema Open Source de SecuryBlack

CromoForge es el pilar de GitOps y despliegues dentro de la suite de agentes modulares de SecuryBlack:

| Agente | Enfoque Principal | Web Oficial | Repositorio |
| :--- | :--- | :--- | :--- |
| **OxiPulse** | Telemetría, métricas OTLP y logs sin overhead | [oxipulse.dev](https://oxipulse.dev) | [securyblack/oxi-pulse](https://github.com/securyblack/oxi-pulse) |
| **FerroSentry** | EDR ligero, auditd, detección de fuerza bruta y firewall | [ferrosentry.dev](https://ferrosentry.dev) | [securyblack/ferro-sentry](https://github.com/securyblack/ferro-sentry) |
| **CupraFlow** | Alta disponibilidad, IP flotante VIP y balanceo de tráfico | [cupraflow.dev](https://cupraflow.dev) | [securyblack/cupra-flow](https://github.com/securyblack/cupra-flow) |
| **CromoForge** | Despliegues continuos, GitOps y gestión de contenedores | [cromoforge.dev](https://cromoforge.dev) | [securyblack/cromo-forge](https://github.com/securyblack/cromo-forge) |
| **TitanVault** | Copias de seguridad en streaming y recuperación ante desastres | [titanvault.dev](https://titanvault.dev) | [securyblack/titan-vault](https://github.com/securyblack/titan-vault) |

Todos los agentes pueden gestionarse de forma centralizada y visual conectándolos a [SecuryBlack Cloud](https://securyblack.com).

---

## License

CromoForge is licensed under the [Apache License, Version 2.0](LICENSE).
