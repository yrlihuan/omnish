#!/usr/bin/env bash
#
# verify_issue_469.sh - Verify fix for issue #469
#
# Bug: /config menu shows duplicate breadcrumb header after selecting
# a non-custom Provider preset in "Add backend" and pressing ESC.
#
# Reproduction:
#   Config > Llm > Backends > Add backend
#   → Enter Provider picker, select non-custom preset, ESC back
#   → Header shows two lines:
#       Config > Llm > Backends > Add backend
#       Config > Llm > Backends
#
# This test uses the real /config menu but does NOT complete the
# add-backend flow - ESC exits the form and the handler fails safely
# because only the Provider change is sent (Name is missing).
#
# Test cases:
#   1. Select non-custom provider with prefill, ESC exit, no duplicate breadcrumb

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Select non-custom provider with prefill, ESC exit, no duplicate breadcrumb
EOF
}

test_init "issue-469" "$@"

# Helper: enter chat and open /config
open_config() {
    enter_chat
    send_keys "/config" 0.3
    send_enter 1
}

# ── Test 1: Select preset provider, ESC, check no duplicate breadcrumb ──
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Preset provider + ESC exit no duplicate breadcrumb (#469) ===${NC}"

    restart_client
    wait_for_client
    open_config

    local content
    content=$(capture_pane -20)
    show_capture "Config root" "$content" 10

    # Verify we're in the config menu
    if ! echo "$content" | grep -q "Config"; then
        assert_fail "Config menu did not open"
        return 1
    fi

    # Navigate: Down to Llm, Enter
    send_special Down 0.3
    send_enter 0.5

    content=$(capture_pane -20)
    show_capture "Inside Llm" "$content" 10

    if ! echo "$content" | grep -q "Backends"; then
        assert_fail "'Backends' not found inside Llm submenu"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    # Navigate: Down to Backends, Enter
    send_special Down 0.3
    send_enter 0.5

    content=$(capture_pane -20)
    show_capture "Inside Backends" "$content" 10

    if ! echo "$content" | grep -q "Add backend"; then
        assert_fail "'Add backend' not found inside Backends submenu"
        send_special Escape 0.5
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    # Enter "Add backend" (should be first item)
    send_enter 0.5

    content=$(capture_pane -20)
    show_capture "Inside Add backend form" "$content" 12

    if ! echo "$content" | grep -q "Provider"; then
        assert_fail "'Provider' field not found in Add backend form"
        send_special Escape 0.5
        send_special Escape 0.5
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}Add backend form opened with Provider field${NC}"

    # Open Provider picker (Enter on first item)
    send_enter 0.5

    content=$(capture_pane -20)
    show_capture "Provider picker" "$content" 10

    # Select a non-custom provider (Down x1 from "custom" to next option)
    send_special Down 0.3
    send_enter 0.5

    content=$(capture_pane -20)
    show_capture "After provider select" "$content" 14

    # Verify prefill worked - Provider should not be "custom"
    if echo "$content" | grep "Provider" | grep -q "custom"; then
        assert_fail "Provider still shows 'custom' after selection"
        send_special Escape 0.5
        send_special Escape 0.5
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}Non-custom provider selected${NC}"

    # ESC to exit the form (handler fires but fails: no Name in changes)
    send_special Escape 1.0

    content=$(capture_pane -20)
    show_capture "After ESC from form" "$content" 15

    # Count breadcrumb lines - should be exactly 1 line containing "Config"
    # The bug shows two lines like:
    #   Config > Llm > Backends > Add backend
    #   Config > Llm > Backends
    local breadcrumb_count
    breadcrumb_count=$(echo "$content" | grep -c "Config > " || true)

    if [[ "$breadcrumb_count" -gt 1 ]]; then
        assert_fail "Duplicate breadcrumb detected ($breadcrumb_count lines with 'Config >')"
        # Clean up: ESC out of remaining levels
        send_special Escape 0.5
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}No duplicate breadcrumb after ESC ($breadcrumb_count line)${NC}"

    # Should be back in Backends level
    if ! echo "$content" | grep -q "Add backend"; then
        assert_fail "Not back in Backends level after ESC"
        send_special Escape 0.5
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}Correctly returned to Backends level${NC}"

    # Clean up: ESC back to root and exit
    send_special Escape 0.5
    send_special Escape 0.5
    send_special Escape 0.5

    assert_pass "No duplicate breadcrumb after preset provider select + ESC (#469)"
    return 0
}

echo -e "${YELLOW}Issue #469 verification: duplicate breadcrumb in /config Add backend${NC}"
run_tests 1
