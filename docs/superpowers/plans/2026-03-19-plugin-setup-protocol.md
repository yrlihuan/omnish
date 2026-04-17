# Plugin Setup Protocol Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let plugins declare their configuration needs in `setup.json`, so `install.sh` can prompt users and auto-configure `daemon.toml` during installation.

**Architecture:** Plugins include a declarative `setup.json` manifest listing required parameters. `install.sh` reads this with `jq`, prompts the user, patches `daemon.toml` with sed, and auto-enables the plugin. A `--setup-plugin=<name>` flag provides on-demand reconfiguration.

**Tech Stack:** Bash, jq, sed

**Spec:** `docs/superpowers/specs/2026-03-19-plugin-setup-protocol-design.md`

---

## File Structure

- Modify: `install.sh` - add `setup_plugin`, `patch_toml_section`, `patch_plugins_enabled` functions, `--setup-plugin` flag, plugin setup loop
- Create: `plugins/web_search/setup.json` - web_search plugin manifest

---

### Task 1: Create web_search setup.json

**Files:**
- Create: `plugins/web_search/setup.json`

- [ ] **Step 1: Create setup.json**

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

- [ ] **Step 2: Verify it's valid JSON**

Run: `jq . plugins/web_search/setup.json`
Expected: pretty-printed JSON output, no errors

- [ ] **Step 3: Commit**

```bash
git add plugins/web_search/setup.json
git commit -m "feat: add setup.json for web_search plugin (#358)"
```

---

### Task 2: Add TOML patching functions to install.sh

These are helper functions that will be used by `setup_plugin`. They go in the `# ── Helpers` section of `install.sh`, after the existing `ask()` function (line 48).

**Files:**
- Modify: `install.sh:44-48` (after helpers section)

- [ ] **Step 1: Add `patch_toml_section` function**

Insert after line 48 (`ask()` definition):

