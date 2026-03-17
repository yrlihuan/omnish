#!/usr/bin/env bash
# omnish auto-updater
#
# Checks GitHub for the latest release, downloads and installs if newer,
# then distributes updated files to client machines.
#
# Usage:
#   bash update.sh [user@host1 user@host2 ...]
#
# When called by the daemon's auto_update task, client hosts from
# [tasks.auto_update] clients config are passed as arguments.
# Can also be run manually.
#
# Environment variables:
#   OMNISH_HOME   Override the default directory (~/.omnish)

set -euo pipefail

OMNISH_DIR="${OMNISH_HOME:-${HOME}/.omnish}"
BIN_DIR="${OMNISH_DIR}/bin"
REPO="yrlihuan/omnish"
CLIENTS=("$@")

# ── Helpers ──────────────────────────────────────────────────────────────────

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mWARN:\033[0m %s\n' "$*"; }
error() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# ── Platform detection ───────────────────────────────────────────────────────

ARCH=$(uname -m)
case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *) error "Unsupported architecture: $ARCH" ;;
esac

# ── Version check ────────────────────────────────────────────────────────────

CURRENT_VERSION=""
if [[ -x "$BIN_DIR/omnish-daemon" ]]; then
    CURRENT_VERSION=$("$BIN_DIR/omnish-daemon" --version 2>/dev/null | awk '{print $2}' || echo "")
fi

LATEST_TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | sed 's/.*"tag_name": *"//;s/".*//') || true
[[ -n "$LATEST_TAG" ]] || error "Could not determine latest version"
LATEST_VERSION="${LATEST_TAG#v}"

if [[ "$CURRENT_VERSION" == "$LATEST_VERSION" ]]; then
    info "Already up to date (v${CURRENT_VERSION})"
    exit 0
fi

info "Update available: v${CURRENT_VERSION:-unknown} -> v${LATEST_VERSION}"

# ── Download & Install ───────────────────────────────────────────────────────

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

TAR_URL="https://github.com/${REPO}/releases/download/${LATEST_TAG}/omnish-${LATEST_VERSION}-linux-${ARCH}.tar.gz"
info "Downloading ${TAR_URL}..."
curl -fSL "$TAR_URL" -o "$TMPDIR/omnish.tar.gz" || error "Download failed"
tar -xzf "$TMPDIR/omnish.tar.gz" -C "$TMPDIR"

EXTRACTED=$(find "$TMPDIR" -maxdepth 1 -type d -name 'omnish-*' | head -1)
[[ -d "$EXTRACTED" ]] || error "Unexpected archive layout"

# Install binaries
info "Installing to ${BIN_DIR}..."
cp "$EXTRACTED/bin/"* "$BIN_DIR/"
chmod 755 "$BIN_DIR"/*

# Install assets
if [[ -d "$EXTRACTED/assets" ]]; then
    mkdir -p "$OMNISH_DIR/plugins/builtin" "$OMNISH_DIR/prompts"
    { echo "// This file is for demonstration only. Use tool.override.json to customize."; cat "$EXTRACTED/assets/plugins/builtin/tool.json"; } > "$OMNISH_DIR/plugins/builtin/tool.json"
    { echo "// This file is for demonstration only. Use chat.override.json to customize."; cat "$EXTRACTED/assets/prompts/chat.json"; } > "$OMNISH_DIR/prompts/chat.json"
    cp "$EXTRACTED/assets/update.sh" "$OMNISH_DIR/"
    chmod 755 "$OMNISH_DIR/update.sh"
fi

info "Server updated to v${LATEST_VERSION}"

# ── Client distribution ─────────────────────────────────────────────────────

for client in "${CLIENTS[@]}"; do
    [[ -z "$client" ]] && continue

    info "Updating client: ${client}..."
    REMOTE_HOME="~/.omnish"

    if scp -q "${BIN_DIR}/omnish" "${BIN_DIR}/omnish-plugin" "${client}:${REMOTE_HOME}/bin/" 2>/dev/null; then
        scp -q "${OMNISH_DIR}/tls/cert.pem" "${client}:${REMOTE_HOME}/tls/" 2>/dev/null || true
        scp -q "${OMNISH_DIR}/auth_token" "${client}:${REMOTE_HOME}/" 2>/dev/null || true
        info "Updated ${client}"
    else
        warn "Failed to update ${client}"
    fi
done

info "Update complete (v${LATEST_VERSION})"
