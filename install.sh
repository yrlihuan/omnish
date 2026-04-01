#!/usr/bin/env bash
# Re-exec under bash if invoked via sh/dash (arrays require bash 4+)
if [ -z "$BASH_VERSION" ]; then
    if [ -f "$0" ]; then
        exec bash "$0" "$@"
    else
        echo "Error: this script requires bash. Please run: curl ... | bash" >&2
        exit 1
    fi
fi
# Force bash to read the entire script into memory before executing,
# so self-replacement during upgrade won't corrupt the running script.
{
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
DEPLOYED_CLIENTS=()

# ── Helpers ──────────────────────────────────────────────────────────────────

info()  { printf '\033[1;34m[omnish]\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mWARN:\033[0m %s\n' "$*"; }
error() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }
ask()   { printf '\033[1;32m?\033[0m %s ' "$1" >&2; read -r REPLY </dev/tty; }

# Write or replace a [tools.<name>] section in a TOML file
patch_toml_section() {
    local toml_file="$1"
    local plugin_name="$2"
    local -n values=$3
    local section_header="[tools.$plugin_name]"

    # Remove existing section if present (from header to next [section] or EOF)
    if grep -q "^\[tools\.$plugin_name\]" "$toml_file"; then
        sed -i "/^\[tools\.$plugin_name\]/,/^\[/{/^\[tools\.$plugin_name\]/d;/^\[/!d}" "$toml_file"
    fi

    # Also remove commented-out section and its commented keys (including blank lines)
    if grep -q "^# *\[tools\.$plugin_name\]" "$toml_file"; then
        sed -i "/^# *\[tools\.$plugin_name\]/,/^[^#$]/{/^# *\[tools\.$plugin_name\]/d;/^# /d}" "$toml_file"
    fi

    # Append new section
    {
        echo ""
        echo "$section_header"
        for key in "${!values[@]}"; do
            if [[ -n "${values[$key]}" ]]; then
                # Escape backslashes and double quotes for TOML string values
                local escaped="${values[$key]//\\/\\\\}"
                escaped="${escaped//\"/\\\"}"
                echo "$key = \"$escaped\""
            fi
        done
    } >> "$toml_file"
}

# Add a plugin name to [plugins] enabled array in a TOML file
patch_plugins_enabled() {
    local toml_file="$1"
    local plugin_name="$2"

    if ! grep -q '^\[plugins\]' "$toml_file"; then
        # No [plugins] section — check for commented one
        if grep -q '^# *\[plugins\]' "$toml_file"; then
            # Uncomment and set
            sed -i 's/^# *\[plugins\]/[plugins]/' "$toml_file"
            sed -i '/^\[plugins\]/,/^\[/{s/^# *enabled = \[.*\]/enabled = ["'"$plugin_name"'"]/}' "$toml_file"
        else
            # Append new section
            printf '\n[plugins]\nenabled = ["%s"]\n' "$plugin_name" >> "$toml_file"
        fi
    else
        # [plugins] section exists — grep for enabled= scoped to [plugins] section only
        local current
        current=$(sed -n '/^\[plugins\]/,/^\[/{/^enabled/p}' "$toml_file" | head -1)
        if [[ -z "$current" ]]; then
            # enabled key missing, add after [plugins]
            sed -i '/^\[plugins\]/a enabled = ["'"$plugin_name"'"]' "$toml_file"
        elif echo "$current" | grep -q "\"$plugin_name\""; then
            # Already in the list
            :
        elif echo "$current" | grep -q '\[\]'; then
            # Empty array: enabled = [] -> enabled = ["web_search"]
            sed -i '/^\[plugins\]/,/^\[/{s/^enabled = \[\]/enabled = ["'"$plugin_name"'"]/}' "$toml_file"
        else
            # Append to existing array: enabled = ["a"] -> enabled = ["a", "web_search"]
            sed -i '/^\[plugins\]/,/^\[/{s/^\(enabled = \[.*\)\]/\1, "'"$plugin_name"'"]/}' "$toml_file"
        fi
    fi
}

