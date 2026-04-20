#!/usr/bin/env bash
#
# verify_issue_149.sh - Test arrow key history navigation in chat mode
#
# Tests that chat history persists across chat sessions within the same
# client process, using /commands (instant, no LLM wait).

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Up arrow recalls previous commands within same chat session
  2. History persists across chat re-entries (exit + re-enter)
  3. Down arrow navigates forward and clears at end
EOF
}

test_init "chat-history" "$@"

send_up_arrow() {
    local wait="${1:-0.5}"
    echo -e "  Sending: ${YELLOW}Up arrow${NC}"
    _tmux send-keys -t "$PANE" Up
    sleep "$wait"
}

send_down_arrow() {
    local wait="${1:-0.5}"
    echo -e "  Sending: ${YELLOW}Down arrow${NC}"
    _tmux send-keys -t "$PANE" Down
    sleep "$wait"
}

get_current_input() {
    local content=$(capture_pane -5)
    last_nonempty_line "$content"
}

# ── Test 1: Up arrow recalls commands within same chat session ────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Up arrow recalls commands within same chat session ===${NC}"

    start_client
    wait_for_client

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Send /commands (instant, no LLM wait)
    send_keys "/help" 0.3
    send_enter 1

    send_keys "/debug events" 0.3
    send_enter 1

    send_keys "/sessions" 0.3
    send_enter 1

    # A /command can occasionally fall through to the LLM path (e.g. if
    # ghost-text completion interferes with input), leaving the Thinking
    # spinner on screen. If we press Up while it is still animating,
    # get_current_input captures "Thinking..." instead of the recalled line.
    if ! wait_for_thinking_cleared 30; then
        assert_fail "Thinking spinner did not clear within 30s; a /command unexpectedly reached the LLM"
        return 1
    fi

    # Press up arrow - should show /sessions
    send_up_arrow 0.5
    local input1=$(get_current_input)
    if ! echo "$input1" | grep -q "/sessions"; then
        assert_fail "First up should show '/sessions', got: '$input1'"
        return 1
    fi

    # Second up - should show /debug events
    send_up_arrow 0.5
    local input2=$(get_current_input)
    if ! echo "$input2" | grep -q "/debug events"; then
        assert_fail "Second up should show '/debug events', got: '$input2'"
        return 1
    fi

    # Third up - should show /help
    send_up_arrow 0.5
    local input3=$(get_current_input)
    if ! echo "$input3" | grep -q "/help"; then
        assert_fail "Third up should show '/help', got: '$input3'"
        return 1
    fi

    assert_pass "Up arrow correctly navigates back through command history"
    return 0
}

# ── Test 2: History persists across chat re-entries ───────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: History persists across chat re-entries ===${NC}"

    # Exit chat mode (ESC)
    echo -e "  Sending: ${YELLOW}ESC${NC}"
    _tmux send-keys -t "$PANE" Escape
    sleep 2  # Must exceed intercept_gap_ms (1000ms) before re-entering chat

    # Re-enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Guard against a lingering Thinking spinner from test_1's LLM call.
    if ! wait_for_thinking_cleared 30; then
        assert_fail "Thinking spinner did not clear within 30s after re-entry"
        return 1
    fi

    # Up arrow should still show /sessions (from previous chat session)
    send_up_arrow 0.5
    local input1=$(get_current_input)
    if ! echo "$input1" | grep -q "/sessions"; then
        assert_fail "After re-entry, up should show '/sessions', got: '$input1'"
        return 1
    fi

    # Second up should show /debug events
    send_up_arrow 0.5
    local input2=$(get_current_input)
    if ! echo "$input2" | grep -q "/debug events"; then
        assert_fail "After re-entry, second up should show '/debug events', got: '$input2'"
        return 1
    fi

    assert_pass "History persists across chat re-entries"
    return 0
}

# ── Test 3: Down arrow navigates forward and clears at end ────────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: Down arrow navigates forward and clears at end ===${NC}"

    # Navigate up to /help (3 times: /sessions → /debug events → /help)
    send_up_arrow 0.5
    send_up_arrow 0.5
    send_up_arrow 0.5
    local at_help=$(get_current_input)
    if ! echo "$at_help" | grep -q "/help"; then
        assert_fail "Should be at '/help', got: '$at_help'"
        return 1
    fi

    # Down arrow - should go to /debug events
    send_down_arrow 0.5
    local at_debug=$(get_current_input)
    if ! echo "$at_debug" | grep -q "/debug events"; then
        assert_fail "Down should show '/debug events', got: '$at_debug'"
        return 1
    fi

    # Down arrow - should go to /sessions
    send_down_arrow 0.5
    local at_sessions=$(get_current_input)
    if ! echo "$at_sessions" | grep -q "/sessions"; then
        assert_fail "Down should show '/sessions', got: '$at_sessions'"
        return 1
    fi

    # Down arrow past end - should show empty prompt
    send_down_arrow 0.5
    local at_end=$(get_current_input)
    if echo "$at_end" | grep -qE '(> $|> \x1b)'; then
        assert_pass "Down past end shows empty prompt"
    elif echo "$at_end" | grep -q "/sessions\|/debug\|/help"; then
        assert_fail "Down past end should clear input, got: '$at_end'"
        return 1
    else
        assert_pass "Down past end clears input: '$at_end'"
    fi

    return 0
}

echo -e "${YELLOW}Testing chat history navigation with arrow keys${NC}"
run_tests 3
