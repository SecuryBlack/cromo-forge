#!/usr/bin/env bash
# CromoForge — Install script
# Usage: curl -fsSL https://install.cromoforge.dev | bash -s -- --token <TOKEN>
#
# Git deployments need Docker Engine with the buildx plugin. If Docker is
# missing it is installed (--no-docker to skip).
set -euo pipefail

SB_AGENT_LABEL="cromoforge"
LIB_URL="https://raw.githubusercontent.com/securyblack/sb-agent-core/master/scripts/install-lib.sh"
LIB_TMP="$(mktemp)"
curl -fsSL "$LIB_URL" -o "$LIB_TMP" || { echo "ERROR: could not fetch install-lib.sh from sb-agent-core" >&2; exit 1; }
# shellcheck source=/dev/null
source "$LIB_TMP"
rm -f "$LIB_TMP"

# ─── Constants ────────────────────────────────────────────────────────────────
GITHUB_REPO="securyblack/cromo-forge"
BINARY_NAME="cromoforge"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/cromoforge"
CONFIG_FILE="${CONFIG_DIR}/config.toml"
# Railpack builds app images for git deployments. Pinned: bump on purpose.
RAILPACK_VERSION="v0.40.1"
RAILPACK_RELEASES="https://github.com/railwayapp/railpack/releases/download/${RAILPACK_VERSION}"

# ─── Argument parsing ─────────────────────────────────────────────────────────
TOKEN=""
SOURCE="local"
INSTALL_DOCKER=true

while [[ $# -gt 0 ]]; do
  case "$1" in
    --token)  TOKEN="$2";  shift 2 ;;
    --source) SOURCE="$2"; shift 2 ;;
    --no-docker) INSTALL_DOCKER=false; shift ;;
    *) sb_die "Unknown argument: $1" ;;
  esac
done

echo ""
sb_info "Deploy agent installer"
echo ""

sb_require_root
sb_require_cmds curl tar systemctl

TARGET="$(sb_detect_arch_linux)"
LATEST_VERSION="$(sb_fetch_latest_version "$GITHUB_REPO")"

ASSET_NAME="${BINARY_NAME}-${TARGET}.tar.gz"
DOWNLOAD_URL="https://github.com/${GITHUB_REPO}/releases/download/${LATEST_VERSION}/${ASSET_NAME}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

sb_download_and_verify "$DOWNLOAD_URL" "${TMP_DIR}/${ASSET_NAME}"
sb_install_binary "${TMP_DIR}/${ASSET_NAME}" "$BINARY_NAME" "$INSTALL_DIR"

# ─── Railpack ─────────────────────────────────────────────────────────────────
case "$TARGET" in
  x86_64-*)  RAILPACK_ARCH="x86_64" ;;
  aarch64-*) RAILPACK_ARCH="arm64"  ;;
esac
RAILPACK_ASSET="railpack-${RAILPACK_VERSION}-${RAILPACK_ARCH}-unknown-linux-musl.tar.gz"
RAILPACK_DIR="${TMP_DIR}/railpack"
mkdir -p "$RAILPACK_DIR"

sb_info "Downloading Railpack ${RAILPACK_VERSION}…"
curl -fsSL "${RAILPACK_RELEASES}/${RAILPACK_ASSET}" -o "${RAILPACK_DIR}/${RAILPACK_ASSET}" \
  || sb_die "Railpack download failed (${RAILPACK_ASSET})"
curl -fsSL "${RAILPACK_RELEASES}/checksums.txt" -o "${RAILPACK_DIR}/checksums.txt" \
  || sb_die "Railpack checksums.txt not found. Refusing to install an unverified binary."
# Only our asset's line: without it, sha256sum -c would have nothing to check.
grep -E "^[0-9a-f]{64}  ${RAILPACK_ASSET}\$" "${RAILPACK_DIR}/checksums.txt" > "${RAILPACK_DIR}/asset.sha256" \
  || sb_die "No checksum for ${RAILPACK_ASSET} in checksums.txt. Refusing to install."
(cd "$RAILPACK_DIR" && sha256sum -c asset.sha256 --quiet) || sb_die "Railpack checksum verification failed"
sb_success "Railpack checksum OK"
sb_install_binary "${RAILPACK_DIR}/${RAILPACK_ASSET}" "railpack" "$INSTALL_DIR"

# ─── Docker ───────────────────────────────────────────────────────────────────
# apt's own packages first: get.docker.com lags new Ubuntu releases and, when it
# fails there, leaves a broken apt source behind. It covers everything else.
install_docker() {
  local log="/tmp/cromoforge-docker.log"
  sb_info "Installing Docker…"
  if command -v apt-get &>/dev/null \
    && DEBIAN_FRONTEND=noninteractive apt-get update -qq >"$log" 2>&1 \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq docker.io docker-buildx >>"$log" 2>&1; then
    :
  elif ! curl -fsSL https://get.docker.com | sh >>"$log" 2>&1; then
    sb_die "Docker install failed (see ${log}). Install Docker Engine with buildx and run this again, or pass --no-docker."
  fi
  systemctl enable --now docker >/dev/null 2>&1 || true
  sb_success "$(docker --version)"
}

if ! command -v docker &>/dev/null; then
  if $INSTALL_DOCKER; then
    install_docker
  else
    sb_warn "Docker not found. Git deployments need Docker Engine with the buildx plugin."
  fi
elif ! docker buildx version &>/dev/null; then
  sb_warn "Docker buildx plugin not found. Git deployments need it."
fi

# ─── Configuration ────────────────────────────────────────────────────────────
mkdir -p "$CONFIG_DIR"
chmod 700 "$CONFIG_DIR"

if [[ -z "$TOKEN" ]]; then
  read -rsp "$(echo -e "${BOLD}  Auth token:${RESET} ")" TOKEN </dev/tty
  echo ""
fi
[[ -z "$TOKEN" ]] && sb_die "Token cannot be empty"

sb_info "Writing config to ${CONFIG_FILE}…"
cat > "$CONFIG_FILE" <<EOF
# CromoForge configuration
# Do not share this file — it contains your auth token.
token = "${TOKEN}"
source = "${SOURCE}"
poll_interval_secs = 30
EOF
chmod 600 "$CONFIG_FILE"
sb_success "Config written"

# ─── systemd service ──────────────────────────────────────────────────────────
sb_write_systemd_unit "cromoforge" "CromoForge deploy agent" "${INSTALL_DIR}/${BINARY_NAME}" "$CONFIG_DIR"
sb_enable_start_service "cromoforge"

echo ""
echo -e "${GREEN}${BOLD}  CromoForge ${LATEST_VERSION} installed successfully!${RESET}"
echo -e "  Status:  ${BOLD}systemctl status cromoforge${RESET}"
echo -e "  Logs:    ${BOLD}journalctl -fu cromoforge${RESET}"
echo -e "  Config:  ${BOLD}${CONFIG_FILE}${RESET}"
echo ""