# Run interactive setup for a plugin using its setup.json manifest
setup_plugin() {
    local plugin_name="$1"
    local plugin_dir="$OMNISH_DIR/plugins/$plugin_name"
    local setup_file="$plugin_dir/setup.json"
    local daemon_toml="$OMNISH_DIR/daemon.toml"

    if [[ ! -f "$setup_file" ]]; then
        return 0
    fi

    if ! command -v jq &>/dev/null; then
        warn "jq not found, skipping setup for $plugin_name"
        return 1
    fi

    echo ""
    info "Setting up plugin: $plugin_name"

    local param_count
    param_count=$(jq '.params | length' "$setup_file")

    # Collect values
    declare -A PARAM_VALUES
    for ((i = 0; i < param_count; i++)); do
        local name prompt required default_val
        name=$(jq -r ".params[$i].name" "$setup_file")
        prompt=$(jq -r ".params[$i].prompt" "$setup_file")
        required=$(jq -r ".params[$i].required // false" "$setup_file")
        default_val=$(jq -r ".params[$i].default // empty" "$setup_file")

        if [[ -n "$default_val" ]]; then
            ask "$prompt [$default_val]:"
            PARAM_VALUES[$name]="${REPLY:-$default_val}"
        else
            ask "$prompt:"
            PARAM_VALUES[$name]="$REPLY"
        fi

        if [[ "$required" == "true" ]] && [[ -z "${PARAM_VALUES[$name]}" ]]; then
            warn "Required parameter '$name' not provided, skipping $plugin_name"
            info "Run install.sh --setup-plugin=$plugin_name to configure later"
            return 1
        fi
    done

    # Write [tools.<plugin_name>] section to daemon.toml
    patch_toml_section "$daemon_toml" "$plugin_name" PARAM_VALUES

    # Add to [plugins] enabled
    patch_plugins_enabled "$daemon_toml" "$plugin_name"

    # Show confirmation
    for key in "${!PARAM_VALUES[@]}"; do
        info "  $key = ${PARAM_VALUES[$key]}"
    done

    info "Plugin $plugin_name configured and enabled"
}

# ── Version normalization ────────────────────────────────────────────────────
# Strips git commit hash and replaces '-' with '.' for numeric comparison.
# e.g. "0.8.4-71-gdf067f6" → "0.8.4.71", "v0.8.4.71" → "0.8.4.71"
normalize_version() {
    local v="${1#v}"
    # Strip -g<hex> suffix
    v="$(echo "$v" | sed 's/-g[0-9a-f]*$//')"
    # Replace remaining dashes with dots
    echo "${v//-/.}"
}

# Check if version $1 is newer than version $2 (using normalized versions).
# Returns 0 (true) if $1 > $2, 1 (false) otherwise.
is_newer_version() {
    local a="$(normalize_version "$1")"
    local b="$(normalize_version "$2")"
    [[ "$a" == "$b" ]] && return 1
    # sort -V puts the smaller version first; if $a comes second, it's newer
    [[ "$(printf '%s\n%s' "$a" "$b" | sort -V | tail -1)" == "$a" ]]
}

# ── Platform detection ───────────────────────────────────────────────────────

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *) error "Unsupported architecture: $ARCH" ;;
esac
# ── Parse arguments ──────────────────────────────────────────────────────────

