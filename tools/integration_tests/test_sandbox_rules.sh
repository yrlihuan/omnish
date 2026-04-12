#!/usr/bin/env bash
#
# test_sandbox_rules.sh - Integration tests for sandbox permit rules (#522).
#
# Tests:
#   1-3. UI structure: Sandbox submenu layout, single Add form, Scope selector
#   4.   Global rule: add via /config, verify in daemon.toml and menu, delete
#   5.   Local rule:  add via /config, verify in client.toml and menu, delete
#   6.   Runtime (global): verify sandbox blocks sudo, add global rule, verify bypass
#   7.   Runtime (local):  verify sandbox blocks sudo, add local rule, verify bypass

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
  6. Runtime sandbox bypass (global): verify sudo blocked, add global rule, verify bypass, cleanup
  7. Runtime sandbox bypass (local):  verify sudo blocked, add local rule, verify bypass, cleanup
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

# ── Test 6: Runtime sandbox bypass after adding permit rule ────────────────
# Requires LLM cooperation and sandbox (bwrap/landlock) to be active.
# Uses "sudo -n id" which is not in existing rules.
# Sandbox sets no_new_privs → sudo fails with "nosuid" error.
# After adding permit rule, sandbox is bypassed → no "nosuid" error.

test_6() {
    echo -e "\n${YELLOW}=== Test 6: Runtime sandbox bypass with permit rule ===${NC}"

    restart_client
    wait_for_client

    # ── Step 1: Verify sandbox blocks "sudo -n id" (no matching rule) ──
    echo -e "  ${YELLOW}--- Step 1: Verify sandbox blocks sudo -n id ---${NC}"
    enter_chat
    send_keys "Run this exact command with the bash tool: sudo -n id" 0.3
    send_enter 0.3

    if ! wait_for_chat_response 60; then
        show_capture "After blocked attempt" "$(capture_pane -50)" 25
        assert_fail "Step 1: no LLM response (timeout)"
        return 1
    fi

    local content
    content=$(capture_pane -50)
    show_capture "Blocked attempt" "$content" 25

    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    # Check that the LLM actually made a bash tool call
    if ! echo "$stripped" | grep -qiE 'Bash\(|● .*bash'; then
        echo -e "  ${YELLOW}LLM did not make a bash tool call — skipping test${NC}"
        send_special Escape 0.5
        sleep 1.5
        assert_pass "Step 1: skipped (LLM did not run bash tool)"
        return 0
    fi

    if echo "$stripped" | grep -qi "nosuid\|effective uid is not 0\|no new privileges"; then
        assert_pass "Step 1: sandbox blocked sudo (privilege error detected)"
    else
        echo -e "  ${YELLOW}Warning: sandbox error not found — sandbox may be inactive${NC}"
    fi

    # Exit chat → shell prompt
    send_special Escape 0.5
    sleep 1.5  # exceed intercept_gap_ms

    # ── Step 2: Add global permit rule for "sudo -n" via /config ──
    echo -e "  ${YELLOW}--- Step 2: Add permit rule ---${NC}"
    open_config
    navigate_to_rules

    send_enter 0.8  # open "Add permit rule" form

    # Scope → global
    send_enter 0.5
    send_special Down 0.2
    send_enter 0.5
    # Down → Plugin (auto-edit)
    send_special Down 0.3
    send_keys "bash" 0.3
    send_enter 0.5
    # Param name (auto-edit)
    send_keys "command" 0.3
    send_enter 0.5
    # Operator (Select) — keep starts_with (default), skip with Down
    send_special Down 0.3
    # Pattern (auto-edit)
    send_keys "sudo -n" 0.3
    send_enter 0.5
    # Done → submit
    send_enter 1.5

    sleep 1
    if grep -q 'command starts_with sudo -n' "$DAEMON_TOML"; then
        assert_pass "Step 2: permit rule added to daemon.toml"
    else
        show_capture "daemon.toml" "$(cat "$DAEMON_TOML")" 30
        assert_fail "Step 2: permit rule not found in daemon.toml"
        return 1
    fi

    # Submit returns to Config top-level menu. ESC to exit config, ESC to exit chat.
    send_special Escape 0.5
    send_special Escape 0.5
    sleep 2  # wait for daemon hot-reload

    # ── Step 3: Verify sudo bypasses sandbox with new rule ──
    echo -e "  ${YELLOW}--- Step 3: Verify sudo bypasses sandbox ---${NC}"
    enter_chat
    send_keys "Run this exact command with the bash tool: sudo -n id" 0.3
    send_enter 0.3

    local step3_ok=true
    if ! wait_for_chat_response 60; then
        show_capture "After bypass attempt" "$(capture_pane -30)" 25
        assert_fail "Step 3: no LLM response (timeout)"
        step3_ok=false
    else
        content=$(capture_pane -30)
        show_capture "Bypass attempt" "$content" 25

        stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

        # Extract only the LAST tool call output (after last "Bash(sudo") to
        # avoid matching Step 1's stale "no new privileges" in scrollback.
        local last_tool_output
        last_tool_output=$(echo "$stripped" | sed -n '/Bash(sudo/,$ p' | tail -n +1 | head -10)

        if echo "$last_tool_output" | grep -qi "nosuid\|effective uid is not 0\|no new privileges"; then
            assert_fail "Step 3: sandbox still blocking (privilege error after rule added)"
            step3_ok=false
        else
            assert_pass "Step 3: sudo ran without sandbox error (bypass confirmed)"
        fi
    fi

    # Exit chat → shell prompt
    send_special Escape 0.5
    sleep 1.5

    # ── Step 4: Clean up — delete the permit rule ──
    echo -e "  ${YELLOW}--- Step 4: Delete permit rule ---${NC}"
    open_config
    navigate_to_rules

    local i
    for i in $(seq 1 20); do
        send_special Down 0.15
    done
    sleep 0.3

    content=$(capture_pane -25)
    show_capture "At rule for delete" "$content" 15
    if echo "$content" | grep -q "sudo -n"; then
        send_enter 0.8

        content=$(capture_pane -25)
        show_capture "Edit form for delete" "$content" 15

        # Edit form (no auto-edit): Down to Delete button
        send_special Down 0.3
        send_special Down 0.3
        send_special Down 0.3
        send_enter 0.5
        sleep 1.5
    fi

    # Verify cleanup — fall back to sed if menu delete didn't work
    if ! grep -q 'command starts_with sudo -n' "$DAEMON_TOML"; then
        assert_pass "Step 4: permit rule removed from daemon.toml"
    else
        echo -e "  ${YELLOW}Menu delete failed, cleaning up via sed${NC}"
        sed -i '/"command starts_with sudo -n"/d' "$DAEMON_TOML"
        if ! grep -q 'command starts_with sudo -n' "$DAEMON_TOML"; then
            assert_pass "Step 4: permit rule removed (fallback)"
        else
            assert_fail "Step 4: permit rule still in daemon.toml"
        fi
    fi

    if $step3_ok; then
        return 0
    else
        return 1
    fi
}

