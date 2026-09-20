# CromoForge

Autonomous GitOps, continuous delivery, and container lifecycle agent for Linux and Windows servers, written in pure Rust. Reconciles running containers against a desired state — with automated instant rollback if health checks fail.

[![Website](https://img.shields.io/badge/Website-cromoforge.dev-6366F1?style=flat-square)](https://cromoforge.dev)
[![Ecosystem](https://img.shields.io/badge/Ecosystem-SecuryBlack-33E1BF?style=flat-square)](https://securyblack.com)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)](https://www.rust-lang.org/)

> **Part of the SecuryBlack ecosystem:**
> [OxiPulse (Metrics)](https://github.com/securyblack/oxi-pulse) · [FerroSentry (Security)](https://github.com/securyblack/ferro-sentry) · [CupraFlow (High Availability)](https://github.com/securyblack/cupra-flow) · **CromoForge (GitOps)** · [TitanVault (Backups)](https://github.com/securyblack/titan-vault) · [SecuryBlack Cloud](https://securyblack.com)

---

## 🏷️ Identity & Naming

- **Product Name:** CromoForge
- **Binary:** `cromoforge`
- **System Service:** `cromoforge` (Linux systemd) / `CromoForge` (Windows Service)

Part of the SecuryBlack native agent family: [OxiPulse](https://github.com/SecuryBlack/oxi-pulse) (metrics), [FerroSentry](https://github.com/SecuryBlack/ferro-sentry) (security & EDR), [CupraFlow](https://github.com/SecuryBlack/cupra-flow) (high availability & networking), [TitanVault](https://github.com/SecuryBlack/titan-vault) (backups & disaster recovery), and **CromoForge (deployments & GitOps)**.

---

## 🏗️ Architecture & Design Principles

- **Registry-First Delivery:** Production deployments perform `pull` + `up` + health check + rollback. Container images are built in CI pipelines and fetched from registries rather than built on production host servers.
- **Reconciliation Engine:** State convergence loop (desired state → observed state → convergence) supporting pluggable sources: `securyblack` (via Conduit tunnel), `git` (declarative GitOps), and `local` (`cromoforge.toml`).
- **Sealed Secrets:** Build arguments separated from runtime secrets sealed with X25519 asymmetric cryptography.
- **Zero-Downtime Atomic Rollout:** Starts the new container instance, verifies health check probes, seamlessly shifts traffic, and stops the prior version. If probes fail, instant rollback restores the previous version immediately.

---

## 📦 Quickstart & Installation

### Linux — One-line Install
```bash
curl -fsSL https://install.cromoforge.dev | sudo bash
```

### Windows — PowerShell (Administrator)
```powershell
irm https://install.cromoforge.dev | iex
```

### Interactive Cockpit (TUI)
```bash
cromoforge tui
```

---

## 🧱 Built on `sb-agent-core`

CromoForge is built directly upon [`sb-agent-core`](https://github.com/SecuryBlack/sb-agent-core), leveraging standardized shared modules for configuration, structured logging, service lifecycle management, self-updates, and UNIX/Named-Pipe status sockets (`/run/sb-agent/cromoforge.sock` on Linux, `\\.\pipe\sb-agent-cromoforge` on Windows).

---

## 🌐 SecuryBlack Open Source Ecosystem

CromoForge is the GitOps and continuous delivery pillar of the SecuryBlack modular agent suite:

| Agent | Core Focus | Official Website | Repository |
| :--- | :--- | :--- | :--- |
| **OxiPulse** | Telemetry, OTLP metrics, and zero-overhead vital signs | [oxipulse.dev](https://oxipulse.dev) | [securyblack/oxi-pulse](https://github.com/securyblack/oxi-pulse) |
| **FerroSentry** | Lightweight EDR, auditd, brute-force mitigation & firewall | [ferrosentry.dev](https://ferrosentry.dev) | [securyblack/ferro-sentry](https://github.com/securyblack/ferro-sentry) |
| **CupraFlow** | High availability, floating VIP failover & traffic balancing | [cupraflow.dev](https://cupraflow.dev) | [securyblack/cupra-flow](https://github.com/securyblack/cupra-flow) |
| **CromoForge** | Continuous delivery, GitOps & container management | [cromoforge.dev](https://cromoforge.dev) | [securyblack/cromo-forge](https://github.com/securyblack/cromo-forge) |
| **TitanVault** | Zero-disk streaming backups & disaster recovery | [titanvault.dev](https://titanvault.dev) | [securyblack/titan-vault](https://github.com/securyblack/titan-vault) |

All agents can be centrally managed with unified observability by connecting them to [SecuryBlack Cloud](https://securyblack.com).

---

## License

CromoForge is licensed under the [Apache License, Version 2.0](LICENSE).
