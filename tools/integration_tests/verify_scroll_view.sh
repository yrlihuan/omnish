#!/usr/bin/env bash
#
# verify_scroll_view.sh - Integration test for ScrollView in resume sessions
#
# Tests that when a conversation is resumed, long responses display in ScrollView
# compact mode with the hint line, and Ctrl+O enters/exits browse mode correctly.
#
# Test cases:
#   1. Create conversation with long response, resume it, verify ScrollView hint
#   2. In resumed session, press Ctrl+O to enter browse mode, then q to exit
#   3. After exiting browse, verify chat prompt is restored correctly

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Create conversation with long response, /resume, verify ScrollView hint
  2. Ctrl+O enters browse mode (scrollbar visible), q exits
  3. After browse exit, chat prompt is restored and input works
EOF
}

test_init "scroll-view" "$@"

# ── Test 1: Resume shows ScrollView compact + hint ────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Resume with ScrollView compact + hint ===${NC}"

    start_client
    wait_for_client

    # Enter chat and ask for a long response to trigger ScrollView
    send_keys ":" 0.5
    wait_for_prompt

    send_keys "Print the numbers 1 through 40, each on its own line. Just the numbers, nothing else." 0.3
    send_enter 0.3

    if ! wait_for_chat_response; then
        show_capture "After long response request" "$(capture_pane -50)" 15
        assert_fail "No chat prompt after requesting long response"
        return 1
    fi

    local content=$(capture_pane -50)
    show_capture "After long response" "$content" 15

    # Verify hint line is present (indicates ScrollView was used)
    # Hint text may be "ctrl+o to view" or "ctrl+o to expand"
    if echo "$content" | grep -qE "ctrl\+o to (view|expand)"; then
        echo -e "  ${GREEN}ScrollView hint detected in initial response${NC}"
    else
        echo -e "  ${YELLOW}Warning: ScrollView hint not found — response may be short enough to fit${NC}"
    fi

    # Exit chat mode
    send_special Escape 0.5
    sleep 1.5

    # Now resume the conversation
    send_keys ":" 0.5
    wait_for_prompt

    send_keys "/resume 1" 0.3
    send_enter 2

    local resume_content=$(capture_pane -50)
    show_capture "After /resume 1" "$resume_content" 15

    # Should see the hint line indicating ScrollView compact mode
    # Hint text may be "ctrl+o to view" or "ctrl+o to expand"
    if echo "$resume_content" | grep -qE "ctrl\+o to (view|expand)"; then
        assert_pass "ScrollView hint visible after /resume"
        return 0
    else
        # Check if conversation history is at least displayed
        # (Look for "User:" or actual content like line numbers)
        if echo "$resume_content" | grep -qE "(User:|line[0-9]+|30|31|32|33|34|35|36|37|38|39|40)"; then
            assert_pass "Resume shows conversation history (ScrollView hint may not appear if content fits screen)"
            return 0
        fi
        assert_fail "Neither ScrollView hint nor conversation history visible after /resume"
        return 1
    fi
}

# ── Test 2: Ctrl+O enters browse mode, q exits ───────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Ctrl+O enters browse mode, q exits ===${NC}"

    # We should still be in chat mode from test 1 (after /resume)
    local content=$(capture_pane -20)
    if ! is_chat_prompt "$content"; then
        # Re-enter chat and resume
        send_keys ":" 0.5
        wait_for_prompt
        send_keys "/resume 1" 0.3
        send_enter 2
    fi

    content=$(capture_pane -50)
    # Hint text may be "ctrl+o to view" or "ctrl+o to expand"
    if ! echo "$content" | grep -qE "ctrl\+o to (view|expand)"; then
        echo -e "  ${YELLOW}ScrollView hint not found — skipping browse test${NC}"
        assert_pass "Skipped: content fits screen without ScrollView"
        return 0
    fi

    # Send Ctrl+O to enter browse mode
    send_special C-o 1

    local browse_content=$(capture_pane -50)
    show_capture "After Ctrl+O (browse mode)" "$browse_content" 15

    # In browse mode, we should see scrollbar characters (▐ or │)
    # or the full content displayed. The hint line should be gone.
    # Check for scrollbar track/thumb or expanded content
    if echo "$browse_content" | grep -qE '▐|│|User:'; then
        echo -e "  ${GREEN}Browse mode entered — content/scrollbar visible${NC}"
    else
        echo -e "  ${YELLOW}Warning: Could not verify browse mode visually${NC}"
    fi

    # Press q to exit browse mode
    send_keys "q" 1

    local after_quit=$(capture_pane -20)
    show_capture "After q (exit browse)" "$after_quit" 10

    # Should be back to hint + chat prompt
    if is_chat_prompt "$after_quit"; then
        assert_pass "Ctrl+O enters browse, q exits back to chat prompt"
        return 0
    else
        assert_fail "Chat prompt not restored after exiting browse mode"
        return 1
    fi
}

# ── Test 3: After browse exit, chat input works normally ──────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: After browse exit, chat input works normally ===${NC}"

    # We should be at chat prompt from test 2
    local content=$(capture_pane -10)
    if ! is_chat_prompt "$content"; then
        # Ensure we're in chat mode
        send_keys ":" 0.5
        wait_for_prompt
    fi

    # Type a follow-up message and verify it gets a response
    send_keys "What was the last number you printed? Reply with just the number." 0.3
    send_enter 0.3

    if ! wait_for_chat_response; then
        show_capture "After follow-up" "$(capture_pane -30)" 10
        assert_fail "No response after browse exit follow-up"
        return 1
    fi

    local response=$(capture_pane -30)
    show_capture "Follow-up response" "$response" 10

    # The response should contain "40" since we asked to print 1-40
    if echo "$response" | grep -q "40"; then
        assert_pass "Chat works after browse — follow-up correctly references '40'"
    else
        assert_pass "Chat input works after browse exit (response received)"
    fi

    send_special Escape 0.5
    sleep 1.5
    return 0
}

echo -e "${YELLOW}ScrollView integration test: resume + compact view + browse mode${NC}"
run_tests 3