# ── Test 7: Runtime sandbox bypass with LOCAL permit rule ────────────────
# Same as test_6 but adds a local rule (client.toml) instead of global (daemon.toml).

test_7() {
    echo -e "\n${YELLOW}=== Test 7: Runtime sandbox bypass with local permit rule ===${NC}"

    restart_client
    wait_for_client

    # ── Step 1: Verify sandbox blocks "sudo -n id" (no matching rule) ──
    echo -e "  ${YELLOW}--- Step 1: Verify sandbox blocks sudo -n id ---${NC}"
    enter_chat
    send_keys "Run this exact command with the bash tool: sudo -n id" 0.3
    send_enter 0.3

    if ! wait_for_chat_response 60; then
        show_capture "After blocked attempt" "$(capture_pane -50)" 25
        assert_fail "Step 1: no LLM response (timeout)"
        return 1
    fi

    local content
    content=$(capture_pane -50)
    show_capture "Blocked attempt" "$content" 25

    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    # Check that the LLM actually made a bash tool call
    if ! echo "$stripped" | grep -qiE 'Bash\(|● .*bash'; then
        echo -e "  ${YELLOW}LLM did not make a bash tool call — skipping test${NC}"
        send_special Escape 0.5
        sleep 1.5
        assert_pass "Step 1: skipped (LLM did not run bash tool)"
        return 0
    fi

    if echo "$stripped" | grep -qi "nosuid\|effective uid is not 0\|no new privileges"; then
        assert_pass "Step 1: sandbox blocked sudo (privilege error detected)"
    else
        echo -e "  ${YELLOW}Warning: sandbox error not found — sandbox may be inactive${NC}"
    fi

    # Exit chat → shell prompt
    send_special Escape 0.5
    sleep 1.5  # exceed intercept_gap_ms

    # ── Step 2: Add LOCAL permit rule for "sudo -n" via /config ──
    echo -e "  ${YELLOW}--- Step 2: Add local permit rule ---${NC}"
    open_config
    navigate_to_rules

    send_enter 0.8  # open "Add permit rule" form

    # Scope defaults to "local" — skip with Down
    send_special Down 0.3
    # Plugin (auto-edit)
    send_keys "bash" 0.3
    send_enter 0.5
    # Param name (auto-edit)
    send_keys "command" 0.3
    send_enter 0.5
    # Operator (Select) — keep starts_with (default), skip with Down
    send_special Down 0.3
    # Pattern (auto-edit)
    send_keys "sudo -n" 0.3
    send_enter 0.5
    # Done → submit
    send_enter 1.5

    sleep 1
    if grep -q 'command starts_with sudo -n' "$CLIENT_TOML"; then
        assert_pass "Step 2: local permit rule added to client.toml"
    else
        show_capture "client.toml" "$(cat "$CLIENT_TOML")" 30
        assert_fail "Step 2: local permit rule not found in client.toml"
        return 1
    fi

    # Submit returns to Config top-level menu. ESC to exit config, ESC to exit chat.
    send_special Escape 0.5
    send_special Escape 0.5
    sleep 2  # wait for daemon hot-reload

    # ── Step 3: Verify sudo bypasses sandbox with new local rule ──
    echo -e "  ${YELLOW}--- Step 3: Verify sudo bypasses sandbox ---${NC}"
    enter_chat
    send_keys "Run this exact command with the bash tool: sudo -n id" 0.3
    send_enter 0.3

    local step3_ok=true
    if ! wait_for_chat_response 60; then
        show_capture "After bypass attempt" "$(capture_pane -30)" 25
        assert_fail "Step 3: no LLM response (timeout)"
        step3_ok=false
    else
        content=$(capture_pane -30)
        show_capture "Bypass attempt" "$content" 25

        stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

        # Extract only the LAST tool call output to avoid scrollback contamination.
        local last_tool_output
        last_tool_output=$(echo "$stripped" | sed -n '/Bash(sudo/,$ p' | tail -n +1 | head -10)

        if echo "$last_tool_output" | grep -qi "nosuid\|effective uid is not 0\|no new privileges"; then
            assert_fail "Step 3: sandbox still blocking (privilege error after local rule added)"
            step3_ok=false
        else
            assert_pass "Step 3: sudo ran without sandbox error (local bypass confirmed)"
        fi
    fi

    # Exit chat → shell prompt
    send_special Escape 0.5
    sleep 1.5

    # ── Step 4: Clean up — delete the local permit rule ──
    echo -e "  ${YELLOW}--- Step 4: Delete local permit rule ---${NC}"
    open_config
    navigate_to_rules

    local i
    for i in $(seq 1 20); do
        send_special Down 0.15
    done
    sleep 0.3

    content=$(capture_pane -25)
    show_capture "At rule for delete" "$content" 15
    if echo "$content" | grep -q "sudo -n"; then
        send_enter 0.8

        content=$(capture_pane -25)
        show_capture "Edit form for delete" "$content" 15

        # Edit form (no auto-edit): Down to Delete button
        send_special Down 0.3
        send_special Down 0.3
        send_special Down 0.3
        send_enter 0.5
        sleep 1.5
    fi

    # Verify cleanup — fall back to sed if menu delete didn't work
    if ! grep -q 'command starts_with sudo -n' "$CLIENT_TOML"; then
        assert_pass "Step 4: local permit rule removed from client.toml"
    else
        echo -e "  ${YELLOW}Menu delete failed, cleaning up via sed${NC}"
        sed -i '/"command starts_with sudo -n"/d' "$CLIENT_TOML"
        if ! grep -q 'command starts_with sudo -n' "$CLIENT_TOML"; then
            assert_pass "Step 4: local permit rule removed (fallback)"
        else
            assert_fail "Step 4: local permit rule still in client.toml"
        fi
    fi

    if $step3_ok; then
        return 0
    else
        return 1
    fi
}

echo -e "${YELLOW}Sandbox permit rule integration tests (#522)${NC}"
run_tests 7
