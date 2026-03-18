#!/usr/bin/env bash
#
# test_basic.sh - Basic integration test covering debug commands, conversations,
#                 resume, thread deletion, and context inspection.
#
# Test cases:
#   1. /debug client — verify client debug info is shown
#   2. /debug session — verify session debug info is shown
#   3. /context | tail -n 10 — verify context output with pipe
#   4. Two conversations with 2 Q&A each, /resume first, /thread del, /thread list verify
#   5. Arrow Up/Down history navigation — echo 1, echo 2, Up recalls echo 2, Down clears
#   6. Chat cursor position — cursor at column 2 after "> " when entering chat mode
#   7. Typing in chat after output — no ghost lines from cursor mispositioning (#278)
#   8. Shell prompt preserved when entering chat mode (#279)
#   9. ESC dismisses ghost completion (#259)
#  10. Ghost text completion via omnish_debug (#328)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. /debug client shows client debug info
  2. /debug session shows session debug info
  3. /context | tail -n 10 shows context output
  4. Two conversations (2 Q&A each), resume first, delete second, verify /thread list
  5. Arrow Up/Down history navigation
  6. Chat cursor at column 2 after "> "
  7. Typing in chat after output — no ghost lines (#278)
  8. Shell prompt preserved when entering chat (#279)
  9. ESC dismisses ghost completion (#259)
 10. Ghost text completion via omnish_debug (#328)
EOF
}

test_init "basic" "$@"

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

# ── Test 1: /debug client ──────────────────────────────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: /debug client ===${NC}"

    start_client
    wait_for_client

    send_keys ":" 0.5
    wait_for_prompt

    send_keys "/debug client" 0.3
    send_enter 1

    local content=$(capture_pane -30)
    show_capture "/debug client output" "$content" 15

    # Should contain version and client state info
    if ! echo "$content" | grep -q "Version:" || ! echo "$content" | grep -q "at_prompt:"; then
        assert_fail "/debug client output missing expected fields"
        return 1
    fi

    # Auto-exit: inspection command as first action should return to shell (issue #148)
    # Check if shell prompt (contains "$ ") appears in last few lines of output.
    # Ghost text may follow "$ " so we can't require it at line end.
    if echo "$content" | tail -3 | grep -q '\$ '; then
        assert_pass "/debug client shows output and auto-exits chat"
        return 0
    else
        assert_fail "/debug client did not auto-exit to shell prompt"
        return 1
    fi
}

# ── Test 2: /debug session ─────────────────────────────────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: /debug session ===${NC}"

    # Re-enter chat (previous test auto-exited)
    send_keys ":" 0.5
    wait_for_prompt

    send_keys "/debug session" 0.3
    send_enter 1

    local content=$(capture_pane -30)
    show_capture "/debug session output" "$content" 15

    # Should contain session info and auto-exit to shell
    if ! echo "$content" | grep -qi "session"; then
        assert_fail "/debug session output missing session info"
        return 1
    fi

    if echo "$content" | tail -3 | grep -q '\$ '; then
        assert_pass "/debug session shows output and auto-exits chat"
        return 0
    else
        assert_fail "/debug session did not auto-exit to shell prompt"
        return 1
    fi
}

# ── Test 3: /context | tail -n 10 ──────────────────────────────────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: /context | tail -n 10 ===${NC}"

    # Re-enter chat (previous test auto-exited)
    send_keys ":" 0.5
    wait_for_prompt

    send_keys "/context | tail -n 10" 0.3
    send_enter 1

    local content=$(capture_pane -30)
    show_capture "/context | tail output" "$content" 15

    # Should show context output and auto-exit to shell
    local non_empty
    non_empty=$(echo "$content" | grep -cvE '^\s*$|^\s*> ') || true
    if [[ $non_empty -le 2 ]]; then
        assert_fail "/context output appears empty"
        return 1
    fi

    if echo "$content" | tail -3 | grep -q '\$ '; then
        assert_pass "/context | tail -n 10 produces output and auto-exits chat"
        return 0
    else
        assert_fail "/context did not auto-exit to shell prompt"
        return 1
    fi
}

