#!/usr/bin/env bash
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
        --version=*)   VERSION="${arg#*=}" ;;
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
        TAR_URL="https://github.com/${REPO}/releases/download/${VERSION}/omnish-${VERSION}-linux-${ARCH}.tar.gz"
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
PROVIDER_TYPE[anthropic]="anthropic";  PROVIDER_URL[anthropic]="";                                    PROVIDER_MODEL[anthropic]="claude-sonnet-4-20250514"
PROVIDER_TYPE[openrouter]="openai";    PROVIDER_URL[openrouter]="https://openrouter.ai/api/v1";       PROVIDER_MODEL[openrouter]=""
PROVIDER_TYPE[deepseek]="openai";      PROVIDER_URL[deepseek]="https://api.deepseek.com/v1";          PROVIDER_MODEL[deepseek]="deepseek-chat"
PROVIDER_TYPE[siliconflow]="openai";   PROVIDER_URL[siliconflow]="https://api.siliconflow.cn/v1";     PROVIDER_MODEL[siliconflow]="Qwen/Qwen2.5-Coder-32B-Instruct"
PROVIDER_TYPE[together]="openai";      PROVIDER_URL[together]="https://api.together.xyz/v1";          PROVIDER_MODEL[together]=""
PROVIDER_TYPE[fireworks]="openai";     PROVIDER_URL[fireworks]="https://api.fireworks.ai/inference/v1"; PROVIDER_MODEL[fireworks]=""
PROVIDER_TYPE[groq]="openai";         PROVIDER_URL[groq]="https://api.groq.com/openai/v1";           PROVIDER_MODEL[groq]=""
PROVIDER_TYPE[custom]="openai";        PROVIDER_URL[custom]="";                                       PROVIDER_MODEL[custom]=""

PROVIDER_NAMES=(anthropic openrouter deepseek siliconflow together fireworks groq custom)

configure_backend() {
    local name="$1"
    local purpose="$2"
    local recommended_model="${3:-}"

    # Interactive prompts go to stderr (stdout is for TOML output)
    echo "" >&2
    info "Configure $purpose backend ($name):" >&2
    local i=1
    for p in "${PROVIDER_NAMES[@]}"; do
        echo "  [$i] $p" >&2
        ((i++))
    done
    ask "Provider [1]:"
    local choice="${REPLY:-1}"
    local idx=$((choice - 1))
    if (( idx < 0 || idx >= ${#PROVIDER_NAMES[@]} )); then
        idx=0
    fi
    local provider="${PROVIDER_NAMES[$idx]}"

    local backend_type="${PROVIDER_TYPE[$provider]}"
    local base_url="${PROVIDER_URL[$provider]}"
    local default_model="${PROVIDER_MODEL[$provider]}"

    # Use recommended model if provided and provider has no default
    if [[ -n "$recommended_model" ]] && [[ -z "$default_model" ]]; then
        default_model="$recommended_model"
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

    if [[ "$provider" == "custom" ]]; then
        ask "Base URL:"
        base_url="$REPLY"
    fi

    # Write TOML section to stdout
    echo "[llm.backends.${name}]"
    echo "backend_type = \"${backend_type}\""
    echo "model = \"${model}\""
    echo "api_key_cmd = 'echo \"${api_key}\"'"
    if [[ -n "$base_url" ]]; then
        echo "base_url = \"${base_url}\""
    fi
    echo ""
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
    configure_backend "claude" "chat/analysis" "" > "$TMPDIR/chat_backend.toml"

    ask "Use the same backend for completion? [Y/n] (recommended: separate, e.g. Qwen/Qwen2.5-Coder-32B-Instruct):"
    SAME="${REPLY:-Y}"

    if [[ "$SAME" =~ ^[Nn] ]]; then
        configure_backend "claude-fast" "completion" "Qwen/Qwen2.5-Coder-32B-Instruct" > "$TMPDIR/completion_backend.toml"
        USE_CASES='[llm.use_cases]
chat = "claude"
analysis = "claude"
completion = "claude-fast"'
    else
        USE_CASES='[llm.use_cases]
chat = "claude"
analysis = "claude"
completion = "claude"'
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
        echo 'default = "claude"'
        echo ""
        cat "$TMPDIR/chat_backend.toml"
        if [[ -f "$TMPDIR/completion_backend.toml" ]]; then
            cat "$TMPDIR/completion_backend.toml"
        fi
        echo "$USE_CASES"
    } > "$TMPDIR/daemon.toml"

    if [[ "$DRY_RUN" == true ]]; then
        info "[DRY RUN] daemon.toml would contain:"
        echo "---"
        cat "$TMPDIR/daemon.toml"
        echo "---"
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