FORCE=false
DRY_RUN=false
UPGRADE=false
CLIENT_ONLY=false
VERSION=""
FROM_DIR=""
SETUP_PLUGIN=""
for arg in "$@"; do
    case "$arg" in
        --upgrade)     UPGRADE=true ;;
        --force)       FORCE=true ;;
        --dry-run)     DRY_RUN=true ;;
        --client-only) CLIENT_ONLY=true ;;
        --version=*)   VERSION="${arg#*=}"
                       [[ "$VERSION" == v* ]] || VERSION="v${VERSION}" ;;
        --dir=*)       FROM_DIR="${arg#*=}" ;;
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
        --setup-plugin=*) SETUP_PLUGIN="${arg#*=}" ;;
        --help|-h)
            echo "Usage: install.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --version=vX.Y.Z  Install specific version (default: latest)"
            echo "  --dir=<path>      Install from local directory containing tar.gz files"
            echo "  --upgrade         Non-interactive upgrade (download + install only)"
            echo "  --client-only     Skip installing omnish-daemon (for client-side updates)"
            echo "  --force           Overwrite existing daemon.toml"
            echo "  --dry-run         Run config wizard but skip download/install/credentials"
            echo "  --uninstall       Remove omnish, systemd service, and PATH entries"
            echo "  --setup-plugin=<name> Configure (or reconfigure) a specific plugin"
            echo "  -h, --help        Show this help"
            exit 0
            ;;
    esac
done

# ── Platform check ───────────────────────────────────────────────────────────
if [[ "$OS" != "linux" ]]; then
    if [[ "$CLIENT_ONLY" != true ]]; then
        error "Full install requires Linux. Use --client-only on $OS."
    fi
fi

# ── Plugin setup (standalone mode) ──────────────────────────────────────────
if [[ -n "$SETUP_PLUGIN" ]]; then
    DAEMON_TOML="$OMNISH_DIR/daemon.toml"
    [[ -f "$DAEMON_TOML" ]] || error "daemon.toml not found. Run install.sh first."
    [[ -d "$OMNISH_DIR/plugins/$SETUP_PLUGIN" ]] || error "Plugin not found: $SETUP_PLUGIN"
    if setup_plugin "$SETUP_PLUGIN"; then
        ask "Restart omnish-daemon to apply changes? [Y/n]:"
        if [[ ! "${REPLY:-Y}" =~ ^[Nn] ]]; then
            systemctl --user restart omnish-daemon 2>/dev/null \
                && info "omnish-daemon restarted" \
                || warn "Failed to restart daemon. Try: systemctl --user restart omnish-daemon"
        fi
    fi
    exit 0
fi

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
    if [[ -z "$VERSION" ]]; then
        if [[ -x "$EXTRACTED/bin/omnish-daemon" ]]; then
            VERSION="v$("$EXTRACTED/bin/omnish-daemon" --version 2>/dev/null | awk '{print $2}' || echo "unknown")"
        elif [[ -x "$EXTRACTED/bin/omnish" ]]; then
            VERSION="v$("$EXTRACTED/bin/omnish" --version 2>/dev/null | awk '{print $NF}' || echo "unknown")"
        fi
    fi
    info "Installing from local directory: $EXTRACTED"
fi

if [[ "$DRY_RUN" == true ]]; then
    info "[DRY RUN] Would install omnish ${VERSION:-latest} to ${OMNISH_DIR}"
