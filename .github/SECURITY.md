# Security Policy

## Supported Versions

SecuryBlack actively supports and maintains the latest minor release line of CromoForge with security updates and bug fixes.

| Version | Supported          | Status |
| ------- | ------------------ | ------ |
| 0.2.x   | :white_check_mark: | Active support |
| < 0.2.0 | :x:                | End of life |

---

## Privileges and network access

You should know exactly what an agent does before running its installer with `sudo`. CromoForge drives Docker and builds and runs code from your repositories, so it is a highly privileged agent. This section describes its current behaviour, including its limitations.

### Installation (Linux)

The install script needs root to:

- place the binary at `/usr/local/bin/cromoforge`;
- install a pinned [Railpack](https://github.com/railwayapp/railpack) binary at `/usr/local/bin/railpack`, verified against the `checksums.txt` of its GitHub release;
- write `/etc/cromoforge/config.toml` (directory mode `0700`, file mode `0600`, because it contains your server token);
- register and start the systemd unit `cromoforge.service`.

It downloads the CromoForge release from GitHub Releases. Docker Engine is not installed by the script.

### Runtime

- **The service runs as root and talks to the Docker daemon.** Access to Docker is equivalent to root on the host. The self-updater also needs to replace the binary in `/usr/local/bin`.
- **No inbound network listener of its own.** Commands arrive through a local Unix socket / named pipe from the SecuryBlack companion agent (Nexus), which relays them from the panel. CromoForge itself never connects to the SecuryBlack API.
- **Signed commands (transition).** Each command can carry an ed25519 signature from the SecuryBlack API, bound to your server token, the target agent, the payload and an expiry, with replay protection; the public keys are built into the binary. **During the transition, commands that are unsigned or fail verification are still accepted with a warning in the log.** A future release will reject them.
- **No shell.** External values (repository, commit, image, container names, SQL identifiers) are validated and passed as separate arguments to `docker`, `railpack` or `psql`, never through a shell.
- **It runs your code.** `deploy_app` builds the repository you selected in the panel and runs it as a container on this host. Web apps are published only on `127.0.0.1` and reached through Caddy.
- **Secrets.**
  - The GitHub installation token is kept in memory and only sent in the `Authorization` header to `api.github.com`.
  - App environment variables (`env`) reach `docker run` through a `0600` `--env-file` in a `0700` directory that is deleted right after the container starts, never through argv.
  - For `cron` apps, the schedule, image and `env` are stored in `/var/lib/cromoforge/cron/<app>.json` (mode `0600`) so each run can start the container.
  - Build-time variables (`build_env`) are passed to Railpack as `--env K=V` arguments, so they are visible in the process list while the build runs. Do not put secrets in build variables.
- **PostgreSQL actions.** `postgres_*` commands run `psql` (on the host or with `docker exec` in a container) to collect metrics and, on request, cancel or terminate backends, `VACUUM` or `REINDEX`.

### Outbound connections

| Destination | When | What is sent |
|---|---|---|
| `api.github.com` (redirects to `codeload.github.com`) | On `deploy_app` from a commit | A request for the commit tarball of the selected repository, with a short-lived GitHub App installation token as `Authorization: Bearer`. The token is not forwarded on the redirect to another host |
| Container registries (Docker Hub by default) | When BuildKit or Caddy is first set up, and during builds | Image pulls by the Docker daemon: `moby/buildkit:v0.23.2`, `caddy:2.10.0` (both pinned), and whatever base images Railpack's build plan needs. App containers always start with `--pull never` |
| Package registries used by your app | During builds | Whatever your build downloads (npm, PyPI, crates.io, …), from inside the BuildKit container |
| Let's Encrypt (ACME), via Caddy | Only after `proxy_install`, when an app has a custom domain | Caddy requests and renews a TLS certificate for that domain (HTTP-01 challenge on port 80). Certificates are stored in the `sb-caddy-data` volume |
| `api.github.com` / GitHub Releases | Once a day, and on the `update_now` command | A request for the latest CromoForge release; if there is a newer one, downloads it over HTTPS, checks its ed25519 signature against the SecuryBlack keys built into the binary and only then replaces it |

The Caddy admin API and app healthchecks only use `127.0.0.1`.

---

## Reporting a Vulnerability

We take the security of CromoForge and our users' infrastructure seriously. If you discover a security vulnerability in CromoForge, please report it through private channels.

**Please DO NOT open public GitHub issues, discussions, or pull requests for security vulnerabilities.**

Instead, please send a private email to:
**security@securyblack.com**

Please include in your report:
- A clear description of the vulnerability and potential security impact.
- Step-by-step reproduction instructions or a minimal proof-of-concept (PoC).
- Affected environment, architecture (`x86_64` or `aarch64`), Docker version, and CromoForge version.
- Any suggested mitigations or patches, if available.

### What to Expect

- **Acknowledgment**: We will acknowledge receipt of your vulnerability report within **48 hours**.
- **Assessment**: Our core security team will investigate, reproduce, and confirm the report within **5 business days**.
- **Remediation & Advisory**: Once confirmed, we will develop a patch, release an updated version, and coordinate responsible public disclosure, crediting your contribution unless you prefer to remain anonymous.
