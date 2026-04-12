#!/usr/bin/env bash
#
# test_sandbox_rules.sh - Integration tests for sandbox permit rules (#522).
#
# Tests:
#   1-3. UI structure: Sandbox submenu layout, single Add form, Scope selector
#   4.   Global rule: add via /config, verify in daemon.toml and menu, delete
#   5.   Local rule:  add via /config, verify in client.toml and menu, delete

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Sandbox submenu structure: Rules appears after availability labels
  2. Rules submenu: single "Add permit rule" entry
  3. Add permit rule form has Scope selector as first field
  4. Global sandbox permit rule: add via /config, verify in daemon.toml and menu, delete
  5. Local sandbox permit rule: add via /config, verify in client.toml and menu, delete
EOF
}

test_init "sandbox-rules" "$@"

DAEMON_TOML="${OMNISH_HOME:-$HOME/.omnish}/daemon.toml"
CLIENT_TOML="${OMNISH_CLIENT_CONFIG:-$HOME/.omnish/client.toml}"

# Helper: enter chat and open /config
open_config() {
    enter_chat
    send_keys "/config" 0.3
    send_enter 1
}

# Helper: navigate down N times
nav_down() {
    local n="${1:-1}"
    for _ in $(seq 1 "$n"); do
        send_special Down 0.3
    done
}

# Helper: navigate to Sandbox from Config top level.
# Config items: General, LLM, Tasks, Plugins, Sandbox → Down×4 + Enter
enter_sandbox() {
    nav_down 4
    send_enter 0.5
    sleep 0.3
}

# Helper: navigate to Rules inside Sandbox submenu.
# Sandbox items: Enabled, Backend, (availability labels), Rules
# Rules is the last interactive item — keep pressing Down until hint says "open".
enter_rules() {
    local found=false
    for _ in $(seq 1 8); do
        send_special Down 0.3
    done
    sleep 0.3
    local content
    content=$(capture_pane -20)
    if echo "$content" | grep -q "Enter open"; then
        send_enter 0.5
        sleep 0.3
        return 0
    fi
    return 1
}

# ── Test 1: Sandbox submenu structure ────────────────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Sandbox submenu structure ===${NC}"

    restart_client
    wait_for_client
    open_config

    local content
    content=$(capture_pane -20)
    show_capture "Config top level" "$content" 12

    # Navigate to Sandbox: Down×4 (General→LLM→Tasks→Plugins→Sandbox) + Enter
    enter_sandbox
    content=$(capture_pane -20)
    show_capture "Sandbox submenu" "$content" 12

    # Verify structure: Enabled, Backend, availability label(s), then Rules
    local enabled_ln backend_ln avail_ln rules_ln
    enabled_ln=$(echo "$content" | grep -n "Enabled" | head -1 | cut -d: -f1)
    backend_ln=$(echo "$content" | grep -n "Backend" | head -1 | cut -d: -f1)
    avail_ln=$(echo "$content" | grep -n -i "available\|not available" | head -1 | cut -d: -f1)
    rules_ln=$(echo "$content" | grep -n "Rules" | head -1 | cut -d: -f1)

    if [ -z "$enabled_ln" ]; then
        assert_fail "Enabled not found in Sandbox submenu"
        send_special Escape 0.5; send_special Escape 0.5
        return 1
    fi
    if [ -z "$rules_ln" ]; then
        assert_fail "Rules not found in Sandbox submenu"
        send_special Escape 0.5; send_special Escape 0.5
        return 1
    fi

    # Rules must appear after availability labels
    if [ -n "$avail_ln" ] && [ "$rules_ln" -le "$avail_ln" ]; then
        assert_fail "Rules (line $rules_ln) appears before availability label (line $avail_ln)"
        send_special Escape 0.5; send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}Sandbox structure correct: Enabled, Backend, availability, Rules${NC}"

    send_special Escape 0.5
    send_special Escape 0.5

    assert_pass "Sandbox submenu structure correct"
    return 0
}

# ── Test 2: Rules submenu has single "Add permit rule" ───────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Rules submenu: single Add permit rule ===${NC}"

    restart_client
    wait_for_client
    open_config
    enter_sandbox

    if ! enter_rules; then
        assert_fail "Could not enter Rules submenu"
        send_special Escape 0.5; send_special Escape 0.5
        return 1
    fi

    local content
    content=$(capture_pane -20)
    show_capture "Rules submenu" "$content" 12

    # Should have exactly one "Add permit rule" (not "Add local" and "Add global")
    local add_count
    add_count=$(echo "$content" | grep -c "Add permit rule")
    if [ "$add_count" -ne 1 ]; then
        assert_fail "Expected 1 'Add permit rule', found $add_count"
        # Also check for split add forms
        local add_local add_global
        add_local=$(echo "$content" | grep -c "Add local")
        add_global=$(echo "$content" | grep -c "Add global")
        if [ "$add_local" -gt 0 ] || [ "$add_global" -gt 0 ]; then
            echo -e "  ${RED}Found separate Add local ($add_local) / Add global ($add_global) forms${NC}"
        fi
        send_special Escape 0.5; send_special Escape 0.5; send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}Single 'Add permit rule' found${NC}"

    # Existing rules should show [local] or [global] suffix
    # (may or may not have existing rules, so just verify no bare duplicates)

    send_special Escape 0.5
    send_special Escape 0.5
    send_special Escape 0.5

    assert_pass "Rules submenu has single Add permit rule"
    return 0
}