elif [[ -z "${EXTRACTED:-}" ]] && [[ -n "$FROM_DIR" ]]; then
    # Install from local directory containing tar.gz files
    [[ -d "$FROM_DIR" ]] || error "Directory not found: $FROM_DIR"

    # Find matching tar.gz files, sort by version (newest first)
    TAR_FILE=""
    BEST_VERSION=""
    for f in "$FROM_DIR"/omnish-*-${OS}-${ARCH}.tar.gz; do
        [[ -f "$f" ]] || continue
        # Extract version from filename: omnish-<version>-<os>-<arch>.tar.gz
        fname="$(basename "$f")"
        ver="${fname#omnish-}"
        ver="${ver%-${OS}-*}"
        if [[ -z "$VERSION" ]] || [[ "v${ver}" == "$VERSION" ]]; then
            # Compare versions: use sort -V to find the latest
            if [[ -z "$BEST_VERSION" ]] || [[ "$(printf '%s\n%s' "$ver" "$BEST_VERSION" | sort -V | tail -1)" == "$ver" ]]; then
                BEST_VERSION="$ver"
                TAR_FILE="$f"
            fi
        fi
    done

    [[ -n "$TAR_FILE" ]] || error "No matching tar.gz found in $FROM_DIR"
    VERSION="v${BEST_VERSION}"
    info "Found: $(basename "$TAR_FILE")"

    # In upgrade mode, skip if already up to date or package is older
    if [[ "$UPGRADE" == true ]]; then
        CURRENT_VERSION=""
        if [[ -x "$BIN_DIR/omnish-daemon" ]]; then
            CURRENT_VERSION=$("$BIN_DIR/omnish-daemon" --version 2>/dev/null | awk '{print $2}' || echo "")
        elif [[ -x "$BIN_DIR/omnish" ]]; then
            CURRENT_VERSION=$("$BIN_DIR/omnish" --version 2>/dev/null | awk '{print $NF}' || echo "")
        fi
        if [[ -n "$CURRENT_VERSION" ]] && ! is_newer_version "$VERSION" "$CURRENT_VERSION"; then
            info "Already up to date ($(normalize_version "$CURRENT_VERSION"))"
            exit 2
        fi
        info "Update available: $(normalize_version "${CURRENT_VERSION:-unknown}") -> $(normalize_version "$VERSION")"
    fi

    tar -xzf "$TAR_FILE" -C "$TMPDIR"
    EXTRACTED=$(find "$TMPDIR" -maxdepth 1 -type d -name 'omnish-*' | head -1)
    [[ -d "$EXTRACTED" ]] || error "Unexpected archive layout"
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

    # In upgrade mode, skip if already up to date or package is older
    if [[ "$UPGRADE" == true ]]; then
        CURRENT_VERSION=""
        if [[ -x "$BIN_DIR/omnish-daemon" ]]; then
            CURRENT_VERSION=$("$BIN_DIR/omnish-daemon" --version 2>/dev/null | awk '{print $2}' || echo "")
        elif [[ -x "$BIN_DIR/omnish" ]]; then
            CURRENT_VERSION=$("$BIN_DIR/omnish" --version 2>/dev/null | awk '{print $NF}' || echo "")
        fi
        if [[ -n "$CURRENT_VERSION" ]] && ! is_newer_version "$VERSION" "$CURRENT_VERSION"; then
            info "Already up to date ($(normalize_version "$CURRENT_VERSION"))"
            exit 2  # No update needed — daemon skips deploy
        fi
        info "Update available: $(normalize_version "${CURRENT_VERSION:-unknown}") -> $(normalize_version "$VERSION")"
    fi

    TAR_URL="https://github.com/${REPO}/releases/download/${VERSION}/omnish-${VERSION#v}-${OS}-${ARCH}.tar.gz"
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

    # Remove old binaries first (running binaries can't be overwritten: "Text file busy")
    if [[ "$CLIENT_ONLY" == true ]]; then
        # Client-only mode: skip daemon binary
        for f in "$EXTRACTED/bin/"*; do
            fname=$(basename "$f")
            [[ "$fname" == "omnish-daemon" ]] && continue
            rm -f "$BIN_DIR/$fname"
            cp "$f" "$BIN_DIR/"
            chmod 755 "$BIN_DIR/$fname"
        done
    else
        rm -f "$BIN_DIR"/omnish "$BIN_DIR"/omnish-daemon "$BIN_DIR"/omnish-plugin
        cp "$EXTRACTED/bin/"* "$BIN_DIR/"
        chmod 755 "$BIN_DIR"/*
    fi

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

    # Install plugins (preserve existing, add new)
    if [[ -d "$EXTRACTED/plugins" ]]; then
        for plugin_dir in "$EXTRACTED/plugins"/*/; do
            [[ -d "$plugin_dir" ]] || continue
            plugin_name=$(basename "$plugin_dir")
            mkdir -p "$OMNISH_DIR/plugins/$plugin_name"
            cp -f "$plugin_dir"* "$OMNISH_DIR/plugins/$plugin_name/"
            chmod +x "$OMNISH_DIR/plugins/$plugin_name/$plugin_name" 2>/dev/null || true
        done
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

