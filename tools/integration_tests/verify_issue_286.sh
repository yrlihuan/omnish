#!/usr/bin/env bash
#
# verify_issue_286.sh - Chat output from previous queries should not disappear
#
# Reproduces: enter chat, send two queries, verify both outputs remain visible.
# Bug: the second response replaces the scroll_view entirely, losing the first.
#
# Test cases:
#   1. Two Q&A rounds in one chat session — both user inputs remain visible

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Two Q&A in one session — both user inputs visible after second response
EOF
}

test_init "issue286" "$@"

test_1() {
    echo -e "\n${YELLOW}=== Test 1: Two Q&A — both outputs preserved ===${NC}"

    restart_client
    wait_for_client

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # First question
    echo -e "  ${YELLOW}--- Q1 ---${NC}"
    send_keys "What is 2+2? Reply with just the number." 0.3
    send_enter 0.3
    if ! wait_for_chat_response 30; then
        show_capture "After Q1" "$(capture_pane -50)" 10
        assert_fail "No chat prompt after Q1"
        return 1
    fi

    local after_q1=$(capture_pane -50)
    show_capture "After Q1 response" "$after_q1" 10

    # Verify Q1 user input is shown
    if ! echo "$after_q1" | grep -q "2+2"; then
        assert_fail "Q1 user input not visible after Q1"
        return 1
    fi

    # Second question
    echo -e "  ${YELLOW}--- Q2 ---${NC}"
    send_keys "What is 3+3? Reply with just the number." 0.3
    send_enter 0.3
    if ! wait_for_chat_response 30; then
        show_capture "After Q2" "$(capture_pane -50)" 10
        assert_fail "No chat prompt after Q2"
        return 1
    fi

    local after_q2=$(capture_pane -100)
    show_capture "After Q2 response" "$after_q2" 20

    # Both user inputs should still be visible in the pane history
    local has_q1 has_q2
    has_q1=$(echo "$after_q2" | grep -c "2+2") || true
    has_q2=$(echo "$after_q2" | grep -c "3+3") || true

    if [[ $has_q1 -gt 0 && $has_q2 -gt 0 ]]; then
        assert_pass "Both Q1 and Q2 content visible after second response"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "Q1 visible=$has_q1, Q2 visible=$has_q2 — first output lost"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

echo -e "${YELLOW}Issue #286: chat output from previous queries should not disappear${NC}"
run_tests 1
