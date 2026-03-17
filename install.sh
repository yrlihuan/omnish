#!/usr/bin/env bash
# omnish installer
#
# Downloads and installs omnish — a transparent shell wrapper with PTY proxy,
# inline LLM completion, and multi-terminal context aggregation.
#
# This script will:
#   1. Download the latest release (or a specified version) from GitHub
#   2. Extract binaries to ~/.omnish/bin/ (or $OMNISH_HOME/bin/)
#   3. Walk you through configuring LLM backends for chat and completion
#   4. Generate TLS certificates and auth tokens for secure communication
#   5. Deploy to remote client machines via scp (if using TCP mode)
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/yrlihuan/omnish/master/install.sh | bash
#   bash install.sh --version=v0.6.4
#   OMNISH_HOME=/opt/omnish bash install.sh
#
# If run from an extracted release directory (containing bin/ and assets/),
# it will use the local files instead of downloading from GitHub.
#
# Environment variables:
#   OMNISH_HOME   Override the default installation directory (~/.omnish)

set -euo pipefail

OMNISH_DIR="${OMNISH_HOME:-${HOME}/.omnish}"
BIN_DIR="${OMNISH_DIR}/bin"

# ── Helpers ──────────────────────────────────────────────────────────────────

info()  { printf '\033[1;34m[omnish]\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mWARN:\033[0m %s\n' "$*"; }
error() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }
ask()   { printf '\033[1;32m?\033[0m %s ' "$1" >&2; read -r REPLY </dev/tty; }

# ── Platform detection ───────────────────────────────────────────────────────

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *) error "Unsupported architecture: $ARCH" ;;
esac
[[ "$OS" == "linux" ]] || error "Only Linux is supported currently"

# ── Parse arguments ──────────────────────────────────────────────────────────

FORCE=false
DRY_RUN=false
UPGRADE=false
VERSION=""
for arg in "$@"; do
    case "$arg" in
        --upgrade)     UPGRADE=true ;;
        --force)       FORCE=true ;;
        --dry-run)     DRY_RUN=true ;;
        --version=*)   VERSION="${arg#*=}"
                       [[ "$VERSION" == v* ]] || VERSION="v${VERSION}" ;;
        --uninstall)
            echo ""
            info "Uninstalling omnish..."
            SERVICE_FILE="$HOME/.config/systemd/user/omnish-daemon.service"
            if [[ -f "$SERVICE_FILE" ]]; then
                systemctl --user stop omnish-daemon 2>/dev/null || true
                systemctl --user disable omnish-daemon 2>/dev/null || true
                rm -f "$SERVICE_FILE"
                systemctl --user daemon-reload
                info "Removed systemd service"
            fi
            # Remove PATH line from shell profiles
            for rc in "$HOME/.bashrc" "$HOME/.zshrc"; do
                if [[ -f "$rc" ]] && grep -q '# omnish' "$rc"; then
                    sed -i '/# omnish/d;/omnish\/bin/d' "$rc"
                    info "Cleaned PATH from ${rc}"
                fi
            done
            info "Uninstall complete"
            exit 0
            ;;
        --help|-h)
            echo "Usage: install.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --version=vX.Y.Z  Install specific version (default: latest)"
            echo "  --upgrade         Non-interactive upgrade (download + install only)"
            echo "  --force           Overwrite existing daemon.toml"
            echo "  --dry-run         Run config wizard but skip download/install/credentials"
            echo "  --uninstall       Remove omnish, systemd service, and PATH entries"
            echo "  -h, --help        Show this help"
            exit 0
            ;;
    esac
done

# In dry-run mode, replace destructive commands with echo
if [[ "$DRY_RUN" == true ]]; then
    info "[DRY RUN] No files will be written"
    VERSION="${VERSION:-v0.0.0-dry-run}"
fi

# ── Locate or download release ────────────────────────────────────────────────

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

# Auto-detect: if bin/ and assets/ exist next to this script, use local files
# (BASH_SOURCE is unset when piped via curl|bash, so skip detection in that case)
if [[ -n "${BASH_SOURCE[0]+x}" ]]; then
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
else
    SCRIPT_DIR=""
