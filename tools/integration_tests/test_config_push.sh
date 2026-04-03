#!/usr/bin/env bash
#
# test_config_push.sh - Integration test for client config push (#490).
#
# Test cases:
#   1. Change chat mode hotkey to * via /config, verify * enters chat mode
#
# Cleanup: always restores command_prefix to ":" in daemon.toml.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

DAEMON_TOML="$HOME/.omnish/daemon.toml"

show_usage() {
    cat <<EOF
Test cases:
  1. Change chat mode hotkey to * via /config, verify * enters chat mode
EOF
}

test_init "config-push" "$@"

# Restore command_prefix to ":" in daemon.toml, regardless of test outcome.
_restore_prefix() {
    echo -e "${YELLOW}Restoring command_prefix to ':' in daemon.toml...${NC}"
    if [[ -f "$DAEMON_TOML" ]]; then
        # Use sed to replace command_prefix value back to ":"
        sed -i 's/^command_prefix = ".*"/command_prefix = ":"/' "$DAEMON_TOML"
        echo -e "${GREEN}Restored.${NC}"
    else
        echo -e "${YELLOW}daemon.toml not found, nothing to restore.${NC}"
    fi
}

# Register restore as part of cleanup (runs on EXIT).
# Save the original cleanup body before redefining.
eval "_original_cleanup() { $(declare -f cleanup | tail -n +2); }"
cleanup() {
    _restore_prefix
    _original_cleanup
}

# ── Test 1: Change hotkey to * via /config, verify it works ───────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Change chat mode hotkey to * via /config ===${NC}"

    restart_client
    wait_for_client

    # Enter chat mode and open /config
    enter_chat
    send_keys "/config" 0.3
    send_enter 1

    local content
    content=$(capture_pane -20)
    show_capture "Config menu" "$content" 12

    # Verify we're in the Config menu
    if ! echo "$content" | grep -q "Config"; then
        assert_fail "Config menu not displayed"
        send_special Escape 0.5
        return 1
    fi

    # Navigate: General is first item, press Enter to drill in
    send_enter 1

    content=$(capture_pane -20)
    show_capture "Inside General" "$content" 12

    if ! echo "$content" | grep -q "Hotkeys"; then
        assert_fail "'Hotkeys' submenu not found inside General"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    # Hotkeys is first item inside General, press Enter
    send_enter 1

    content=$(capture_pane -20)
    show_capture "Inside Hotkeys" "$content" 12

    if ! echo "$content" | grep -q "Enter chat mode"; then
        assert_fail "'Enter chat mode' not found inside Hotkeys"
        send_special Escape 0.5
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    # "Enter chat mode" is first item, Enter to edit
    send_enter 0.5

    content=$(capture_pane -20)
    show_capture "Editing Enter chat mode" "$content" 12

    # Verify edit mode (hint should show "confirm")
    if ! echo "$content" | grep -q "confirm"; then
        assert_fail "Not in edit mode for 'Enter chat mode'"
        send_special Escape 0.5
        send_special Escape 0.5
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    # Clear current value ":" (1 char) and type "*"
    send_special BSpace 0.1
    send_keys "*" 0.3

    # Confirm with Enter
    send_enter 0.5

    content=$(capture_pane -20)
    show_capture "After setting *" "$content" 12

    # Verify the value shows "*"
    if ! echo "$content" | grep "Enter chat mode" | grep -q '\*'; then
        assert_fail "'Enter chat mode' value not updated to *"
        send_special Escape 0.5
        send_special Escape 0.5
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}Enter chat mode value set to *${NC}"

    # ESC x3 to exit menu (Hotkeys -> General -> Config -> exit)
    send_special Escape 0.5
    send_special Escape 0.5
    send_special Escape 0.5
    sleep 0.5

    # Exit chat mode
    send_special Escape 0.5

    # Wait for config push to propagate to client
    sleep 2

    # Now verify: ":" should NOT enter chat mode
    echo -e "  ${YELLOW}Verifying ':' no longer enters chat mode...${NC}"
    sleep 1.5  # intercept gap
    send_keys ":" 0.5
    sleep 1

    content=$(capture_pane -10)
    show_capture "After pressing ':'" "$content" 5

    if is_chat_prompt "$content"; then
        assert_fail "':' still enters chat mode after changing hotkey to *"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
    echo -e "  ${GREEN}':' no longer triggers chat mode${NC}"

    # Clear the typed ":"
    send_special C-c 0.5

    # Now verify: "*" SHOULD enter chat mode
    echo -e "  ${YELLOW}Verifying '*' enters chat mode...${NC}"
    sleep 1.5  # intercept gap
    send_keys "*" 0.5
    wait_for_prompt

    content=$(capture_pane -10)
    show_capture "After pressing '*'" "$content" 5

    if is_chat_prompt "$content"; then
        echo -e "  ${GREEN}'*' correctly enters chat mode${NC}"
        send_special Escape 0.5
        assert_pass "Chat mode hotkey changed to * and works correctly"
        return 0
    else
        assert_fail "'*' did not enter chat mode"
        return 1
    fi
}

echo -e "${YELLOW}Config push integration test: change hotkey via /config, verify push to client${NC}"
run_tests 1