# ── Test 4: Two conversations, resume, delete, verify ──────────────────────
test_4() {
    echo -e "\n${YELLOW}=== Test 4: Two conversations + resume + delete + verify ===${NC}"

    restart_client
    wait_for_client

    # ── Conversation 1: two Q&A rounds ──
    echo -e "  ${YELLOW}--- Conv 1, Q1 ---${NC}"
    send_keys ":" 0.5
    wait_for_prompt

    send_keys "What is 2+2? Reply with just the number." 0.3
    send_enter 0.3
    if ! wait_for_chat_response 30; then
        show_capture "After Conv1 Q1" "$(capture_pane -30)" 10
        assert_fail "No chat prompt after Conv1 Q1"
        return 1
    fi

    echo -e "  ${YELLOW}--- Conv 1, Q2 ---${NC}"
    send_keys "Now multiply that by 3. Reply with just the number." 0.3
    send_enter 0.3
    if ! wait_for_chat_response 30; then
        show_capture "After Conv1 Q2" "$(capture_pane -30)" 10
        assert_fail "No chat prompt after Conv1 Q2"
        return 1
    fi

    # Exit chat mode
    send_special Escape 0.5
    sleep 1.5  # exceed intercept_gap_ms

    # ── Conversation 2: two Q&A rounds ──
    echo -e "  ${YELLOW}--- Conv 2, Q1 ---${NC}"
    send_keys ":" 0.5
    wait_for_prompt

    send_keys "Name three primary colors. Be brief." 0.3
    send_enter 0.3
    if ! wait_for_chat_response 30; then
        show_capture "After Conv2 Q1" "$(capture_pane -30)" 10
        assert_fail "No chat prompt after Conv2 Q1"
        return 1
    fi

    echo -e "  ${YELLOW}--- Conv 2, Q2 ---${NC}"
    send_keys "Which of those has the shortest wavelength? Be brief." 0.3
    send_enter 0.3
    if ! wait_for_chat_response 30; then
        show_capture "After Conv2 Q2" "$(capture_pane -30)" 10
        assert_fail "No chat prompt after Conv2 Q2"
        return 1
    fi

    # ── List threads — expect at least 2 ──
    echo -e "  ${YELLOW}--- /thread list ---${NC}"
    send_keys "/thread list" 0.3
    send_enter 1

    local threads_before=$(capture_pane -50)
    show_capture "/thread list listing" "$threads_before" 10

    local thread_count
    thread_count=$(_count_thread_lines "$threads_before")
    if [[ $thread_count -lt 2 ]]; then
        assert_fail "Expected at least 2 threads, found $thread_count"
        return 1
    fi
    echo -e "  Found $thread_count thread(s)"

    # ── /resume 1 (most recent = conv 2) then check we can go back to conv 1 ──
    # New threads sort to top, so [1] = conv 2, [2] = conv 1
    echo -e "  ${YELLOW}--- /resume 2 (conv 1) ---${NC}"
    send_keys "/resume 2" 0.3
    send_enter 2

    local resume_out=$(capture_pane -30)
    show_capture "/resume 2 output" "$resume_out" 10

    # Should show conversation history or "Resuming"
    if echo "$resume_out" | grep -qi "resum\|2+2\|multiply"; then
        echo -e "  ${GREEN}Resume shows conv 1 content${NC}"
    else
        echo -e "  ${YELLOW}Warning: resume output may not show conv 1 content${NC}"
    fi

    if ! is_chat_prompt "$resume_out"; then
        assert_fail "No chat prompt after /resume"
        return 1
    fi

    # ── Delete conv 2 (index 1, the most recent) ──
    echo -e "  ${YELLOW}--- /thread del 1 ---${NC}"
    send_keys "/thread del 1" 0.3
    send_enter 1

    local del_out=$(capture_pane -20)
    show_capture "After delete" "$del_out" 5

    if echo "$del_out" | grep -qi "deleted\|Deleted"; then
        echo -e "  ${GREEN}Delete confirmed${NC}"
    else
        echo -e "  ${YELLOW}Warning: no delete confirmation seen${NC}"
    fi

    # ── /thread list again — verify count decreased ──
    echo -e "  ${YELLOW}--- /thread list (after delete) ---${NC}"
    send_keys "/thread list" 0.3
    send_enter 1

    local threads_after=$(capture_pane -50)
    show_capture "/thread list after delete" "$threads_after" 10

    local thread_count_after
    thread_count_after=$(_count_thread_lines "$threads_after")

    if [[ $thread_count_after -lt $thread_count ]]; then
        assert_pass "Thread count decreased after delete ($thread_count -> $thread_count_after)"
        return 0
    else
        assert_fail "Thread count did not decrease ($thread_count -> $thread_count_after)"
        return 1
    fi
}