# ── Test 3: Add permit rule form has Scope selector first ────────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: Add permit rule form has Scope selector ===${NC}"

    restart_client
    wait_for_client
    open_config
    enter_sandbox

    if ! enter_rules; then
        assert_fail "Could not enter Rules submenu"
        send_special Escape 0.5; send_special Escape 0.5
        return 1
    fi

    # "Add permit rule" should be the first interactive item in Rules.
    # It's a submenu — press Enter to open it.
    send_enter 0.5
    sleep 0.3
    local content
    content=$(capture_pane -20)
    show_capture "Add permit rule form" "$content" 12

    # First field should be Scope with [local] or [global] selector
    if ! echo "$content" | grep -q "Scope"; then
        assert_fail "Scope field not found in Add permit rule form"
        send_special Escape 0.5; send_special Escape 0.5; send_special Escape 0.5; send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}Scope field found${NC}"

    # Scope should appear before Plugin
    local scope_ln plugin_ln
    scope_ln=$(echo "$content" | grep -n "Scope" | head -1 | cut -d: -f1)
    plugin_ln=$(echo "$content" | grep -n "Plugin" | head -1 | cut -d: -f1)
    if [ -n "$plugin_ln" ] && [ "$scope_ln" -ge "$plugin_ln" ]; then
        assert_fail "Scope (line $scope_ln) not before Plugin (line $plugin_ln)"
        send_special Escape 0.5; send_special Escape 0.5; send_special Escape 0.5; send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}Scope appears before Plugin${NC}"

    # Verify other fields present: Param name, Operator, Pattern
    local ok=true
    for field in "Param name" "Operator" "Pattern"; do
        if ! echo "$content" | grep -q "$field"; then
            echo -e "  ${RED}Missing field: $field${NC}"
            ok=false
        fi
    done

    send_special Escape 0.5
    send_special Escape 0.5
    send_special Escape 0.5
    send_special Escape 0.5

    if ! $ok; then
        assert_fail "Add permit rule form missing required fields"
        return 1
    fi

    assert_pass "Add permit rule form has Scope selector as first field"
    return 0
}

# ── Shared helper: from Config top-level, navigate to Sandbox > Rules ───────
navigate_to_rules() {
    enter_sandbox
    enter_rules
}

# ── Test 4: Global rule add / verify / delete ──────────────────────────────

test_4() {
    echo -e "\n${YELLOW}=== Test 4: Global sandbox permit rule add/delete ===${NC}"

    restart_client
    wait_for_client

    # ── Step 1: Add global permit rule via /config ──
    echo -e "  ${YELLOW}--- Step 1: Add global permit rule via /config ---${NC}"
    open_config
    navigate_to_rules

    local content
    content=$(capture_pane -25)
    show_capture "Rules submenu (before add)" "$content" 15

    # Cursor on "Add permit rule" (first item). Enter -> open form.
    send_enter 0.8

    content=$(capture_pane -25)
    show_capture "Add form (initial)" "$content" 15

    # Form field order: Scope / Plugin / Param name / Operator / Pattern / Done
    # Cursor starts on Scope (Select, default "local"). Change to "global".
    send_enter 0.5          # open Scope picker
    send_special Down 0.2   # local -> global
    send_enter 0.5          # confirm
    send_special Down 0.3   # advance to Plugin (TextInput, auto-edit)
    send_keys "bash" 0.3
    send_enter 0.5
    send_keys "command" 0.3
    send_enter 0.5
    # Operator (Select) -- press Enter to open picker
    send_enter 0.5
    # Down x3 -> matches (starts_with, contains, equals, matches)
    send_special Down 0.2
    send_special Down 0.2
    send_special Down 0.2
    send_enter 0.5
    send_keys "sudo route.*" 0.3
    send_enter 0.5

    content=$(capture_pane -25)
    show_capture "Add form (filled)" "$content" 15

    # Cursor on Done button -> Enter to submit
    send_enter 1.5

    # Verify rule persisted to daemon.toml
    sleep 1
    if grep -q 'command matches sudo route\.\*' "$DAEMON_TOML"; then
        assert_pass "Step 1: global rule persisted in daemon.toml"
    else
        show_capture "daemon.toml contents" "$(cat "$DAEMON_TOML")" 30
        assert_fail "Step 1: global rule not found in daemon.toml"
        return 1
    fi

    # ── Step 2: Verify rule visible in Rules menu ──
    echo -e "  ${YELLOW}--- Step 2: Verify rule visible in Rules menu ---${NC}"
    sleep 0.5
    navigate_to_rules

    content=$(capture_pane -25)
    show_capture "Rules submenu (verify)" "$content" 15

    if echo "$content" | grep -q "bash command matches sudo route\\.\\* \\[global\\]"; then
        assert_pass "Step 2: global rule visible in Rules menu with [global] tag"
    else
        assert_fail "Step 2: global rule not visible in Rules menu"
        return 1
    fi

    # ── Step 3: Delete the rule via edit form ──
    echo -e "  ${YELLOW}--- Step 3: Delete global rule ---${NC}"

    # Reach the newly added rule (last global rule).
    local i
    for i in $(seq 1 20); do
        send_special Down 0.15
    done
    sleep 0.3

    content=$(capture_pane -25)
    show_capture "At global rule" "$content" 15
    if ! echo "$content" | grep -q "sudo route"; then
        assert_fail "Step 3: could not navigate to global rule entry"
        return 1
    fi

    # Enter the edit form
    send_enter 0.8

    # Edit form: Scope(L) / Plugin(L) / Param name(T) / Operator(S) / Pattern(T) / Delete(B)
    # Cursor starts at Param name (first_interactive skips Labels).
    # Edit forms don't auto-edit — navigate to Delete button directly.
    send_special Down 0.3   # Param name -> Operator
    send_special Down 0.3   # Operator -> Pattern
    send_special Down 0.3   # Pattern -> Delete button
    send_enter 0.5          # press Delete button -> submits form with _delete=true

    sleep 1.5

    if grep -q 'command matches sudo route\.\*' "$DAEMON_TOML"; then
        assert_fail "Step 3: global rule still present in daemon.toml after delete"
        return 1
    else
        assert_pass "Step 3: global rule removed from daemon.toml"
    fi

    return 0
}

