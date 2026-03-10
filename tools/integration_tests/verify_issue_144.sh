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

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Run /context | tail -n 5
    send_keys "/context | tail -n 5" 0.3
    send_enter 3

    local content=$(capture_pane -20)
    show_capture "After /context | tail -n 5" "$content" 10

    if echo "$content" | grep -q '# workingDirectory' && echo "$content" | grep -q '<system-reminder>'; then
        assert_pass "/context output contains <system-reminder> with # workingDirectory"
        return 0
    else
        assert_fail "/context output does not contain <system-reminder> with # workingDirectory"
        return 1
    fi
}

# ── Test 2: /context after /resume should NOT show cwd ───────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: /context after /resume should NOT show current directory ===${NC}"

    restart_client
    wait_for_client

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Resume a conversation
    send_keys "/resume" 0.3
    send_enter 1

    # Run /context | tail -n 5
    send_keys "/context | tail -n 5" 0.3
    send_enter 3

    local content=$(capture_pane -20)
    show_capture "After /context | tail -n 5" "$content" 10

    if echo "$content" | grep -q '# workingDirectory' && echo "$content" | grep -q '<system-reminder>'; then
        assert_fail "/context output contains <system-reminder> with # workingDirectory but should not"
        return 1
    else
        assert_pass "/context output does not contain <system-reminder> with # workingDirectory (as expected)"
        return 0
    fi
}

echo -e "${YELLOW}Testing issue #144: /context should show current directory${NC}"
run_tests 2
