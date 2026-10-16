---
name: Bug report
about: Create a report to help us improve CromoForge
title: '[BUG] '
labels: 'bug'
assignees: ''

---

**Describe the bug**
A clear and concise description of what the bug is.

**To Reproduce**
Steps to reproduce the behavior:
1. Agent configuration used (`config.toml` with the token redacted)
2. Command or deployment that failed (e.g. `deploy_app` for a web/worker/cron app, `proxy_install`, `postgres_action`, Docker container action)
3. Service state (`systemctl status cromoforge` or `Get-Service CromoForge`)
4. See error in logs (`journalctl -u cromoforge`, the deployment's build log, or Event Viewer)

**Expected behavior**
A clear and concise description of what you expected to happen.

**Environment (please complete the following information):**
 - CromoForge Version: [e.g. 0.2.0]
 - OS & Architecture: [e.g. Ubuntu 24.04 LTS x86_64, Windows Server 2022 AMD64]
 - Docker Version: [e.g. 27.3.1, output of `docker version`]
 - PostgreSQL Version (if relevant): [e.g. 16, host or container]

**Logs / Screenshots**
If applicable, attach relevant log snippets from `cromoforge`. Redact tokens, secrets and environment variables.

**Additional context**
Add any other context about the problem here (repository language/framework for build failures, custom domain for Caddy/HTTPS issues).
