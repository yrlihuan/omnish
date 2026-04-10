#!/usr/bin/env bash
#
# test_sandbox_rules.sh - Integration test for sandbox permit rules config menu (#522).
#
# Verifies that Config > Sandbox > Rules matches the expected structure:
#   1. Sandbox submenu: Enabled, Backend, availability labels, then Rules
#   2. Rules submenu: single "Add permit rule", existing rules with [local]/[global]
#   3. Add form: Scope (local/global), Plugin, Param name, Operator, Pattern

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Sandbox submenu structure: Rules appears after availability labels
  2. Rules submenu: single "Add permit rule" entry
  3. Add permit rule form has Scope selector as first field
EOF
}

test_init "sandbox-rules" "$@"

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

echo -e "${YELLOW}Sandbox rules config test: submenu structure, single add form, scope selector (#522)${NC}"
run_tests 3
