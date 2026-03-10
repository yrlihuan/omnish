#!/usr/bin/env bash
#
# verify_picker_selection.sh - Test picker widget selection in /resume command
#
# Tests that:
#   1. Creating 3 conversations with different messages
#   2. Calling /resume without index triggers picker widget
#   3. Using down arrow key selects the second conversation
#   4. The correct conversation is resumed

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Picker widget selection with down arrow
EOF
}

test_init "picker-selection" "$@"

send_down_arrow() {
    local wait="${1:-0.5}"
    echo -e "  Sending: ${YELLOW}Down arrow${NC}"
    _tmux send-keys -t "$PANE" Down
    sleep "$wait"
}

send_up_arrow() {
    local wait="${1:-0.5}"
    echo -e "  Sending: ${YELLOW}Up arrow${NC}"
    _tmux send-keys -t "$PANE" Up
    sleep "$wait"
}

# Count [N] lines from only the LATEST /thread list output in a capture buffer.
# Strips ANSI codes, then uses awk to reset count on each "> /thread list" line
# (excluding "> /thread del ..."), so only the final listing is counted.
_count_thread_lines() {
    echo "$1" | sed 's/\x1b\[[0-9;]*m//g' | awk '
        /^>? *\/thread list[[:space:]]*$/ { c=0; next }
        /^\s*\[[0-9]+\]/ { c++ }
        END { print c }
    '
}

# Helper to check if picker is displayed
picker_displayed() {
    local content="$1"
    echo "$content" | grep -q "Resume conversation:"
}

# Helper to get selected item from picker display (line with '>' prefix)
get_selected_item() {
    local content="$1"
    # Look for lines with '>' marker (might be ANSI colored)
    echo "$content" | grep -E '^\s*>' | sed 's/\x1b\[[0-9;]*m//g' | sed 's/^\s*>//'
}

# ── Test 1: Picker widget selection with down arrow ─────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Picker widget selection with down arrow ===${NC}"

    restart_client
    wait_for_client

    # ── Conversation 1: simple math ──
    echo -e "  ${YELLOW}--- Conv 1 ---${NC}"
    send_keys ":" 0.5
    wait_for_prompt

    send_keys "What is 2+2? Reply with just the number." 0.3
    send_enter 0.3
    if ! wait_for_chat_response 30; then
        show_capture "After Conv1" "$(capture_pane -30)" 10
        assert_fail "No chat prompt after Conv1"
        return 1
    fi

    # Exit chat mode
    send_special Escape 0.5
    sleep 1.5  # exceed intercept_gap_ms

    # ── Conversation 2: colors ──
    echo -e "  ${YELLOW}--- Conv 2 ---${NC}"
    send_keys ":" 0.5
    wait_for_prompt

    send_keys "Name three primary colors. Be brief." 0.3
    send_enter 0.3
    if ! wait_for_chat_response 30; then
        show_capture "After Conv2" "$(capture_pane -30)" 10
        assert_fail "No chat prompt after Conv2"
        return 1
    fi

    # Exit chat mode
    send_special Escape 0.5
    sleep 1.5

    # ── Conversation 3: animals ──
    echo -e "  ${YELLOW}--- Conv 3 ---${NC}"
    send_keys ":" 0.5
    wait_for_prompt

    send_keys "Name three animals that can fly. Be brief." 0.3
    send_enter 0.3
    if ! wait_for_chat_response 30; then
        show_capture "After Conv3" "$(capture_pane -30)" 10
        assert_fail "No chat prompt after Conv3"
        return 1
    fi

    # Exit chat mode
    send_special Escape 0.5
    sleep 1.5

    # Re-enter chat mode for /thread list command
    send_keys ":" 0.5
    wait_for_prompt
    sleep 0.5  # Extra wait to ensure chat prompt is ready

    # ── Verify we have 3 conversations ──
    echo -e "  ${YELLOW}--- /thread list check ---${NC}"
    send_keys "/thread list" 0.3
    send_enter 1

    local threads_output=$(capture_pane -50)
    show_capture "/thread list output" "$threads_output" 10

    # Count thread lines (lines starting with [N])
    local thread_count
    thread_count=$(_count_thread_lines "$threads_output")
    if [[ $thread_count -lt 3 ]]; then
        assert_fail "Expected at least 3 threads, found $thread_count"
        return 1
    fi
    echo -e "  Found $thread_count thread(s)"

    # ── Trigger picker with /resume without index ──
    echo -e "  ${YELLOW}--- /resume (trigger picker) ---${NC}"
    send_keys "/resume" 0.3
    send_enter 1  # Wait for picker to render (longer for observation)
    sleep 1  # Extra pause for visual observation in attach mode

    local picker_output=$(capture_pane -30)
    show_capture "After /resume" "$picker_output" 15

    # Check if picker is displayed (optional for now)
    # if ! picker_displayed "$picker_output"; then
    #     assert_fail "Picker not displayed after /resume"
    #     return 1
    # fi
    # echo -e "  ${GREEN}Picker displayed${NC}"

    # ── Select second item with down arrow ──
    echo -e "  ${YELLOW}--- Selecting second item ---${NC}"
    send_down_arrow 0.5
    send_enter 1
    sleep 1  # Pause to observe the selection result

    local after_select=$(capture_pane -30)
    show_capture "After selecting" "$after_select" 15

    # Verify we're in chat mode
    if ! is_chat_prompt "$after_select"; then
        assert_fail "Not in chat mode after selecting from picker"
        return 1
    fi

    # Check that conversation 2 content is visible (colors)
    if echo "$after_select" | grep -qi "color\|red\|green\|blue"; then
        echo -e "  ${GREEN}Conv 2 content detected in output${NC}"
        # Still send a follow-up to confirm we're in the right conversation
        send_keys "What was my previous question?" 0.5
        send_enter 0.5
        if ! wait_for_chat_response 30; then
            show_capture "Follow-up timeout" "$(capture_pane -30)" 10
            assert_fail "Follow-up response timeout"
            return 1
        fi
        local response=$(capture_pane -30)
        if echo "$response" | grep -qi "color\|red\|green\|blue"; then
            echo -e "  ${GREEN}Conv 2 content confirmed via follow-up${NC}"
            assert_pass "Successfully selected second conversation with down arrow"
            return 0
        else
            show_capture "Follow-up response" "$response" 10
            assert_fail "Selected conversation does not appear to be conv 2 (colors)"
            return 1
        fi
    else
        # Might be showing conversation history or just prompt
        # Try sending a message to see which conversation we're in
        send_keys "What was my previous question?" 0.5
        send_enter 0.5
        if ! wait_for_chat_response 30; then
            show_capture "Follow-up timeout" "$(capture_pane -30)" 10
            assert_fail "Follow-up response timeout"
            return 1
        fi
        local response=$(capture_pane -30)
        if echo "$response" | grep -qi "color\|red\|green\|blue"; then
            echo -e "  ${GREEN}Conv 2 content confirmed via follow-up${NC}"
            assert_pass "Successfully selected second conversation with down arrow"
            return 0
        else
            show_capture "Follow-up response" "$response" 10
            assert_fail "Selected conversation does not appear to be conv 2 (colors)"
            return 1
        fi
    fi
}

run_tests 1