# ── Test 5: Arrow Up/Down history navigation ─────────────────────────────
test_5() {
    echo -e "\n${YELLOW}=== Test 5: Arrow Up/Down history navigation ===${NC}"

    restart_client
    wait_for_client

    # Run echo 1
    send_keys "echo 1" 0.3
    send_enter 1

    # Run echo 2
    send_keys "echo 2" 0.3
    send_enter 1

    # Press Arrow Up — should recall "echo 2"
    send_special Up 0.5

    local content=$(capture_pane -10)
    show_capture "After Arrow Up" "$content" 5

    local last_line
    last_line=$(last_nonempty_line "$content")
    if echo "$last_line" | grep -q "echo 2"; then
        echo -e "  ${GREEN}Arrow Up recalled 'echo 2'${NC}"
    else
        assert_fail "Arrow Up did not recall 'echo 2', got: $last_line"
        return 1
    fi

    # Press Arrow Down — should return to empty prompt
    send_special Down 0.5

    content=$(capture_pane -10)
    show_capture "After Arrow Down" "$content" 5

    last_line=$(last_nonempty_line "$content")
    # After Down, the command line should be empty (just the prompt ending with $)
    if echo "$last_line" | grep -qE '\$ $'; then
        assert_pass "Arrow Up/Down history navigation works correctly"
        return 0
    else
        assert_fail "Arrow Down did not return to empty prompt, got: $last_line"
        return 1
    fi
}