```bash
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

- [ ] **Step 2: Add `patch_plugins_enabled` function**

Insert immediately after `patch_toml_section`:

```bash
# Add a plugin name to [plugins] enabled array in a TOML file
patch_plugins_enabled() {
    local toml_file="$1"
    local plugin_name="$2"

    if ! grep -q '^\[plugins\]' "$toml_file"; then
        # No [plugins] section - check for commented one
        if grep -q '^# *\[plugins\]' "$toml_file"; then
            # Uncomment and set
            sed -i 's/^# *\[plugins\]/[plugins]/' "$toml_file"
            sed -i 's/^# *enabled = \[.*\]/enabled = ["'"$plugin_name"'"]/' "$toml_file"
        else
            # Append new section
            printf '\n[plugins]\nenabled = ["%s"]\n' "$plugin_name" >> "$toml_file"
        fi
    else
        # [plugins] section exists - grep for enabled= scoped to [plugins] section only
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

- [ ] **Step 3: Test the patching functions manually**

Create a test TOML file and verify the functions work:

```bash
# Create a test file mimicking daemon.toml
cat > /tmp/test_daemon.toml << 'EOF'
[tasks.auto_update]
enabled = true

# [plugins]
# enabled = []

# Per-tool parameter injection
# [tools.web_search]
# api_key = "BSAxxxxxxxx"
# base_url = "https://api.search.brave.com/res/v1/web/search"
EOF

# Source install.sh helpers (just the functions)
source <(sed -n '/^info()/,/^$/p; /^warn()/,/^$/p; /^ask()/,/^$/p; /^patch_toml_section/,/^}/p; /^patch_plugins_enabled/,/^}/p' install.sh)

# Test patch_toml_section
declare -A TEST_PARAMS=([api_key]="BSA_test_key_123")
patch_toml_section /tmp/test_daemon.toml web_search TEST_PARAMS
cat /tmp/test_daemon.toml
# Expected: [tools.web_search] section with api_key = "BSA_test_key_123" at end

# Test patch_plugins_enabled
patch_plugins_enabled /tmp/test_daemon.toml web_search
cat /tmp/test_daemon.toml
# Expected: [plugins] uncommented with enabled = ["web_search"]

# Test idempotency - running again should not duplicate
patch_plugins_enabled /tmp/test_daemon.toml web_search
grep enabled /tmp/test_daemon.toml
# Expected: still just enabled = ["web_search"], not duplicated

rm /tmp/test_daemon.toml
```

- [ ] **Step 4: Commit**

```bash
git add install.sh
git commit -m "feat: add TOML patching helpers to install.sh (#358)"
```

---

### Task 3: Add `setup_plugin` function and `--setup-plugin` flag

**Files:**
- Modify: `install.sh:68-111` (argument parsing) and after helpers

- [ ] **Step 1: Add `setup_plugin` function**

Insert after the `patch_plugins_enabled` function (still in the helpers section):

```bash
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
            info "Run install.sh --setup-plugin=$plugin_name to configure later"
            return 1
        fi
    done

    # Write [tools.<plugin_name>] section to daemon.toml
    patch_toml_section "$daemon_toml" "$plugin_name" PARAM_VALUES

    # Add to [plugins] enabled
    patch_plugins_enabled "$daemon_toml" "$plugin_name"

    # Show confirmation with secret masking
    for key in "${!PARAM_VALUES[@]}"; do
        local val="${PARAM_VALUES[$key]}"
        # Check if this param is secret
        local is_secret
        is_secret=$(jq -r ".params[] | select(.name == \"$key\") | .secret // false" "$setup_file")
        if [[ "$is_secret" == "true" ]] && [[ ${#val} -ge 8 ]]; then
            info "  $key = ${val:0:4}...${val: -4}"
        elif [[ "$is_secret" == "true" ]]; then
            info "  $key = ****"
        else
            info "  $key = $val"
        fi
    done

    info "Plugin $plugin_name configured and enabled"
}
```

- [ ] **Step 2: Add `--setup-plugin` to argument parsing**

In the `case "$arg"` block (around line 69-110), add before `--help|-h)`:

```bash
        --setup-plugin=*) SETUP_PLUGIN="${arg#*=}" ;;
```

Also initialize the variable after the other initializations (line 67, after `FROM_DIR=""`):

```bash
SETUP_PLUGIN=""
```

- [ ] **Step 3: Add `--setup-plugin` to help text**

In the help text block (lines 98-107), add:

```bash
            echo "  --setup-plugin=<name> Configure (or reconfigure) a specific plugin"
```

- [ ] **Step 4: Add early-exit handler for `--setup-plugin`**

Insert after the argument parsing block (after line 111, before the dry-run check):

```bash
# ── Plugin setup (standalone mode) ──────────────────────────────────────────
if [[ -n "$SETUP_PLUGIN" ]]; then
    DAEMON_TOML="$OMNISH_DIR/daemon.toml"
    [[ -f "$DAEMON_TOML" ]] || error "daemon.toml not found. Run install.sh first."
    [[ -d "$OMNISH_DIR/plugins/$SETUP_PLUGIN" ]] || error "Plugin not found: $SETUP_PLUGIN"
    setup_plugin "$SETUP_PLUGIN"
    exit 0
fi
```

- [ ] **Step 5: Commit**

```bash
git add install.sh
git commit -m "feat: add setup_plugin function and --setup-plugin flag (#358)"
```

---

### Task 4: Add plugin setup loop to initial install flow

**Files:**
- Modify: `install.sh:600` (after daemon.toml is written, before credentials generation)

- [ ] **Step 1: Add plugin setup loop**

Insert after line 600 (`fi` closing the daemon.toml config block), before the `# ── Generate credentials` section:

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

Note: This runs only during initial install (or `--force`), not during `--upgrade` (which exits at line 287 before reaching this code).

- [ ] **Step 2: Verify install.sh syntax**

Run: `bash -n install.sh`
Expected: no output (no syntax errors)

- [ ] **Step 3: End-to-end manual test**

Test the full flow with a temporary OMNISH_DIR:

```bash
# Setup test environment
export OMNISH_HOME=/tmp/test_omnish
mkdir -p "$OMNISH_HOME/plugins/web_search"
cp plugins/web_search/* "$OMNISH_HOME/plugins/web_search/"

# Create a minimal daemon.toml (simulating post-wizard state)
cat > "$OMNISH_HOME/daemon.toml" << 'EOF'
[llm]
default = "test"

# [plugins]
# enabled = []

# [tools.web_search]
# api_key = "BSAxxxxxxxx"
EOF

# Test --setup-plugin
bash install.sh --setup-plugin=web_search
# Enter an API key when prompted
# Verify: cat "$OMNISH_HOME/daemon.toml" should show:
#   [plugins]
#   enabled = ["web_search"]
#   [tools.web_search]
#   api_key = "<your_input>"

# Cleanup
rm -rf /tmp/test_omnish
unset OMNISH_HOME
```

- [ ] **Step 4: Commit**

```bash
git add install.sh
git commit -m "feat: add plugin setup loop during installation (#358)"
```