# Locate model_presets.json (release tarball or dev source tree)
PRESETS_JSON=""
if [[ -n "${EXTRACTED:-}" ]] && [[ -f "$EXTRACTED/assets/model_presets.json" ]]; then
    PRESETS_JSON="$EXTRACTED/assets/model_presets.json"
elif [[ -n "${SCRIPT_DIR:-}" ]] && [[ -f "$SCRIPT_DIR/crates/omnish-llm/assets/model_presets.json" ]]; then
    PRESETS_JSON="$SCRIPT_DIR/crates/omnish-llm/assets/model_presets.json"
fi

# Helper: read a provider field from model_presets.json via jq
preset_field() {
    local provider="$1" field="$2" fallback="${3:-}"
    if [[ -n "$PRESETS_JSON" ]] && command -v jq &>/dev/null; then
        local val
        val=$(jq -r --arg p "$provider" --arg f "$field" '.providers[$p][$f] // empty' "$PRESETS_JSON" 2>/dev/null)
        if [[ -n "$val" ]]; then
            echo "$val"
            return
        fi
    fi
    echo "$fallback"
}

# Read provider lists from JSON, fallback to hardcoded defaults
if [[ -n "$PRESETS_JSON" ]] && command -v jq &>/dev/null; then
    readarray -t CHAT_PROVIDERS < <(jq -r '.chat_providers[]' "$PRESETS_JSON")
    readarray -t COMPLETION_PROVIDERS < <(jq -r '.completion_providers[]' "$PRESETS_JSON")
else
    CHAT_PROVIDERS=(anthropic openai gemini openrouter deepseek moonshot-cn moonshot-global custom)
    COMPLETION_PROVIDERS=(gemini openrouter custom)
fi

configure_backend() {
    local purpose="$1"
    local recommended_model="${2:-}"
    local max_chars_override="${3:-}"
    shift 3
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

    local backend_type="$(preset_field "$provider" backend_type "openai-compat")"
    local base_url="$(preset_field "$provider" base_url "")"
    local default_model="$(preset_field "$provider" default_model "")"

    # Use recommended model if provided and provider has no default
    if [[ -n "$recommended_model" ]] && [[ -z "$default_model" ]]; then
        default_model="$recommended_model"
    fi

    local model="" api_key="" cur_base_url="$base_url" cur_backend_type="$backend_type"

    while true; do
        if [[ "$provider" == "custom" ]]; then
            echo "  [1] OpenAI-compatible" >&2
            echo "  [2] Anthropic-compatible" >&2
            local type_default="1"
            [[ "$cur_backend_type" == "anthropic" ]] && type_default="2"
            ask "API type [$type_default]:"
            if [[ "${REPLY:-$type_default}" == "2" ]]; then
                cur_backend_type="anthropic"
            else
                cur_backend_type="openai-compat"
            fi
            if [[ -n "$cur_base_url" ]]; then
                ask "Base URL [$cur_base_url]:"
                cur_base_url="${REPLY:-$cur_base_url}"
            else
                ask "Base URL:"
                cur_base_url="$REPLY"
            fi
        fi

        local model_default="${model:-$default_model}"
        if [[ -n "$model_default" ]]; then
            ask "Model name [$model_default]:"
            model="${REPLY:-$model_default}"
        else
            ask "Model name:"
            model="$REPLY"
        fi

        local key_hint=""
        if [[ -n "$api_key" ]]; then
            key_hint="${api_key:0:4}...${api_key: -4}"
        fi
        if [[ -n "$key_hint" ]]; then
            ask "API key [$key_hint]:"
            api_key="${REPLY:-$api_key}"
        else
            ask "API key:"
            api_key="$REPLY"
        fi

        # Determine context_window (tokens)
        local ctx_window="$(preset_field "$provider" context_window "200000")"
        local max_chars="${max_chars_override:-$ctx_window}"

        # Build TOML snippet
        local toml="[llm.backends.${name}]"$'\n'
        toml+="backend_type = \"${cur_backend_type}\""$'\n'
        toml+="model = \"${model}\""$'\n'
        toml+="api_key_cmd = 'echo \"${api_key}\"'"$'\n'
        if [[ -n "$cur_base_url" ]]; then
            toml+="base_url = \"${cur_base_url}\""$'\n'
        fi
        toml+="context_window = ${max_chars}"$'\n'

        # Preview with masked API key and confirm
        echo "" >&2
        echo "───────────────────────────────────────" >&2
        echo "$toml" | sed 's/echo "\(.\{4\}\).*\(.\{4\}\)"/echo "\1...\2"/' >&2
        echo "───────────────────────────────────────" >&2
        ask "OK? [Y/n]:"
        if [[ "${REPLY:-Y}" =~ ^[Nn] ]]; then
            echo "" >&2
            info "Let's try again..." >&2
            continue
        fi
        break
    done

    # Save backend name for caller
    echo "$name" > "$TMPDIR/last_backend_name"

    # Write TOML section to stdout
    printf '%s\n' "$toml"
}

