# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2026-10-22

### Added
- **Deployments**: `deploy_app` takes `app.builder`: `railpack` (default, as before), `dockerfile` (built with BuildKit's `dockerfile.v0` inside `sb-buildkit`, same limits and cache) or `auto` (the Dockerfile if there is one, otherwise Railpack). `app.dockerfile_path` defaults to `Dockerfile`.
- **Deployments**: `app.root_dir` builds from a folder of the repo (monorepos). Paths are checked to stay inside the repo.
- **Deployments**: `app.install_command`, `app.build_command` and `app.start_command` override what Railpack detects; `start_command` also works with a Dockerfile.
- **Install**: `install.sh` installs Docker when it is missing (`--no-docker` skips it).

### Fixed
- **Deployments**: App and cron containers no longer grow their logs forever. With Docker's default `json-file` driver they are capped at 3 files of 10 MB; other drivers are left as configured. Applies to containers started from now on (next deploy).

## [0.4.0] - 2026-10-22

### Added
- **Containers**: `docker_get_logs` with `structured: true` returns `lines` (time, `stdout`/`stderr`, container) and `last_ts`. With `since` it drops the lines at that exact time, so the panel can ask only for what is new.
- **Containers**: `docker_get_logs` accepts `container_ids` (up to 20) and merges their logs by time, for a whole Compose project or app.
- **Containers**: `docker_container_stats` command — a `docker stats` snapshot of the running containers (CPU, memory, network, disk I/O, PIDs).
- **Containers**: `docker_list_containers` adds `compose_project`, `compose_service` and `sb_app` from the container labels.

### Fixed
- **Containers**: `docker_get_logs` without timestamps put all stderr after all stdout; lines are now always ordered by time. Sub-second times are compared numerically (Docker trims trailing zeros).

## [0.3.0] - 2026-10-20

### Added
- **Proxy**: Extra domains per app (`app.aliases` in `deploy_app`). Every domain gets its own certificate; aliases with `redirect` answer a 308 to the primary domain keeping path and query (`www` ↔ apex), the rest serve the app too. Still one Caddy route per app (`sbapp-<app>`).

### Fixed
- **Proxy**: Deploying a web app without a domain now removes its Caddy route, which used to keep pointing at the stopped container.

## [0.2.0] - 2026-10-16

### Added
- **Deployments**: `deploy_app` command — fetches a GitHub commit tarball, builds it with Railpack against a resource-capped BuildKit container (`sb-buildkit`), starts it as `sbapp-<app>-<deployment>`, healthchecks it (HTTP for `web`, 30 s stability for `worker`), switches the Caddy route and keeps the newest `keep_images` images. Streams `build_log` lines and stage heartbeats; rollbacks reuse an existing `sbapp/<app>` image. One deployment at a time per app.
- **Deployments**: `remove_app` command — removes an app's containers, images and Caddy route.
- **Proxy**: `proxy_status` and `proxy_install` commands — SB-managed Caddy (`sb-caddy`, host network, admin API on `127.0.0.1:2019`) with automatic HTTPS.
- **Deployments**: `cron` apps — the schedule (5 fields, UTC) lives in `/var/lib/cromoforge/cron/<app>.json`; each run starts `sbapp-<app>-run` and a run is skipped while the previous one is still going.
- **Deployments**: App secrets reach the container through a 0600 `--env-file` that is deleted right after `docker run`, never through argv.
- **Deployments**: BuildKit cache is pruned after each build to a size that depends on the disk, and its state lives in the named volume `sb-buildkit-state`.
- **Install**: `install.sh` installs Railpack.

### Security
- **Commands**: Verify S4 signed commands (ed25519 signature, expiry, replay protection) via `sb-agent-core` 0.4 `command_intake`. During the transition any verification failure (unsigned, invalid or expired) is accepted with a warning.

### Fixed
- **Deployments**: `remove_app` untags only the app's own images, so an image shared with another app no longer makes it fail and leave the Caddy route behind.

## [0.1.5] - 2026-10-14

### Security
- **Auto-update**: Only installs ed25519-signed releases (`sb-agent-core` 0.3).

## [0.1.4] - 2026-09-17

### Fixed
- **Postgres**: Allow hyphens and special characters in SQL object identifiers.
- **Performance**: Exclude internal streaming `COPY` commands from slow queries telemetry collection.

## [0.1.3] - 2026-09-17

### Added
- **Postgres**: Implement `postgres_action` command dispatch (canceling queries, terminating backends, running concurrent reindex and vacuum).
- **Postgres**: Enrich telemetry collection with active queries, installed extensions, and unused index metrics.

## [0.1.2] - 2026-09-17

### Added
- **Postgres**: Implement internal zero-port PostgreSQL connection testing and telemetry collection.

## [0.1.1] - 2026-09-16

### Added
- **Docker**: Implement active container lifecycle commands (`start`, `stop`, `restart`).
- **Logs**: Implement live container log extraction and tailing with interleaved stdout/stderr.

## [0.1.0] - 2026-09-13

### Added
- **Initial Release**: Container management and deployment agent for SecuryBlack.
- **Commands**: Unix domain socket command intake and `docker_list_containers` handler.
- **Core**: Built on top of `sb-agent-core` with structured logging and CLI.

[0.2.0]: https://github.com/SecuryBlack/cromo-forge/compare/v0.1.5...v0.2.0
[0.1.5]: https://github.com/SecuryBlack/cromo-forge/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/SecuryBlack/cromo-forge/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/SecuryBlack/cromo-forge/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/SecuryBlack/cromo-forge/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/SecuryBlack/cromo-forge/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/SecuryBlack/cromo-forge/releases/tag/v0.1.0
