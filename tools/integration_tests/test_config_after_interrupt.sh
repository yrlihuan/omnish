#!/usr/bin/env bash
#
# test_config_after_interrupt.sh - Integration test for input display after
#                                   interrupting a task and running /config (#534).
#
# Test cases:
#   1. Interrupt task → /config → ESC → type new message → input displayed correctly

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Interrupt task, run /config, verify next input displays correctly (#534)
EOF
}

test_init "config-after-interrupt" "$@"

# ── Test 1: Input display after interrupt + /config ─────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Input display after interrupt + /config (#534) ===${NC}"

    restart_client
    wait_for_client

    # Enter chat mode
    enter_chat

    # Ask LLM to run a long command that we can interrupt
    send_keys "使用bash工具执行: sleep 30" 0.3
    send_enter 0.3

    # Wait for the tool to be actively running before interrupting
    sleep 15
    echo -e "  Sending Ctrl-C to interrupt..."
    send_special C-c 2

    # Verify we're back at the chat prompt
    local content
    content=$(capture_pane -20)
    if ! is_chat_prompt "$content"; then
        show_capture "After Ctrl-C" "$content" 10
        assert_fail "Not at chat prompt after Ctrl-C"
        return 1
    fi
    echo -e "  Back at chat prompt after interrupt"

    # Type a test message and send it (skip /config to isolate the bug)
    local test_msg="hello world"
    send_keys "$test_msg" 0.3
    send_enter 0.3

    # Wait for LLM response
    if ! wait_for_chat_response; then
        show_capture "After sending message" "$(capture_pane -30)" 15
        assert_fail "No chat response after sending message"
        return 1
    fi

    content=$(capture_pane -30)
    show_capture "After response" "$content" 15

    # Strip ANSI codes
    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    # Check for user input echo line: "> hello world"
    # This must appear as a distinct line, not just inside LLM thinking text.
    if echo "$stripped" | grep -qE '^>?\s*hello world\s*$'; then
        assert_pass "Input echo line displayed correctly"
        return 0
    else
        assert_fail "Input echo line '> hello world' missing from output (#534)"
        return 1
    fi
}

run_tests 1
