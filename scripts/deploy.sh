#!/usr/bin/env bash
# omnish client deployer
#
# Distributes omnish client binaries, TLS cert, and auth token to remote machines.
#
# Usage:
#   bash deploy.sh [user@host1 user@host2 ...]
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
CLIENTS=("$@")

# ── Helpers ──────────────────────────────────────────────────────────────────

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mWARN:\033[0m %s\n' "$*"; }
error() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# ── Validate ─────────────────────────────────────────────────────────────────

[[ ${#CLIENTS[@]} -gt 0 ]] || { info "No clients specified, nothing to deploy."; exit 0; }
[[ -x "$BIN_DIR/omnish" ]] || error "omnish client binary not found at $BIN_DIR/omnish"

# ── Deploy to clients ────────────────────────────────────────────────────────

for client in "${CLIENTS[@]}"; do
    [[ -z "$client" ]] && continue

    info "Deploying to: ${client}..."
    REMOTE_HOME="~/.omnish"

    # Ensure remote directories exist and remove old binaries (running binaries can't be overwritten)
    ssh -n -o BatchMode=yes -o ConnectTimeout=5 "$client" \
        "mkdir -p ${REMOTE_HOME}/bin ${REMOTE_HOME}/tls && rm -f ${REMOTE_HOME}/bin/omnish ${REMOTE_HOME}/bin/omnish-plugin" 2>/dev/null \
        || { warn "Cannot connect to ${client}, skipping"; continue; }

    if scp -q -o BatchMode=yes "${BIN_DIR}/omnish" "${BIN_DIR}/omnish-plugin" "${client}:${REMOTE_HOME}/bin/" 2>/dev/null; then
        scp -q -o BatchMode=yes "${OMNISH_DIR}/tls/cert.pem" "${client}:${REMOTE_HOME}/tls/" 2>/dev/null || true
        scp -q -o BatchMode=yes "${OMNISH_DIR}/auth_token" "${client}:${REMOTE_HOME}/" 2>/dev/null || true
        info "Deployed to ${client}"
    else
        warn "Failed to deploy to ${client}"
    fi
done

info "Deploy complete"
