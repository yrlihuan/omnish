# Plugin Setup Protocol Design

Issue: #358 — Plugin installation configuration protocol

## Overview

Plugins declare their configuration needs in a `setup.json` manifest. During installation, `install.sh` reads each plugin's manifest, prompts the user for required parameters, writes the values into `daemon.toml`, and auto-enables the plugin. A `--setup-plugin <name>` flag allows on-demand reconfiguration of individual plugins.

## setup.json Format

Each plugin directory may contain a `setup.json` alongside `tool.json` and the executable:

```
~/.omnish/plugins/web_search/
├── web_search          # executable
├── tool.json           # tool definition
└── setup.json          # installation manifest (optional)
```

```json
{
  "params": [
    {
      "name": "api_key",
      "prompt": "Brave Search API key",
      "required": true,
      "secret": true
    },
    {
      "name": "base_url",
      "prompt": "API base URL",
      "default": "https://api.search.brave.com/res/v1/web/search"
    }
  ]
}
```

### Param fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Key under `[tools.<plugin_name>]` in daemon.toml |
| `prompt` | string | yes | Text shown to the user during setup |
| `required` | bool | no | If true, plugin setup is aborted when user provides empty value. Default: false |
| `secret` | bool | no | If true, use `read -rs` for silent input and mask value in info message (show first 4 + last 4 chars). Default: false |
| `default` | string | no | Default value shown in prompt brackets, used if user presses Enter |

## install.sh Changes

### New function: `setup_plugin`

```bash
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
        local name prompt required secret default_val
        name=$(jq -r ".params[$i].name" "$setup_file")
        prompt=$(jq -r ".params[$i].prompt" "$setup_file")
        required=$(jq -r ".params[$i].required // false" "$setup_file")
        secret=$(jq -r ".params[$i].secret // false" "$setup_file")
        default_val=$(jq -r ".params[$i].default // empty" "$setup_file")

        if [[ "$secret" == "true" ]]; then
            printf '\033[1;32m?\033[0m %s ' "$prompt" >&2
            read -rs REPLY </dev/tty
            echo "" >&2  # newline after silent input
            PARAM_VALUES[$name]="$REPLY"
        elif [[ -n "$default_val" ]]; then
            ask "$prompt [$default_val]:"
            PARAM_VALUES[$name]="${REPLY:-$default_val}"
        else
            ask "$prompt:"
            PARAM_VALUES[$name]="$REPLY"
        fi

        if [[ "$required" == "true" ]] && [[ -z "${PARAM_VALUES[$name]}" ]]; then
            warn "Required parameter '$name' not provided, skipping $plugin_name"
            info "Run install.sh --setup-plugin $plugin_name to configure later"
            return 1
        fi
    done

    # Write [tools.<plugin_name>] section to daemon.toml
    patch_toml_section "$daemon_toml" "$plugin_name" PARAM_VALUES

    # Add to [plugins] enabled
    patch_plugins_enabled "$daemon_toml" "$plugin_name"

    info "Plugin $plugin_name configured and enabled"
}
```

### TOML patching functions

#### `patch_toml_section` — write `[tools.<name>]` section

```bash
patch_toml_section() {
    local toml_file="$1"
    local plugin_name="$2"
    local -n values=$3
    local section_header="[tools.$plugin_name]"

    # Remove existing section if present (from header to next [section] or EOF)
    if grep -q "^\[tools\.$plugin_name\]" "$toml_file"; then
        sed -i "/^\[tools\.$plugin_name\]/,/^\[/{/^\[tools\.$plugin_name\]/d;/^\[/!d}" "$toml_file"
    fi

    # Also remove commented-out section and its commented keys
    if grep -q "^# *\[tools\.$plugin_name\]" "$toml_file"; then
        sed -i "/^# *\[tools\.$plugin_name\]/,/^[^#]/{/^# *\[tools\.$plugin_name\]/d;/^# /d}" "$toml_file"
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
```

#### `patch_plugins_enabled` — add plugin to enabled array

```bash
patch_plugins_enabled() {
    local toml_file="$1"
    local plugin_name="$2"

    if ! grep -q '^\[plugins\]' "$toml_file"; then
        # No [plugins] section — check for commented one
        if grep -q '^# *\[plugins\]' "$toml_file"; then
            # Uncomment and set
            sed -i 's/^# *\[plugins\]/[plugins]/' "$toml_file"
            sed -i 's/^# *enabled = \[.*\]/enabled = ["'"$plugin_name"'"]/' "$toml_file"
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
```

### Integration points

#### Initial install (after daemon.toml is written, ~line 599)

```bash
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
```

#### New CLI flag `--setup-plugin=<name>`

Added to argument parsing:

```bash
--setup-plugin=*) SETUP_PLUGIN="${arg#*=}" ;;
```

When set, skip all other steps and run `setup_plugin` directly:

```bash
if [[ -n "${SETUP_PLUGIN:-}" ]]; then
    DAEMON_TOML="$OMNISH_DIR/daemon.toml"
    [[ -f "$DAEMON_TOML" ]] || error "daemon.toml not found. Run install.sh first."
    [[ -d "$OMNISH_DIR/plugins/$SETUP_PLUGIN" ]] || error "Plugin not found: $SETUP_PLUGIN"
    setup_plugin "$SETUP_PLUGIN"
    exit 0
fi
```

This early-exit goes right after argument parsing, before download/install steps.

### Skipping rules

| Scenario | Behavior |
|----------|----------|
| `--upgrade` | Skip plugin setup entirely (exit before reaching setup code) |
| Initial install, `[tools.<name>]` exists | Skip that plugin (already configured) |
| Initial install, no `setup.json` | Skip that plugin (nothing to configure) |
| `--setup-plugin <name>` | Always run setup, overwrite existing `[tools.<name>]` |
| `jq` not installed | Warn and skip plugin setup |
| Required param empty | Skip that plugin, suggest `--setup-plugin` |

## web_search setup.json

```json
{
  "params": [
    {
      "name": "api_key",
      "prompt": "Brave Search API key",
      "required": true,
      "secret": true
    }
  ]
}
```

Only `api_key` is required. `base_url` uses the script's built-in default and doesn't need setup.

## Data Flow

```
install.sh
  │
  ├─ Parse args (--setup-plugin=X → early exit to setup_plugin)
  ├─ Download/install binaries
  ├─ Copy plugins to ~/.omnish/plugins/
  ├─ LLM config wizard → writes daemon.toml
  │
  ▼
Plugin setup (after daemon.toml exists):
  │
  For each plugin with setup.json (skip builtin, skip already configured):
  │
  ├─ jq reads setup.json params
  ├─ Prompt user for each param
  ├─ If required param empty → skip plugin, suggest --setup-plugin
  │
  ├─ patch_toml_section: write [tools.<name>] to daemon.toml
  ├─ patch_plugins_enabled: add to [plugins] enabled array
  │
  ▼
Plugin configured and enabled
```

## Error Handling

- **`jq` not installed:** warn and skip plugin setup. Plugins are still copied, just not configured.
- **User skips required param:** skip that plugin, print message suggesting `--setup-plugin <name>` later.
- **daemon.toml doesn't exist:** for `--setup-plugin`, error and exit. For initial install, setup runs after daemon.toml is written, so it will always exist.
- **Malformed setup.json:** jq returns errors, setup_plugin returns non-zero, continues to next plugin.
