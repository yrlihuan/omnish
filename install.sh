#!/usr/bin/env bash
# omnish installer
#
# Downloads and installs omnish — a transparent shell wrapper with PTY proxy,
# inline LLM completion, and multi-terminal context aggregation.
#
# This script will:
#   1. Download the latest release (or a specified version) from GitHub/GitLab
#   2. Extract binaries to ~/.omnish/bin/ (or $OMNISH_HOME/bin/)
#   3. Walk you through configuring LLM backends for chat and completion
#   4. Generate TLS certificates and auth tokens for secure communication
#   5. Print client deployment instructions (if using TCP mode)
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/yrlihuan/omnish/master/install.sh | bash
#   bash install.sh --github --version=v0.6.4
#   OMNISH_HOME=/opt/omnish bash install.sh
#
# Environment variables:
#   OMNISH_HOME   Override the default installation directory (~/.omnish)

set -euo pipefail

OMNISH_DIR="${OMNISH_HOME:-${HOME}/.omnish}"
BIN_DIR="${OMNISH_DIR}/bin"

# ── Helpers ──────────────────────────────────────────────────────────────────

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mWARN:\033[0m %s\n' "$*"; }
error() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }
ask()   { printf '\033[1;32m?\033[0m %s ' "$1" >&2; read -r REPLY; }

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

SOURCE="gitlab"
FORCE=false
DRY_RUN=false
VERSION=""
for arg in "$@"; do
    case "$arg" in
        --github)      SOURCE="github" ;;
        --gitlab)      SOURCE="gitlab" ;;
        --force)       FORCE=true ;;
        --dry-run)     DRY_RUN=true ;;
        --version=*)   VERSION="${arg#*=}"
                       [[ "$VERSION" == v* ]] || VERSION="v${VERSION}" ;;
        --help|-h)
            echo "Usage: install.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --github          Download from GitHub (default: gitlab)"
            echo "  --gitlab          Download from GitLab"
            echo "  --version=vX.Y.Z  Install specific version (default: latest)"
            echo "  --force           Overwrite existing daemon.toml"
            echo "  --dry-run         Run config wizard but skip download/install/credentials"
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

# ── Download & Install ───────────────────────────────────────────────────────

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

if [[ "$DRY_RUN" == true ]]; then
    info "[DRY RUN] Would download omnish $VERSION from $SOURCE"
    info "[DRY RUN] Would install to ${OMNISH_DIR}"
else
    info "Downloading omnish from $SOURCE..."

    if [[ "$SOURCE" == "github" ]]; then
        REPO="yrlihuan/omnish"
        if [[ -z "$VERSION" ]]; then
            VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
                | grep '"tag_name"' | sed 's/.*"tag_name": *"//;s/".*//')
            [[ -n "$VERSION" ]] || error "Could not determine latest version"
        fi
        TAR_URL="https://github.com/${REPO}/releases/download/${VERSION}/omnish-${VERSION#v}-linux-${ARCH}.tar.gz"
    else
        PROJECT="dev%2Fomnish"
        if [[ -z "$VERSION" ]]; then
            VERSION=$(glab api "projects/${PROJECT}/releases" 2>/dev/null \
                | python3 -c "import sys,json; print(json.load(sys.stdin)[0]['tag_name'])" 2>/dev/null || echo "")
            [[ -n "$VERSION" ]] || error "Could not determine latest version. Use --version=vX.Y.Z"
        fi
        TAR_URL=$(glab api "projects/${PROJECT}/releases/${VERSION}" 2>/dev/null \
            | python3 -c "
import sys, json
links = json.load(sys.stdin).get('assets', {}).get('links', [])
for l in links:
    if 'tar.gz' in l.get('name', ''):
        print(l['direct_asset_url'])
        break
" 2>/dev/null || echo "")
        [[ -n "$TAR_URL" ]] || error "Could not find tar.gz asset for ${VERSION}"
    fi

    info "Version: $VERSION"

    curl -fSL "$TAR_URL" -o "$TMPDIR/omnish.tar.gz" || error "Download failed"
    tar -xzf "$TMPDIR/omnish.tar.gz" -C "$TMPDIR"

    # Find extracted directory
    EXTRACTED=$(find "$TMPDIR" -maxdepth 1 -type d -name 'omnish-*' | head -1)
    [[ -d "$EXTRACTED" ]] || error "Unexpected archive layout"

    # Install to ~/.omnish/
    info "Installing to ${OMNISH_DIR}..."

    mkdir -p "$BIN_DIR" "$OMNISH_DIR/plugins"

    cp "$EXTRACTED/bin/"* "$BIN_DIR/"
    chmod 755 "$BIN_DIR"/*

    if [[ -d "$EXTRACTED/plugins" ]] && ls "$EXTRACTED/plugins/"* &>/dev/null; then
        cp -r "$EXTRACTED/plugins/"* "$OMNISH_DIR/plugins/"
    fi

    chmod 700 "$OMNISH_DIR"
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
    ask "Use the same backend as chat/analysis? [Y/n]:"
    SAME="${REPLY:-Y}"

    if [[ "$SAME" =~ ^[Nn] ]]; then
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
    echo ""
    info "Daemon listen address:"
    echo "  [1] Unix socket (local only, default)"
    echo "  [2] TCP (for remote clients)"
    ask "Choice [1]:"
    LISTEN_CHOICE="${REPLY:-1}"

    if [[ "$LISTEN_CHOICE" == "2" ]]; then
        ask "TCP address (e.g. 0.0.0.0:9800):"
        LISTEN_ADDR="$REPLY"
    else
        LISTEN_ADDR="${OMNISH_DIR}/omnish.sock"
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

# ── PATH setup ───────────────────────────────────────────────────────────────

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
    echo ""
    info "Add to your shell profile:"
    echo ""
    echo "  export PATH=\"${BIN_DIR}:\$PATH\""
    echo ""
fi

# ── Client deployment instructions ──────────────────────────────────────────

if [[ "$LISTEN_CHOICE" == "2" ]] && [[ -n "${LISTEN_ADDR:-}" ]]; then
    SERVER_IP=$(hostname -I 2>/dev/null | awk '{print $1}' || hostname -i 2>/dev/null || echo "<server-ip>")
    LISTEN_PORT="${LISTEN_ADDR##*:}"
    echo ""
    info "=== Client Deployment ==="
    echo ""
    echo "On each client machine, run:"
    echo ""
    echo "  mkdir -p ~/.omnish/bin ~/.omnish/tls"
    echo ""
    echo "Then copy these files from this server:"
    echo ""
    echo "  scp ${BIN_DIR}/omnish ${BIN_DIR}/omnish-plugin \\"
    echo "      ${OMNISH_DIR}/tls/cert.pem ${OMNISH_DIR}/auth_token \\"
    echo "      user@client:~/.omnish/"
    echo ""
    echo "  scp ${BIN_DIR}/omnish ${BIN_DIR}/omnish-plugin user@client:~/.omnish/bin/"
    echo "  scp ${OMNISH_DIR}/tls/cert.pem user@client:~/.omnish/tls/"
    echo "  scp ${OMNISH_DIR}/auth_token user@client:~/.omnish/"
    echo ""
    echo "Create ~/.omnish/client.toml on the client:"
    echo ""
    echo "  daemon_addr = \"${SERVER_IP}:${LISTEN_PORT}\""
    echo ""
    echo "Add to PATH on the client:"
    echo ""
    echo "  export PATH=\"\$HOME/.omnish/bin:\$PATH\""
fi

echo ""
info "Installation complete! (omnish ${VERSION})"
