# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/SecuryBlack/cromo-forge/compare/v0.1.4...HEAD
[0.1.4]: https://github.com/SecuryBlack/cromo-forge/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/SecuryBlack/cromo-forge/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/SecuryBlack/cromo-forge/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/SecuryBlack/cromo-forge/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/SecuryBlack/cromo-forge/releases/tag/v0.1.0