DAEMON_TOML="$OMNISH_DIR/daemon.toml"

if [[ -f "$DAEMON_TOML" ]] && [[ "$FORCE" != true ]]; then
    info "daemon.toml already exists, skipping LLM configuration (use --force to overwrite)"
    # Still need LISTEN_CHOICE/LISTEN_ADDR for client deployment
    LISTEN_ADDR=$(grep '^listen_addr' "$DAEMON_TOML" 2>/dev/null | sed 's/.*= *"\(.*\)"/\1/' || echo "")
    if [[ "$LISTEN_ADDR" == *:* ]]; then
        LISTEN_CHOICE="2"
        # Resolve SERVER_IP from existing client.toml or hostname
        if [[ -f "$OMNISH_DIR/client.toml" ]]; then
            SERVER_IP=$(grep '^daemon_addr' "$OMNISH_DIR/client.toml" 2>/dev/null | sed 's/.*= *"\(.*\):.*/\1/' || echo "")
        fi
        if [[ -z "${SERVER_IP:-}" ]]; then
            SERVER_IP=$(hostname -I 2>/dev/null | awk '{print $1}' || echo "<server-ip>")
        fi
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
    configure_backend "chat/analysis" "" "" "${CHAT_PROVIDERS[@]}" > "$TMPDIR/chat_backend.toml"
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
        configure_backend "completion" "Qwen/Qwen2.5-Coder-32B-Instruct" "64000" "${COMPLETION_PROVIDERS[@]}" > "$TMPDIR/completion_backend.toml"
        COMPLETION_NAME=$(cat "$TMPDIR/last_backend_name")
        USE_CASES="[llm.use_cases]
chat = \"${CHAT_NAME}\"
analysis = \"${CHAT_NAME}\"
completion = \"${COMPLETION_NAME}\"
summarize = \"${CHAT_NAME}\""
    else
        USE_CASES="[llm.use_cases]
