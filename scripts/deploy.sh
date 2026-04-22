#!/usr/bin/env bash
# omnish client deployer
#
# Distributes omnish client binaries, TLS cert, and auth token to remote machines.
# If OMNISH_DAEMON_ADDR is set and the remote has no client.toml yet, also
# writes a minimal client.toml pointing at a reachable daemon address.
#
# Usage:
#   bash deploy.sh [user@host1 user@host2 ...]
#
# When called by the daemon's auto_update task, client hosts from
# [tasks.auto_update] clients config are passed as arguments.
# Can also be run manually.
#
# Environment variables:
#   OMNISH_HOME         Override the default directory (~/.omnish)
#   OMNISH_DAEMON_ADDR  Daemon listen address (e.g. "tcp://0.0.0.0:9500" or
#                       "/path/to/omnish.sock"). When set, deploy.sh probes
#                       reachable daemon addresses from the remote's POV and
#                       writes a minimal client.toml. Unset = skip config step.
#
# Status markers (stderr, consumed by daemon deploy.rs):
#   OMNISH_DEPLOY_STATUS: probe_failed <target>
#   OMNISH_DEPLOY_STATUS: unix_socket  <target>

set -euo pipefail

OMNISH_DIR="${OMNISH_HOME:-${HOME}/.omnish}"
BIN_DIR="${OMNISH_DIR}/bin"
CLIENTS=("$@")

DAEMON_ADDR_RAW="${OMNISH_DAEMON_ADDR:-}"
DAEMON_KIND="none"   # none | unix | tcp
DAEMON_HOST=""
DAEMON_PORT=""
CANDIDATES=()

# ── Helpers ──────────────────────────────────────────────────────────────────

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mWARN:\033[0m %s\n' "$*" >&2; }
error() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }
status_marker() { printf 'OMNISH_DEPLOY_STATUS: %s\n' "$*" >&2; }

FAILED_COUNT=0

