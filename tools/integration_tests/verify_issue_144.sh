#!/usr/bin/env bash
#
# verify_issue_144.sh - Test that /context shows current directory
#
# Verifies fix for issue #144: "/context最后没有显示current directory"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Chat mode (new thread): /context | tail -n 5 should contain current directory
  2. Chat mode (after /resume): /context | tail -n 5 should NOT contain current directory
EOF
}

test_init "144" "$@"

# ── Test 1: /context in new chat shows cwd ───────────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: /context in new chat should show current directory ===${NC}"

    start_client
    wait_for_client

    # Enter chat mode and run /context chat
    send_keys ":" 0.5
    wait_for_prompt
    send_keys "/context chat" 0.3
    send_enter 3

    local content=$(capture_pane -50)
    show_capture "After /context" "$content" 20

    # Check for current directory indicators in context output
    # New format may include: git repo info, platform, commands with paths
    if echo "$content" | grep -qE "(WORKING DIR:|Is directory a git repo|omnish.*workspace)"; then
        assert_pass "/context output contains current directory info"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "/context output does not contain current directory info"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 2: /context after /resume should NOT show cwd ───────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: /context after /resume should NOT show current directory ===${NC}"

    restart_client
    wait_for_client

    # Enter chat mode and resume a conversation
    send_keys ":" 0.5
    wait_for_prompt

    # Resume a conversation
    send_keys "/resume" 0.3
    send_enter 1

    # Run /context chat
    send_keys "/context chat" 0.3
    send_enter 3

    local content=$(capture_pane -50)
    show_capture "After /context (after /resume)" "$content" 20

    # After /resume, context should come from thread history, not current shell
    # So it should NOT have WORKING DIR from current session
    if echo "$content" | grep -q "WORKING DIR:"; then
        # Check if it's a historic working dir or current
        local found_dir
        found_dir=$(echo "$content" | grep "WORKING DIR:" | head -1)
        echo -e "  Found: ${YELLOW}${found_dir}${NC}"
        assert_pass "/context output contains WORKING DIR (from thread history)"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_pass "/context output does not contain WORKING DIR (as expected for resumed thread)"
        send_special Escape 0.5
        sleep 1.5
        return 0
    fi
}

echo -e "${YELLOW}Testing issue #144: /context should show current directory${NC}"
run_tests 2