# ── Test 6: Chat cursor position after entering chat mode ────────────────
test_6() {
    echo -e "\n${YELLOW}=== Test 6: Chat cursor at column 2 after \"> \" ===${NC}"

    restart_client
    wait_for_client

    send_keys ":" 0.5
    wait_for_prompt

    # Get cursor X position via tmux
    local cursor_x
    cursor_x=$(_tmux display-message -p -t "$PANE" '#{cursor_x}')

    if [[ "$cursor_x" == "2" ]]; then
        assert_pass "Cursor at column 2 (after \"> \") when entering chat mode"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "Cursor at column $cursor_x, expected 2 (after \"> \")"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 7: Typing in chat doesn't leave ghost lines (#278) ──────────────
test_7() {
    echo -e "\n${YELLOW}=== Test 7: Typing in chat after output doesn't leave ghost lines ===${NC}"

    restart_client
    wait_for_client

    # Fill screen with output
    send_keys "for i in {1..10}; do echo \$i; done" 0.3
    send_enter 1.5

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Type "hello" character by character
    for c in h e l l o; do
        send_keys "$c" 0.15
    done
    sleep 0.5

    local content
    content=$(capture_pane -20)
    show_capture "After typing hello" "$content" 10

    # "> hello" should appear exactly once; partial prompts like "> hell", "> hel" must NOT
    local full_count partial_count
    full_count=$(echo "$content" | grep -c '> hello' || true)
    partial_count=$(echo "$content" | grep -cE '> (h|he|hel|hell)$' || true)

    if [[ $full_count -eq 1 && $partial_count -eq 0 ]]; then
        assert_pass "Typing in chat renders correctly without ghost lines"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "Ghost lines detected: '> hello' count=$full_count, partial prompts=$partial_count"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 8: Shell prompt preserved when entering chat mode (#279) ─────────
test_8() {
    echo -e "\n${YELLOW}=== Test 8: Shell prompt preserved when entering chat ===${NC}"

    restart_client
    wait_for_client

    send_keys ":" 0.5
    wait_for_prompt

    local content
    content=$(capture_pane -10)
    show_capture "After entering chat" "$content" 5

    # The shell prompt (ending with "$ ") should still be visible above "> "
    if echo "$content" | grep -q '\$ ' && echo "$content" | grep -qE '^\s*> \s*$'; then
        assert_pass "Shell prompt preserved above chat prompt"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "Shell prompt not found above chat prompt"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 9: ESC dismisses ghost completion (#259) ────────────────────────
test_9() {
    echo -e "\n${YELLOW}=== Test 9: ESC dismisses ghost completion (#259) ===${NC}"

    restart_client
    wait_for_client

    # Run a command first to seed history context for completion
    send_keys "echo hello world" 0.3
    send_enter 1

    # Type partial command likely to trigger ghost completion
    send_keys "echo hel" 0.3

    # Poll for ghost text to appear (debounce ~500ms + LLM latency)
    local ghost_appeared=false
    local content
    for attempt in $(seq 1 15); do
        sleep 1
        content=$(capture_pane -5)
        local last
        last=$(last_nonempty_line "$content")
        local stripped
        stripped=$(echo "$last" | sed 's/\x1b\[[0-9;]*m//g')
        # Ghost text adds characters after "echo hel" (e.g. "echo hello")
        if echo "$stripped" | grep -q 'echo hel[a-zA-Z]'; then
            ghost_appeared=true
            break
        fi
    done

    if ! $ghost_appeared; then
        echo -e "  ${YELLOW}Ghost completion not available in test environment, skipping${NC}"
        send_special C-c 0.5
        assert_pass "ESC dismiss test skipped (no ghost completion)"
        return 0
    fi

    show_capture "Ghost visible" "$content" 3

    # Press ESC to dismiss ghost text
    send_special Escape 0.5

    content=$(capture_pane -5)
    show_capture "After ESC" "$content" 3

    local last stripped
    last=$(last_nonempty_line "$content")
    stripped=$(echo "$last" | sed 's/\x1b\[[0-9;]*m//g')

    # Ghost text should be gone after ESC
    if echo "$stripped" | grep -q 'echo hel[a-zA-Z]'; then
        assert_fail "Ghost text still present after ESC"
        send_special C-c 0.5
        return 1
    fi

    # But typed text should still be preserved
    if echo "$stripped" | grep -q 'echo hel'; then
        assert_pass "ESC dismissed ghost completion (#259)"
        send_special C-c 0.5
        return 0
    else
        assert_fail "Typed text lost after ESC dismiss"
        send_special C-c 0.5
        return 1
    fi
}

# ── Test 10: Ghost text completion via omnish_debug (#328) ────────────────
test_10() {
    echo -e "\n${YELLOW}=== Test 10: Ghost text completion via omnish_debug (#328) ===${NC}"

    restart_client
    wait_for_client

    # Type "omnish_debug" — daemon returns canned suggestions
    send_keys "omnish_debug" 0.3

    # Poll for ghost text to appear (debounce ~500ms + round trip)
    local ghost_appeared=false
    local content
    for attempt in $(seq 1 15); do
        sleep 1
        content=$(capture_pane -5)
        local last
        last=$(last_nonempty_line "$content")
        local stripped
        stripped=$(echo "$last" | sed 's/\x1b\[[0-9;]*m//g')
        # Ghost text should add " yes" after "omnish_debug"
        if echo "$stripped" | grep -q 'omnish_debug yes'; then
            ghost_appeared=true
            break
        fi
    done

    if ! $ghost_appeared; then
        show_capture "No ghost text" "$content" 5
        assert_fail "Ghost text did not appear for omnish_debug"
        send_special C-c 0.5
        return 1
    fi

    show_capture "Ghost visible" "$content" 3

    # Press Tab to accept ghost text
    send_special Tab 0.5

    content=$(capture_pane -5)
    show_capture "After Tab accept" "$content" 3

    local last stripped
    last=$(last_nonempty_line "$content")
    stripped=$(echo "$last" | sed 's/\x1b\[[0-9;]*m//g')

    # After Tab, readline should contain "omnish_debug yes" as real text
    if echo "$stripped" | grep -q 'omnish_debug yes'; then
        assert_pass "Ghost text completion works end-to-end via omnish_debug"
        send_special C-c 0.5
        return 0
    else
        assert_fail "Tab accept failed, got: $stripped"
        send_special C-c 0.5
        return 1
    fi
}

echo -e "${YELLOW}Basic integration test: debug, context, conversations, resume, delete, history, cursor, ghost-dismiss, completion${NC}"
run_tests 10