parse_daemon_addr() {
    [[ -z "$DAEMON_ADDR_RAW" ]] && return 0
    # Mirror transport::parse_addr / config_schema::is_tcp_listen semantics:
    # TCP if the value either has an explicit tcp:// scheme, or has no scheme,
    # does not start with '/' or '.', and contains a ':'. Everything else is
    # treated as a Unix socket path.
    local value="$DAEMON_ADDR_RAW"
    local had_scheme=0
    if [[ "$value" == tcp://* ]]; then
        value="${value#tcp://}"
        had_scheme=1
    fi
    if [[ $had_scheme -eq 0 ]] && [[ "$value" == /* || "$value" == .* || "$value" != *:* ]]; then
        DAEMON_KIND="unix"
        return 0
    fi
    local hostport="${value%%/*}"
    if [[ "$hostport" == \[*\]:* ]]; then
        DAEMON_HOST="${hostport#[}"
        DAEMON_HOST="${DAEMON_HOST%%]*}"
        DAEMON_PORT="${hostport##*:}"
    else
        DAEMON_HOST="${hostport%:*}"
        DAEMON_PORT="${hostport##*:}"
    fi
    DAEMON_KIND="tcp"
}

# Populate CANDIDATES with addresses the remote might use to reach this host.
collect_candidates() {
    CANDIDATES=()
    [[ "$DAEMON_KIND" != "tcp" ]] && return 0

    case "$DAEMON_HOST" in
        ""|"0.0.0.0"|"::"|"*")
            # Wildcard bind: enumerate all non-loopback local addresses.
            if command -v hostname >/dev/null 2>&1 && hostname -I >/dev/null 2>&1; then
                read -r -a CANDIDATES < <(hostname -I 2>/dev/null || true)
            elif command -v ip >/dev/null 2>&1; then
                mapfile -t CANDIDATES < <(
                    ip -o addr show 2>/dev/null \
                        | awk '/inet /{print $4}' \
                        | awk -F/ '{print $1}' \
                        | grep -Ev '^(127\.|169\.254\.)' || true
                )
            fi
            if command -v hostname >/dev/null 2>&1; then
                local fqdn
                fqdn="$(hostname -f 2>/dev/null || true)"
                if [[ -n "$fqdn" && "$fqdn" != "localhost" ]]; then
                    CANDIDATES+=("$fqdn")
                fi
            fi
            ;;
        *)
            CANDIDATES=("$DAEMON_HOST")
            ;;
    esac
}

# Probe candidates from the remote's point of view. Echoes the first
# reachable address, or empty string if none.
probe_candidate() {
    local target="$1"
    local cands_str="${CANDIDATES[*]}"
    [[ -z "$cands_str" ]] && return 0
    # bash /dev/tcp is present on the overwhelming majority of Linux/BSD
    # remotes. timeout(1) bounds each attempt; total bounded by len*2s.
    ssh -n -o BatchMode=yes -o ConnectTimeout=5 "$target" \
        "for a in $cands_str; do \
            timeout 2 bash -c \"exec 3<>/dev/tcp/\$a/$DAEMON_PORT\" 2>/dev/null \
                && { echo \$a; exec 3<&- 2>/dev/null || true; break; }; \
         done" 2>/dev/null
}

remote_has_config() {
    local target="$1"
    ssh -n -o BatchMode=yes -o ConnectTimeout=5 "$target" \
        "test -e ~/.omnish/client.toml" 2>/dev/null
}

write_remote_config() {
    local target="$1" addr="$2" toml_addr
    # Bracket bare IPv6 literals for the TOML url. A bare v4 / hostname has
    # no colons, so this only triggers for v6.
    if [[ "$addr" == *:* && "$addr" != \[* ]]; then
        toml_addr="tcp://[$addr]:$DAEMON_PORT"
    else
        toml_addr="tcp://$addr:$DAEMON_PORT"
    fi
    # No -n here: the heredoc IS ssh's stdin, which gets forwarded to the
    # remote `cat`. Adding -n would redirect stdin from /dev/null and write
    # an empty client.toml.
    # `client_addr` records the ssh target we used so the client can echo
    # it back to the daemon (via the client_addr probe) and the daemon's
    # "Clients" menu can re-deploy using a known-working target instead of
    # guessing from gethostname().
    ssh -o BatchMode=yes -o ConnectTimeout=5 "$target" \
        "cat > ~/.omnish/client.toml" <<EOF
# Generated by deploy.sh on first deploy.
daemon_addr = "$toml_addr"
client_addr = "$target"
EOF
}

configure_remote() {
    local target="$1"
    if [[ "$DAEMON_KIND" == "none" ]]; then
        return 0   # no daemon addr provided, skip entirely
    fi
    if [[ "$DAEMON_KIND" == "unix" ]]; then
        status_marker "unix_socket $target"
        return 0
    fi
    if remote_has_config "$target"; then
        return 0   # preserve user-managed config
    fi
    local winner
    winner="$(probe_candidate "$target" || true)"
    winner="${winner//$'\r'/}"
    winner="${winner%%$'\n'*}"
    if [[ -z "$winner" ]]; then
        status_marker "probe_failed $target"
        return 0
    fi
    if ! write_remote_config "$target" "$winner"; then
        status_marker "probe_failed $target"
    fi
}

# ── Validate ─────────────────────────────────────────────────────────────────

[[ ${#CLIENTS[@]} -gt 0 ]] || { info "No clients specified, nothing to deploy."; exit 0; }

UPDATES_DIR="${OMNISH_DIR}/updates"
[[ -d "$UPDATES_DIR" ]] || error "updates directory not found at $UPDATES_DIR"

parse_daemon_addr
collect_candidates

if [[ "$DAEMON_KIND" == "none" ]]; then
    warn "OMNISH_DAEMON_ADDR is not set; client.toml will not be generated on remotes."
    warn "To auto-configure it, export OMNISH_DAEMON_ADDR to the daemon's listen_addr"
    warn "  (e.g. export OMNISH_DAEMON_ADDR=tcp://your-host:9500) and re-run."
fi

# ── Deploy helpers ───────────────────────────────────────────────────────────

# Detect remote platform via ssh. Echoes "<os>-<arch>" (e.g. "linux-x86_64",
# "macos-aarch64") on success, empty string on failure. Normalizes the raw
# `uname` output into the same tag the daemon uses for update packages.
detect_remote_platform() {
    local target="$1"
    local raw
    raw="$(ssh -n -o BatchMode=yes -o ConnectTimeout=5 "$target" 'uname -s; uname -m' 2>/dev/null)" || return 1
    local os="$(echo "$raw" | sed -n '1p')"
    local arch="$(echo "$raw" | sed -n '2p')"
    case "$os" in
        Linux)  os="linux" ;;
        Darwin) os="macos" ;;
        *)      return 1 ;;
    esac
    case "$arch" in
        x86_64|amd64)   arch="x86_64" ;;
        aarch64|arm64)  arch="aarch64" ;;
        *) return 1 ;;
    esac
    echo "${os}-${arch}"
}

# Pick the newest package in $UPDATES_DIR/<platform>/ matching
# omnish-*-<platform>.tar.gz. Echoes the absolute path, empty if none.
latest_package() {
    local platform="$1" dir="$UPDATES_DIR/$platform"
    [[ -d "$dir" ]] || return 0
    ls -1 "$dir"/omnish-*-"${platform}".tar.gz 2>/dev/null \
        | sort -V \
        | tail -n 1
}

# ── Deploy to clients ────────────────────────────────────────────────────────

for client in "${CLIENTS[@]}"; do
    [[ -z "$client" ]] && continue

    info "Deploying to: ${client}..."
    REMOTE_HOME="~/.omnish"

    platform="$(detect_remote_platform "$client")" || platform=""
    if [[ -z "$platform" ]]; then
        warn "Cannot detect platform for ${client} (ssh failed or unsupported uname)"
        status_marker "connect_failed ${client}"
        FAILED_COUNT=$((FAILED_COUNT + 1))
        continue
    fi

    pkg="$(latest_package "$platform")"
    if [[ -z "$pkg" ]]; then
        warn "No update package for ${platform} under ${UPDATES_DIR}/${platform}/"
        status_marker "no_package ${client}"
        FAILED_COUNT=$((FAILED_COUNT + 1))
        continue
    fi
    info "  platform=${platform} pkg=$(basename "$pkg")"

    # Ensure remote layout exists. Do NOT remove the old binaries - install.sh
    # replaces them atomically via rename(2), so nothing overwrites a running
    # process and there is no window with a missing file.
    if ! ssh -n -o BatchMode=yes -o ConnectTimeout=5 "$client" \
            "mkdir -p ${REMOTE_HOME}/bin ${REMOTE_HOME}/tls" 2>/dev/null; then
        warn "Cannot connect to ${client}, skipping"
        status_marker "connect_failed ${client}"
        FAILED_COUNT=$((FAILED_COUNT + 1))
        continue
    fi

    # Use a per-deploy tmp path on the remote so parallel deploys can't clash.
    # Stage locally to ~/.omnish/updates/ so we never leave junk in /tmp if the
    # final ssh step fails before cleanup.
    remote_stage="${REMOTE_HOME}/updates/.deploy-$$-$RANDOM"
    pkg_name="$(basename "$pkg")"

    if ! ssh -n -o BatchMode=yes -o ConnectTimeout=5 "$client" \
            "mkdir -p ${remote_stage}" 2>/dev/null; then
        warn "Failed to create staging dir on ${client}"
        status_marker "scp_failed ${client}"
        FAILED_COUNT=$((FAILED_COUNT + 1))
        continue
    fi

    if ! scp -q -o BatchMode=yes "$pkg" "${client}:${remote_stage}/${pkg_name}" 2>/dev/null; then
        warn "Failed to scp package to ${client}"
        ssh -n -o BatchMode=yes "$client" "rm -rf ${remote_stage}" 2>/dev/null || true
        status_marker "scp_failed ${client}"
        FAILED_COUNT=$((FAILED_COUNT + 1))
        continue
    fi

    # Extract + invoke install.sh --client-only --upgrade on the remote.
    # install.sh exits 0 on success, exit 2 when already up-to-date (both OK);
    # any other non-zero is a real failure. Let stderr flow back so the
    # failure notice can surface the actual error. Cleanup the staging dir
    # regardless of outcome.
    install_rc=0
    ssh -n -o BatchMode=yes "$client" "
        set -e
        cd ${remote_stage}
        tar xzf ${pkg_name}
        extracted=\"\$(find . -maxdepth 1 -type d -name 'omnish-*' | head -n 1)\"
        [ -d \"\$extracted\" ] || { echo 'extracted dir missing' >&2; exit 10; }
        bash \"\$extracted/install.sh\" --client-only --upgrade
    " || install_rc=$?
    ssh -n -o BatchMode=yes "$client" "rm -rf ${remote_stage}" 2>/dev/null || true

    # exit 2 from install.sh = already up-to-date; treat as success.
    if (( install_rc != 0 && install_rc != 2 )); then
        warn "install.sh failed on ${client} (rc=${install_rc})"
        status_marker "install_failed ${client}"
        FAILED_COUNT=$((FAILED_COUNT + 1))
        continue
    fi

    # TLS cert and auth token live outside the release package (they are
    # per-daemon credentials), so scp them separately. configure_remote then
    # writes client.toml if the remote doesn't have one.
    scp -q -o BatchMode=yes "${OMNISH_DIR}/tls/cert.pem" "${client}:${REMOTE_HOME}/tls/" 2>/dev/null || true
    scp -q -o BatchMode=yes "${OMNISH_DIR}/auth_token" "${client}:${REMOTE_HOME}/" 2>/dev/null || true
    configure_remote "$client"
    info "Deployed to ${client}"
done

if (( FAILED_COUNT > 0 )); then
    info "Deploy finished with ${FAILED_COUNT} failure(s)"
    exit 1
fi
info "Deploy complete"