chat = \"${CHAT_NAME}\"
analysis = \"${CHAT_NAME}\"
completion = \"${CHAT_NAME}\"
summarize = \"${CHAT_NAME}\""
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

        # Select server IP for client.toml
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
    CHECK_URL_LINE=""
    if [[ -n "$FROM_DIR" ]]; then
        CHECK_URL_LINE="check_url = \"${FROM_DIR}\""
    else
        CHECK_URL_LINE="# check_url = \"https://api.github.com/repos/yrlihuan/omnish/releases/latest\""
    fi

    {
        cat << DAEMON_EOF
# omnish daemon configuration

listen_addr = "${LISTEN_ADDR}"

[llm]
default = "${CHAT_NAME}"

${USE_CASES}

DAEMON_EOF
        cat "$TMPDIR/chat_backend.toml"
        if [[ -f "$TMPDIR/completion_backend.toml" ]]; then
            cat "$TMPDIR/completion_backend.toml"
        fi
        cat << DAEMON_EOF

# Optional: Langfuse observability (LLM tracing & analytics)
# [llm.langfuse]
# public_key = "pk-lf-..."
# secret_key = "sk-lf-..."
# base_url = "https://cloud.langfuse.com"


[context.completion]
# detailed_commands = 30   # recent commands with full output
# history_commands = 500   # older commands as command-line only
# head_lines = 20          # output lines from start of each command
# tail_lines = 20          # output lines from end of each command
# max_line_width = 200     # max characters per output line
# max_context_chars = 8000 # fallback context limit

# [context.hourly_summary]
# head_lines = 50
# tail_lines = 100
# max_line_width = 128

[tasks.eviction]
# session_evict_hours = 48

[tasks.daily_notes]
# enabled = true
# schedule_hour = 23

[tasks.disk_cleanup]
# schedule = "0 0 */6 * * *"

[tasks.auto_update]
enabled = ${AUTO_UPDATE_ENABLED}
# schedule = "0 0 4 * * *"
${CHECK_URL_LINE}

# [plugins]
# enabled = []
DAEMON_EOF
    } > "$TMPDIR/daemon.toml"

    if [[ "$DRY_RUN" == true ]]; then
        info "[DRY RUN] Would write to $DAEMON_TOML"
    else
        cp "$TMPDIR/daemon.toml" "$DAEMON_TOML"
        chmod 600 "$DAEMON_TOML"
        info "Written: $DAEMON_TOML"
    fi
fi

# ── Plugin setup ──────────────────────────────────────────────────────────