fi
if [[ -n "$SCRIPT_DIR" ]] && [[ -d "$SCRIPT_DIR/bin" ]] && [[ -d "$SCRIPT_DIR/assets" ]]; then
    EXTRACTED="$SCRIPT_DIR"
    # Derive version from binary if not specified
    if [[ -z "$VERSION" ]] && [[ -x "$EXTRACTED/bin/omnish-daemon" ]]; then
        VERSION="v$("$EXTRACTED/bin/omnish-daemon" --version 2>/dev/null | awk '{print $2}' || echo "unknown")"
    fi
    info "Installing from local directory: $EXTRACTED"
fi

if [[ "$DRY_RUN" == true ]]; then
    info "[DRY RUN] Would install omnish ${VERSION:-latest} to ${OMNISH_DIR}"
elif [[ -z "${EXTRACTED:-}" ]]; then
    # Download from GitHub
    REPO="yrlihuan/omnish"
    if [[ -z "$VERSION" ]]; then
        info "Fetching latest version..."
        VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name"' | sed 's/.*"tag_name": *"//;s/".*//')
        [[ -n "$VERSION" ]] || error "Could not determine latest version"
    fi

    info "Version: $VERSION"

    # In upgrade mode, skip if already up to date
    if [[ "$UPGRADE" == true ]]; then
        CURRENT_VERSION=""
        if [[ -x "$BIN_DIR/omnish-daemon" ]]; then
            CURRENT_VERSION=$("$BIN_DIR/omnish-daemon" --version 2>/dev/null | awk '{print $2}' || echo "")
        fi
        if [[ "$CURRENT_VERSION" == "${VERSION#v}" ]]; then
            info "Already up to date (v${CURRENT_VERSION})"
            exit 0
        fi
        info "Update available: v${CURRENT_VERSION:-unknown} -> ${VERSION}"
    fi

    TAR_URL="https://github.com/${REPO}/releases/download/${VERSION}/omnish-${VERSION#v}-linux-${ARCH}.tar.gz"
    curl -fSL "$TAR_URL" -o "$TMPDIR/omnish.tar.gz" || error "Download failed"
    tar -xzf "$TMPDIR/omnish.tar.gz" -C "$TMPDIR"

    EXTRACTED=$(find "$TMPDIR" -maxdepth 1 -type d -name 'omnish-*' | head -1)
    [[ -d "$EXTRACTED" ]] || error "Unexpected archive layout"
fi

# ── Install ──────────────────────────────────────────────────────────────────

