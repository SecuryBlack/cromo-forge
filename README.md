# CromoForge

Rust agent for Docker container lifecycle/logs, PostgreSQL management and GitHub app deployments (early access) on Linux and Windows. See the [workspace wiki](../sb-wiki/Producto/Estado%20actual.md) for verified capability status.

## Use with SecuryBlack

SecuryBlack is a hosted panel for server metrics, security findings and alerts. This repository contains the native agent; the hosted panel is a separate part of the product.

[Watch the 42-second product demo](https://securyblack.com/en?utm_source=github&utm_medium=referral&utm_campaign=agent_readme&utm_content=cromo-forge#how-it-works) · [Open the hosted panel](https://app.securyblack.com) · [Installation documentation](https://securyblack.com/en/docs/installation)

Follow the agent-specific installation and configuration instructions below. Use the hosted panel to obtain connection settings for your server.

[![Website](https://img.shields.io/badge/Website-cromoforge.dev-6366F1?style=flat-square)](https://cromoforge.dev)
[![Ecosystem](https://img.shields.io/badge/Ecosystem-SecuryBlack-33E1BF?style=flat-square)](https://securyblack.com)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)](https://www.rust-lang.org/)

> **Part of the SecuryBlack ecosystem:**
> [OxiPulse (Metrics)](https://github.com/securyblack/oxi-pulse) · [FerroSentry (Security)](https://github.com/securyblack/ferro-sentry) · [CupraFlow (Networking/HA development)](https://github.com/securyblack/cupra-flow) · **CromoForge (Docker/PostgreSQL)** · [TitanVault (Backups)](https://github.com/securyblack/titan-vault) · [SecuryBlack Cloud](https://securyblack.com)

---

## 🏷️ Identity & Naming

- **Product Name:** CromoForge
- **Binary:** `cromoforge`
- **System Service:** `cromoforge` (Linux systemd) / `CromoForge` (Windows Service)

Part of the SecuryBlack native agent family: [OxiPulse](https://github.com/SecuryBlack/oxi-pulse) (metrics), [FerroSentry](https://github.com/SecuryBlack/ferro-sentry) (security & EDR), [CupraFlow](https://github.com/SecuryBlack/cupra-flow) (high availability & networking), [TitanVault](https://github.com/SecuryBlack/titan-vault) (backups & disaster recovery), and **CromoForge (deployments & GitOps)**.

---

## What it does (v0.2.0)

CromoForge receives commands from the SecuryBlack panel through the companion agent and runs them against the local Docker daemon. See [CHANGELOG.md](CHANGELOG.md) and [SECURITY.md](.github/SECURITY.md) for details.

- **GitHub app deploys (early access, Linux):** `deploy_app` downloads the commit tarball from GitHub, builds it with [Railpack](https://github.com/railwayapp/railpack) against a resource-capped BuildKit container (`sb-buildkit`), starts the new container, healthchecks it and only then switches traffic and removes the old one. Deploys can be triggered on every push from the panel.
- **App types:** `web` (HTTP healthcheck, routed through Caddy), `worker` (30 s stability check) and `cron` (5-field UTC schedule; a run is skipped while the previous one is still going).
- **Rollback:** redeploys an existing `sbapp/<app>` image without rebuilding; the newest `keep_images` images are kept.
- **Custom domains:** an SB-managed Caddy (`sb-caddy`) publishes web apps on their domain with automatic HTTPS from Let's Encrypt.
- **Secrets:** app environment variables reach the container through a `0600` env file that is deleted right after `docker run`, never through argv.
- **BuildKit cache cleanup:** the build cache is pruned after each build to a size based on free disk.
- **Docker:** list, start, stop and restart containers, and tail their logs.
- **PostgreSQL:** connection test, metrics (connections, size, cache hit, slow queries, bloat, unused indexes) and actions (cancel/terminate backends, `VACUUM`, `REINDEX CONCURRENTLY`).
- **Signed commands:** ed25519-signed commands from the SecuryBlack API are verified (expiry and replay protection). During the transition, unsigned or invalid commands are still accepted with a warning.

---

## 📦 Quickstart & Installation

### Linux — One-line Install
```bash
curl -fsSL https://install.cromoforge.dev | sudo bash -s -- --token <TOKEN>
```

Git deployments require Docker Engine with the buildx plugin; the installer does not install Docker. It does install a pinned, checksum-verified [Railpack](https://github.com/railwayapp/railpack) binary to `/usr/local/bin/railpack`.

### Windows — PowerShell (Administrator)
```powershell
irm https://install.cromoforge.dev | iex
```

Without `--token` / `-Token`, the installers ask for the token interactively. The SecuryBlack panel shows the install command with your server token.

---

## 🧱 Built on `sb-agent-core`

CromoForge is built directly upon [`sb-agent-core`](https://github.com/SecuryBlack/sb-agent-core), leveraging standardized shared modules for configuration, structured logging, service lifecycle management, self-updates, and UNIX/Named-Pipe status sockets (`/run/sb-agent/cromoforge.sock` on Linux, `\\.\pipe\sb-agent-cromoforge` on Windows).

---

## 🌐 SecuryBlack Open Source Ecosystem

CromoForge is the GitOps and continuous delivery pillar of the SecuryBlack modular agent suite:

| Agent | Core Focus | Official Website | Repository |
| :--- | :--- | :--- | :--- |
| **OxiPulse** | Server telemetry and OTLP metrics | [oxipulse.dev](https://oxipulse.dev) | [securyblack/oxi-pulse](https://github.com/securyblack/oxi-pulse) |
| **FerroSentry** | Lightweight EDR, auditd, brute-force mitigation & firewall | [ferrosentry.dev](https://ferrosentry.dev) | [securyblack/ferro-sentry](https://github.com/securyblack/ferro-sentry) |
| **CupraFlow** | Networking/HA development (not operationally verified) | [cupraflow.dev](https://cupraflow.dev) | [securyblack/cupra-flow](https://github.com/securyblack/cupra-flow) |
| **CromoForge** | Docker containers, logs & PostgreSQL management | [cromoforge.dev](https://cromoforge.dev) | [securyblack/cromo-forge](https://github.com/securyblack/cromo-forge) |
| **TitanVault** | Streaming backups (restore workflow not verified) | [titanvault.dev](https://titanvault.dev) | [securyblack/titan-vault](https://github.com/securyblack/titan-vault) |

All agents can be centrally managed with unified observability by connecting them to [SecuryBlack Cloud](https://securyblack.com).

---

## License

CromoForge is licensed under the [Apache License, Version 2.0](LICENSE).

## Maintainer context (workspace)

English is the primary language for this repository's public documentation. Shared product decisions, commercial terms and the backlog live in the [workspace wiki](../sb-wiki/Índice.md), [current state](../sb-wiki/Producto/Estado%20actual.md) and [product log](../sb-wiki/Producto/Bitácora.md). These links require `sb-wiki` as a sibling checkout. Keep agent-specific usage and technical contracts here; update shared decisions in the wiki.