if [[ "$DRY_RUN" != true ]] && [[ -f "$DAEMON_TOML" ]]; then
    for plugin_dir in "$OMNISH_DIR/plugins"/*/; do
        [[ -d "$plugin_dir" ]] || continue
        plugin_name=$(basename "$plugin_dir")
        [[ "$plugin_name" == "builtin" ]] && continue
        # Skip if already configured
        if grep -q "^\[tools\.$plugin_name\]" "$DAEMON_TOML" 2>/dev/null; then
            continue
        fi
        setup_plugin "$plugin_name" || true
    done
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
    if [[ "$LISTEN_CHOICE" == "2" ]] && [[ -n "${SERVER_IP:-}" ]]; then
        LISTEN_PORT="${LISTEN_ADDR##*:}"
        CLIENT_DAEMON_ADDR="${SERVER_IP}:${LISTEN_PORT}"
    else
        CLIENT_DAEMON_ADDR="${LISTEN_ADDR:-${OMNISH_DIR}/omnish.sock}"
    fi

    if [[ "$DRY_RUN" == true ]]; then
        info "[DRY RUN] Would write client.toml (daemon_addr = \"${CLIENT_DAEMON_ADDR}\")"
    else
        cat > "$CLIENT_TOML" << EOF
# omnish client configuration

# Daemon address (Unix socket path or host:port for TCP)
daemon_addr = "${CLIENT_DAEMON_ADDR}"

# Enable inline ghost-text completion
completion_enabled = true

# Auto-update: client checks binary mtime and re-execs if updated
auto_update = ${AUTO_UPDATE_ENABLED:-true}

# First-run onboarding completed
# onboarded = false

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

    deploy_client() {
        local target="$1"
        local remote_home="~/.omnish"

        info "Deploying to ${target}..."

        local SSH_OPTS="-o BatchMode=yes"

        # Create directories and remove old binaries (running binaries can't be overwritten)
        ssh -n $SSH_OPTS "$target" "mkdir -p ${remote_home}/bin ${remote_home}/tls && rm -f ${remote_home}/bin/omnish ${remote_home}/bin/omnish-plugin" \
            || { warn "SSH connection failed for ${target}"; return 1; }

        # Copy binaries
        scp -q $SSH_OPTS "${BIN_DIR}/omnish" "${BIN_DIR}/omnish-plugin" "${target}:${remote_home}/bin/" \
            || { warn "Failed to copy binaries to ${target}"; return 1; }

        # Copy TLS cert and auth token
        scp -q $SSH_OPTS "${OMNISH_DIR}/tls/cert.pem" "${target}:${remote_home}/tls/" \
            || { warn "Failed to copy TLS cert to ${target}"; return 1; }
        scp -q $SSH_OPTS "${OMNISH_DIR}/auth_token" "${target}:${remote_home}/" \
            || { warn "Failed to copy auth token to ${target}"; return 1; }

        # Copy client.toml and set permissions
        scp -q $SSH_OPTS "${OMNISH_DIR}/client.toml" "${target}:${remote_home}/" \
            || { warn "Failed to copy client.toml to ${target}"; return 1; }
        ssh -n $SSH_OPTS "$target" "chmod 600 ${remote_home}/client.toml ${remote_home}/auth_token" \
            || { warn "Failed to set permissions on ${target}"; return 1; }

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
                CLIENT_COUNT=$((CLIENT_COUNT + 1))
                CLIENT_HOST="$REPLY"

                # Verify SSH connectivity
                info "Checking SSH connectivity to ${CLIENT_HOST}..."
                if ssh -n -o ConnectTimeout=5 -o BatchMode=yes "$CLIENT_HOST" true 2>/dev/null; then
                    info "SSH OK: ${CLIENT_HOST}"
                    deploy_client "$CLIENT_HOST" && DEPLOYED_CLIENTS+=("$CLIENT_HOST") || true
                else
                    warn "Cannot connect to ${CLIENT_HOST} via SSH, skipping"
                fi
            done
        fi
    fi

    # Add client list to existing [tasks.auto_update] section in daemon.toml
    if [[ ${#DEPLOYED_CLIENTS[@]} -gt 0 ]] && [[ -f "$DAEMON_TOML" ]]; then
        # Build TOML array string: clients = ["user@host1", "user@host2"]
        CLIENTS_TOML="clients = ["
        for i in "${!DEPLOYED_CLIENTS[@]}"; do
            (( i > 0 )) && CLIENTS_TOML+=", "
            CLIENTS_TOML+="\"${DEPLOYED_CLIENTS[$i]}\""
        done
        CLIENTS_TOML+="]"
        # Insert clients line after check_url (unique in the file, inside [tasks.auto_update])
        sed -i "/check_url/a ${CLIENTS_TOML}" "$DAEMON_TOML"
        # Ensure auto_update is enabled (only match within [tasks.auto_update] section)
        sed -i '/^\[tasks\.auto_update\]/,/^\[/{s/^enabled = false$/enabled = true/}' "$DAEMON_TOML"
        info "Auto-update enabled with ${#DEPLOYED_CLIENTS[@]} client(s) in daemon.toml"
    fi

fi

echo ""
if [[ "$UPGRADE" == true ]] || { [[ -n "${OLD_VERSION:-}" ]] && [[ "$OLD_VERSION" != "${VERSION#v}" ]]; }; then
    info "Upgrade complete! (v${OLD_VERSION:-unknown} → ${VERSION})"
else
    info "Installation complete! (omnish ${VERSION})"
    echo ""
    echo "To get started, run:"
    if echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
        echo "  omnish"
    else
        echo "  ${BIN_DIR}/omnish"
    fi
fi
exit
}