if [[ "$DRY_RUN" != true ]]; then
    info "Version: ${VERSION:-unknown}"

    # Detect existing version for upgrade message
    OLD_VERSION=""
    if [[ -x "$BIN_DIR/omnish" ]]; then
        OLD_VERSION=$("$BIN_DIR/omnish" --version 2>/dev/null | awk '{print $NF}' || echo "")
    fi

    info "Installing to ${OMNISH_DIR}..."

    mkdir -p "$BIN_DIR" "$OMNISH_DIR/plugins"

    cp "$EXTRACTED/bin/"* "$BIN_DIR/"
    chmod 755 "$BIN_DIR"/*

    # Install assets (plugin configs, prompts, scripts)
    if [[ -d "$EXTRACTED/assets" ]]; then
        # Plugin tool definitions (always overwrite, with warning header)
        mkdir -p "$OMNISH_DIR/plugins/builtin"
        { echo "// This file is for demonstration only. Use tool.override.json to customize."; cat "$EXTRACTED/assets/plugins/builtin/tool.json"; } > "$OMNISH_DIR/plugins/builtin/tool.json"

        # tool.override.json.example (only if not present)
        if [[ ! -f "$OMNISH_DIR/plugins/builtin/tool.override.json.example" ]]; then
            cp "$EXTRACTED/assets/plugins/builtin/tool.override.json.example" "$OMNISH_DIR/plugins/builtin/"
        fi

        # Chat prompts (always overwrite, with warning header)
        mkdir -p "$OMNISH_DIR/prompts"
        { echo "// This file is for demonstration only. Use chat.override.json to customize."; cat "$EXTRACTED/assets/prompts/chat.json"; } > "$OMNISH_DIR/prompts/chat.json"

        # chat.override.json.example (only if not present)
        if [[ ! -f "$OMNISH_DIR/prompts/chat.override.json.example" ]]; then
            cp "$EXTRACTED/assets/prompts/chat.override.json.example" "$OMNISH_DIR/prompts/"
        fi

        # Scripts (always overwrite)
        cp "$EXTRACTED/assets/deploy.sh" "$OMNISH_DIR/"
        chmod 755 "$OMNISH_DIR/deploy.sh"
    fi

    # Copy install.sh itself (from extracted root or assets)
    if [[ -f "$EXTRACTED/install.sh" ]]; then
        cp "$EXTRACTED/install.sh" "$OMNISH_DIR/"
        chmod 755 "$OMNISH_DIR/install.sh"
    fi

    chmod 700 "$OMNISH_DIR"
fi

# In upgrade mode, skip all interactive configuration
if [[ "$UPGRADE" == true ]]; then
    echo ""
    info "Upgrade complete (${VERSION})"
    exit 0
fi

# ── LLM configuration ───────────────────────────────────────────────────────

# Provider presets: name -> (backend_type, base_url, default_model)
declare -A PROVIDER_TYPE PROVIDER_URL PROVIDER_MODEL
PROVIDER_TYPE[anthropic]="anthropic";    PROVIDER_URL[anthropic]="";                                PROVIDER_MODEL[anthropic]="claude-sonnet-4-20250514"
PROVIDER_TYPE[openai]="openai";          PROVIDER_URL[openai]="https://api.openai.com/v1";          PROVIDER_MODEL[openai]="gpt-4o"
PROVIDER_TYPE[openrouter]="openai";      PROVIDER_URL[openrouter]="https://openrouter.ai/api/v1";   PROVIDER_MODEL[openrouter]=""
PROVIDER_TYPE[deepseek]="anthropic";      PROVIDER_URL[deepseek]="https://api.deepseek.com/anthropic";      PROVIDER_MODEL[deepseek]="deepseek-chat"
PROVIDER_TYPE[moonshot-cn]="anthropic";  PROVIDER_URL[moonshot-cn]="https://api.moonshot.cn/anthropic";     PROVIDER_MODEL[moonshot-cn]="kimi-k2-preview"
PROVIDER_TYPE[moonshot-global]="anthropic"; PROVIDER_URL[moonshot-global]="https://api.moonshot.ai/anthropic"; PROVIDER_MODEL[moonshot-global]="kimi-k2-preview"
PROVIDER_TYPE[custom]="openai";          PROVIDER_URL[custom]="";                                    PROVIDER_MODEL[custom]=""

CHAT_PROVIDERS=(anthropic openai openrouter deepseek moonshot-cn moonshot-global custom)
COMPLETION_PROVIDERS=(openrouter custom)

configure_backend() {
    local purpose="$1"
    local recommended_model="${2:-}"
    shift 2
    local providers=("$@")

    # Interactive prompts go to stderr (stdout is for TOML output)
    echo "" >&2
    info "Configure $purpose backend:" >&2
    local i=1
    for p in "${providers[@]}"; do
        echo "  [$i] $p" >&2
        ((i++))
    done
    ask "Provider [1]:"
    local choice="${REPLY:-1}"
    local idx=$((choice - 1))
    if (( idx < 0 || idx >= ${#providers[@]} )); then
        idx=0
    fi
    local provider="${providers[$idx]}"

    # Backend name: provider name for presets, user-specified for custom
    local name="$provider"
    if [[ "$provider" == "custom" ]]; then
        ask "Backend name:"
        name="$REPLY"
    fi

    local backend_type="${PROVIDER_TYPE[$provider]}"
    local base_url="${PROVIDER_URL[$provider]}"
    local default_model="${PROVIDER_MODEL[$provider]}"

    # Use recommended model if provided and provider has no default
    if [[ -n "$recommended_model" ]] && [[ -z "$default_model" ]]; then
        default_model="$recommended_model"
    fi

    if [[ "$provider" == "custom" ]]; then
        ask "Base URL:"
        base_url="$REPLY"
    fi

    if [[ -n "$default_model" ]]; then
        ask "Model name [$default_model]:"
        local model="${REPLY:-$default_model}"
    else
        ask "Model name:"
        local model="$REPLY"
    fi

    ask "API key:"
    local api_key="$REPLY"

    # Save backend name for caller
    echo "$name" > "$TMPDIR/last_backend_name"

    # Build TOML snippet
    local toml="[llm.backends.${name}]"$'\n'
    toml+="backend_type = \"${backend_type}\""$'\n'
    toml+="model = \"${model}\""$'\n'
    toml+="api_key_cmd = 'echo \"${api_key}\"'"$'\n'
    if [[ -n "$base_url" ]]; then
        toml+="base_url = \"${base_url}\""$'\n'
    fi

    # Preview with masked API key and confirm
    echo "" >&2
    echo "───────────────────────────────────────" >&2
    echo "$toml" | sed 's/echo "\(.\{4\}\).*\(.\{4\}\)"/echo "\1...\2"/' >&2
    echo "───────────────────────────────────────" >&2
    ask "OK? [Y/n]:"
    if [[ "${REPLY:-Y}" =~ ^[Nn] ]]; then
        warn "Aborted" >&2
        exit 1
    fi

    # Write TOML section to stdout
    printf '%s\n' "$toml"
}

DAEMON_TOML="$OMNISH_DIR/daemon.toml"

if [[ -f "$DAEMON_TOML" ]] && [[ "$FORCE" != true ]]; then
    info "daemon.toml already exists, skipping LLM configuration (use --force to overwrite)"
    # Still need LISTEN_CHOICE for client deployment instructions
    if grep -q 'listen_addr.*:' "$DAEMON_TOML" 2>/dev/null; then
        LISTEN_CHOICE="2"
    else
        LISTEN_CHOICE="1"
    fi
else
    # Chat/analysis backend
    echo "" >&2
    info "Step 1: Chat & Analysis model" >&2
    echo "  This model handles interactive chat (: prefix), command error analysis," >&2
    echo "  and context-aware responses. A capable model (e.g. Claude Sonnet) is" >&2
    echo "  recommended for best results." >&2
    configure_backend "chat/analysis" "" "${CHAT_PROVIDERS[@]}" > "$TMPDIR/chat_backend.toml"
    CHAT_NAME=$(cat "$TMPDIR/last_backend_name")

    echo "" >&2
    info "Step 2: Completion model" >&2
    echo "  This model powers inline ghost-text completion as you type in the shell." >&2
    echo "  It runs frequently and should be fast and cheap. A coding-specific model" >&2
    echo "  like Qwen2.5-Coder-32B works well. You can use the same model as chat," >&2
    echo "  but a separate, faster model is recommended." >&2
    ask "Use the same backend as chat/analysis? [y/N]:"
    SAME="${REPLY:-N}"

    if [[ ! "$SAME" =~ ^[Yy] ]]; then
        configure_backend "completion" "Qwen/Qwen2.5-Coder-32B-Instruct" "${COMPLETION_PROVIDERS[@]}" > "$TMPDIR/completion_backend.toml"
        COMPLETION_NAME=$(cat "$TMPDIR/last_backend_name")
        USE_CASES="[llm.use_cases]
chat = \"${CHAT_NAME}\"
analysis = \"${CHAT_NAME}\"
completion = \"${COMPLETION_NAME}\""
    else
        USE_CASES="[llm.use_cases]
chat = \"${CHAT_NAME}\"
analysis = \"${CHAT_NAME}\"
completion = \"${CHAT_NAME}\""
    fi

    # Listen address
    echo "" >&2
    info "Daemon listen address:" >&2
    echo "  [1] Unix socket (local only, default)" >&2
    echo "  [2] TCP (for remote clients)" >&2
    ask "Choice [1]:"
    LISTEN_CHOICE="${REPLY:-1}"

    if [[ "$LISTEN_CHOICE" == "2" ]]; then
        ask "TCP address [0.0.0.0:9800]:"
        LISTEN_ADDR="${REPLY:-0.0.0.0:9800}"
    else
        LISTEN_ADDR="${OMNISH_DIR}/omnish.sock"
    fi

    # Auto-update
    echo "" >&2
    ask "Enable auto-update? [Y/n]:"
    AUTO_UPDATE="${REPLY:-Y}"
    if [[ "$AUTO_UPDATE" =~ ^[Yy] ]]; then
        AUTO_UPDATE_ENABLED=true
    else
        AUTO_UPDATE_ENABLED=false
    fi

    # Assemble daemon.toml
    {
        echo "listen_addr = \"${LISTEN_ADDR}\""
        echo ""
        echo "[llm]"
        echo "default = \"${CHAT_NAME}\""
        echo ""
        cat "$TMPDIR/chat_backend.toml"
        if [[ -f "$TMPDIR/completion_backend.toml" ]]; then
            cat "$TMPDIR/completion_backend.toml"
        fi
        echo "$USE_CASES"
        echo ""
        echo "[tasks.auto_update]"
        echo "enabled = ${AUTO_UPDATE_ENABLED}"
    } > "$TMPDIR/daemon.toml"

    if [[ "$DRY_RUN" == true ]]; then
        info "[DRY RUN] Would write to $DAEMON_TOML"
    else
        cp "$TMPDIR/daemon.toml" "$DAEMON_TOML"
        chmod 600 "$DAEMON_TOML"
        info "Written: $DAEMON_TOML"
    fi
fi

# ── Generate credentials ────────────────────────────────────────────────────

if [[ "$DRY_RUN" == true ]]; then
    info "[DRY RUN] Would run: omnish-daemon --init"
else
    info "Generating TLS certificate and auth token..."
    "$BIN_DIR/omnish-daemon" --init
    chmod 600 "$OMNISH_DIR/auth_token"
fi

# ── Generate client.toml ──────────────────────────────────────────────────────

CLIENT_TOML="$OMNISH_DIR/client.toml"
if [[ ! -f "$CLIENT_TOML" ]] || [[ "$FORCE" == true ]]; then
    if [[ "$LISTEN_CHOICE" == "2" ]] && [[ -n "${LISTEN_ADDR:-}" ]]; then
        LISTEN_PORT="${LISTEN_ADDR##*:}"

        # Collect private network IPs (192.168.x.x and 10.x.x.x)
        CANDIDATE_IPS=()
        if command -v hostname &>/dev/null; then
            for ip in $(hostname -I 2>/dev/null); do
                if [[ "$ip" == 192.168.* ]] || [[ "$ip" == 10.* ]]; then
                    CANDIDATE_IPS+=("$ip")
                fi
            done
        fi

        if [[ ${#CANDIDATE_IPS[@]} -gt 1 ]]; then
            echo "" >&2
            info "Multiple private IPs detected:" >&2
            i=1
            for ip in "${CANDIDATE_IPS[@]}"; do
                echo "  [$i] $ip" >&2
                ((i++))
            done
            ask "Select server IP [1]:"
            idx=$(( ${REPLY:-1} - 1 ))
            if (( idx < 0 || idx >= ${#CANDIDATE_IPS[@]} )); then
                idx=0
            fi
            SERVER_IP="${CANDIDATE_IPS[$idx]}"
        elif [[ ${#CANDIDATE_IPS[@]} -eq 1 ]]; then
            SERVER_IP="${CANDIDATE_IPS[0]}"
        else
            SERVER_IP=$(hostname -I 2>/dev/null | awk '{print $1}' || hostname -i 2>/dev/null || echo "<server-ip>")
        fi

        CLIENT_DAEMON_ADDR="${SERVER_IP}:${LISTEN_PORT}"
    else
        CLIENT_DAEMON_ADDR="${LISTEN_ADDR:-${OMNISH_DIR}/omnish.sock}"
    fi

    if [[ "$DRY_RUN" == true ]]; then
        info "[DRY RUN] Would write client.toml (daemon_addr = \"${CLIENT_DAEMON_ADDR}\")"
    else
        cat > "$CLIENT_TOML" << EOF
# omnish client configuration
# Copy to ~/.omnish/client.toml on each client machine

# Daemon address (Unix socket path or host:port for TCP)
daemon_addr = "${CLIENT_DAEMON_ADDR}"

# Enable inline ghost-text completion
completion_enabled = true

# Enable auto-update from server
auto_update = ${AUTO_UPDATE_ENABLED:-true}

[shell]
# Shell to spawn (defaults to \$SHELL)
# command = "/bin/bash"

# Prefix to trigger chat mode
command_prefix = ":"

# Prefix to resume last chat thread
resume_prefix = "::"

# Minimum idle time (ms) before prefix triggers intercept
# intercept_gap_ms = 1000

# Timeout (ms) for ghost-text completion
# ghost_timeout_ms = 10000
EOF
        chmod 600 "$CLIENT_TOML"
        info "Written: $CLIENT_TOML"
    fi
fi

# ── Systemd service ──────────────────────────────────────────────────────────

SERVICE_DIR="$HOME/.config/systemd/user"
SERVICE_FILE="$SERVICE_DIR/omnish-daemon.service"

if [[ "$DRY_RUN" == true ]]; then
    info "[DRY RUN] Would offer to install systemd user service"
elif ! command -v systemctl &>/dev/null; then
    info "systemctl not found, skipping daemon autostart setup"
elif [[ -f "$SERVICE_FILE" ]]; then
    info "systemd service already installed, reloading..."
    systemctl --user daemon-reload
    systemctl --user restart omnish-daemon
    info "omnish-daemon restarted"
else
    ask "Enable omnish-daemon to start on boot (systemd user service)? [Y/n]:"
    if [[ ! "${REPLY:-Y}" =~ ^[Nn] ]]; then
        mkdir -p "$SERVICE_DIR"
        cat > "$SERVICE_FILE" << UNIT
[Unit]
Description=omnish daemon
After=network.target

[Service]
ExecStart=${BIN_DIR}/omnish-daemon
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
UNIT
        systemctl --user daemon-reload
        systemctl --user enable --now omnish-daemon
        info "omnish-daemon enabled and started"

        # enable-linger so service runs without active login session
        if command -v loginctl &>/dev/null && ! loginctl show-user "$USER" --property=Linger 2>/dev/null | grep -q 'yes'; then
            info "Enabling lingering for $USER (so daemon runs at boot without login)..."
            sudo loginctl enable-linger "$USER" 2>/dev/null \
                || warn "Could not enable linger — run: sudo loginctl enable-linger $USER"
        fi
    fi
fi

# ── PATH setup ───────────────────────────────────────────────────────────────

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
    # Detect user's shell profile
    SHELL_NAME="$(basename "${SHELL:-/bin/bash}")"
    case "$SHELL_NAME" in
        zsh)  PROFILE="$HOME/.zshrc" ;;
        bash) PROFILE="$HOME/.bashrc" ;;
        *)    PROFILE="" ;;
    esac
    PATH_LINE="export PATH=\"${BIN_DIR}:\$PATH\""

    if [[ -n "$PROFILE" ]]; then
        ask "Add omnish to PATH in ${PROFILE}? [Y/n]:"
        if [[ ! "${REPLY:-Y}" =~ ^[Nn] ]]; then
            echo "" >> "$PROFILE"
            echo "# omnish" >> "$PROFILE"
            echo "$PATH_LINE" >> "$PROFILE"
            info "Added to ${PROFILE} — restart your shell or run: source ${PROFILE}"
        else
            echo ""
            info "Add to your shell profile manually:"
            echo ""
            echo "  $PATH_LINE"
            echo ""
        fi
    else
        echo ""
        info "Add to your shell profile:"
        echo ""
        echo "  $PATH_LINE"
        echo ""
    fi
fi

# ── Client deployment (skip on upgrade) ───────────────────────────────────

if [[ "$LISTEN_CHOICE" == "2" ]] && [[ -n "${LISTEN_ADDR:-}" ]] && [[ -z "${OLD_VERSION:-}" ]]; then
    echo ""
    info "=== Client Deployment ==="
    echo ""
    info "Server address: ${SERVER_IP}:${LISTEN_PORT}"
    echo ""

    deploy_client() {
        local target="$1"
        local remote_home="~/.omnish"

        info "Deploying to ${target}..."

        # Create directories
        ssh "$target" "mkdir -p ${remote_home}/bin ${remote_home}/tls" \
            || { warn "SSH connection failed for ${target}"; return 1; }

        # Copy binaries
        scp -q "${BIN_DIR}/omnish" "${BIN_DIR}/omnish-plugin" "${target}:${remote_home}/bin/" \
            || { warn "Failed to copy binaries to ${target}"; return 1; }

        # Copy TLS cert and auth token
        scp -q "${OMNISH_DIR}/tls/cert.pem" "${target}:${remote_home}/tls/" \
            || { warn "Failed to copy TLS cert to ${target}"; return 1; }
        scp -q "${OMNISH_DIR}/auth_token" "${target}:${remote_home}/" \
            || { warn "Failed to copy auth token to ${target}"; return 1; }

        # Copy client.toml and set permissions
        scp -q "${OMNISH_DIR}/client.toml" "${target}:${remote_home}/" \
            || { warn "Failed to copy client.toml to ${target}"; return 1; }
        ssh "$target" "chmod 600 ${remote_home}/client.toml ${remote_home}/auth_token"

        info "Deployed to ${target}"
        echo "  Run on client: export PATH=\"\$HOME/.omnish/bin:\$PATH\""
    }

    DEPLOYED_CLIENTS=()

    if [[ "$DRY_RUN" == true ]]; then
        info "[DRY RUN] Would ask to deploy clients via scp"
    else
        ask "Deploy to client machines via scp? [Y/n]:"
        if [[ ! "${REPLY:-Y}" =~ ^[Nn] ]]; then
            echo "  Enter user@host for each client (empty line to finish):"
            CLIENT_COUNT=0
            while true; do
                if [[ $CLIENT_COUNT -eq 0 ]]; then
                    ask "  Client:"
                else
                    ask "  Another client (enter to finish):"
                fi
                [[ -n "$REPLY" ]] || break
                ((CLIENT_COUNT++))
                CLIENT_HOST="$REPLY"

                # Verify SSH connectivity
                info "Checking SSH connectivity to ${CLIENT_HOST}..."
                if ssh -o ConnectTimeout=5 -o BatchMode=yes "$CLIENT_HOST" true 2>/dev/null; then
                    info "SSH OK: ${CLIENT_HOST}"
                    deploy_client "$CLIENT_HOST" && DEPLOYED_CLIENTS+=("$CLIENT_HOST") || true
                else
                    warn "Cannot connect to ${CLIENT_HOST} via SSH, skipping"
                fi
            done
        fi
    fi

    # Append auto_update config with client list to daemon.toml
    if [[ ${#DEPLOYED_CLIENTS[@]} -gt 0 ]] && [[ -f "$DAEMON_TOML" ]]; then
        {
            echo ""
            echo "[tasks.auto_update]"
            echo "enabled = true"
            # Build TOML array
            printf 'clients = ['
            for i in "${!DEPLOYED_CLIENTS[@]}"; do
                (( i > 0 )) && printf ', '
                printf '"%s"' "${DEPLOYED_CLIENTS[$i]}"
            done
            echo ']'
        } >> "$DAEMON_TOML"
        info "Auto-update enabled with ${#DEPLOYED_CLIENTS[@]} client(s) in daemon.toml"
    fi

fi

echo ""
if [[ -n "${OLD_VERSION:-}" ]] && [[ "$OLD_VERSION" != "${VERSION#v}" ]]; then
    info "Upgrade complete! (v${OLD_VERSION} → ${VERSION})"
else
    info "Installation complete! (omnish ${VERSION})"
fi
