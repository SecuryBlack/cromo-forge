#!/usr/bin/env bash
# CromoForge — Install script
# Usage: curl -fsSL https://install.cromoforge.dev | bash -s -- --token <TOKEN>
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

# ─── Argument parsing ─────────────────────────────────────────────────────────
TOKEN=""
SOURCE="local"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --token)  TOKEN="$2";  shift 2 ;;
    --source) SOURCE="$2"; shift 2 ;;
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