# ── Test 5: Local rule add / verify / delete ───────────────────────────────

test_5() {
    echo -e "\n${YELLOW}=== Test 5: Local sandbox permit rule add/delete ===${NC}"

    restart_client
    wait_for_client

    # ── Step 1: Add local permit rule via /config ──
    echo -e "  ${YELLOW}--- Step 1: Add local permit rule via /config ---${NC}"
    open_config
    navigate_to_rules

    local content
    content=$(capture_pane -25)
    show_capture "Rules submenu (before add)" "$content" 15

    # Cursor on "Add permit rule". Enter -> open form.
    send_enter 0.8

    content=$(capture_pane -25)
    show_capture "Add form (initial)" "$content" 15

    # Scope defaults to "local" -- press Down to skip.
    send_special Down 0.3
    send_keys "bash" 0.3
    send_enter 0.5
    send_keys "command" 0.3
    send_enter 0.5
    send_enter 0.5
    send_special Down 0.2
    send_special Down 0.2
    send_special Down 0.2
    send_enter 0.5
    send_keys "sudo route.*" 0.3
    send_enter 0.5

    content=$(capture_pane -25)
    show_capture "Add form (filled)" "$content" 15

    # Cursor on Done -> submit
    send_enter 1.5

    sleep 0.5
    if grep -q 'command matches sudo route\.\*' "$CLIENT_TOML"; then
        assert_pass "Step 1: local rule persisted in client.toml"
    else
        show_capture "client.toml contents" "$(cat "$CLIENT_TOML")" 20
        assert_fail "Step 1: local rule not found in client.toml"
        return 1
    fi

    # ── Step 2: Verify rule visible in Rules menu ──
    echo -e "  ${YELLOW}--- Step 2: Verify rule visible in Rules menu ---${NC}"
    sleep 0.5
    navigate_to_rules

    content=$(capture_pane -25)
    show_capture "Rules submenu (verify)" "$content" 15

    if echo "$content" | grep -q "bash command matches sudo route\\.\\* \\[local\\]"; then
        assert_pass "Step 2: local rule visible in Rules menu with [local] tag"
    else
        assert_fail "Step 2: local rule not visible in Rules menu"
        return 1
    fi

    # ── Step 3: Delete the rule via edit form ──
    echo -e "  ${YELLOW}--- Step 3: Delete local rule ---${NC}"

    local i
    for i in $(seq 1 20); do
        send_special Down 0.15
    done
    sleep 0.3

    content=$(capture_pane -25)
    show_capture "At local rule" "$content" 15
    if ! echo "$content" | grep -q "sudo route"; then
        assert_fail "Step 3: could not navigate to local rule entry"
        return 1
    fi

    send_enter 0.8

    # Edit form: Scope(L) / Plugin(L) / Param name(T) / Operator(S) / Pattern(T) / Delete(B)
    # Edit forms don't auto-edit — navigate to Delete button directly.
    send_special Down 0.3   # Param name -> Operator
    send_special Down 0.3   # Operator -> Pattern
    send_special Down 0.3   # Pattern -> Delete button
    send_enter 0.5          # press Delete button -> submits with _delete=true

    sleep 0.5

    if grep -q 'command matches sudo route\.\*' "$CLIENT_TOML"; then
        assert_fail "Step 3: local rule still present in client.toml after delete"
        return 1
    else
        assert_pass "Step 3: local rule removed from client.toml"
    fi

    return 0
}

echo -e "${YELLOW}Sandbox permit rule integration tests (#522)${NC}"
run_tests 5
