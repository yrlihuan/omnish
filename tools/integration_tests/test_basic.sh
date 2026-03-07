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

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. /debug client shows client debug info
  2. /debug session shows session debug info
  3. /context | tail -n 10 shows context output
  4. Two conversations (2 Q&A each), resume first, delete second, verify /thread list
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
    echo -e "  Waiting 15s for LLM response..."
    sleep 15

    local c1q1=$(capture_pane -30)
    if ! is_chat_prompt "$c1q1"; then
        show_capture "After Conv1 Q1" "$c1q1" 10
        assert_fail "No chat prompt after Conv1 Q1"
        return 1
    fi

    echo -e "  ${YELLOW}--- Conv 1, Q2 ---${NC}"
    send_keys "Now multiply that by 3. Reply with just the number." 0.3
    send_enter 0.3
    echo -e "  Waiting 15s for LLM response..."
    sleep 15

    local c1q2=$(capture_pane -30)
    if ! is_chat_prompt "$c1q2"; then
        show_capture "After Conv1 Q2" "$c1q2" 10
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
    echo -e "  Waiting 15s for LLM response..."
    sleep 15

    local c2q1=$(capture_pane -30)
    if ! is_chat_prompt "$c2q1"; then
        show_capture "After Conv2 Q1" "$c2q1" 10
        assert_fail "No chat prompt after Conv2 Q1"
        return 1
    fi

    echo -e "  ${YELLOW}--- Conv 2, Q2 ---${NC}"
    send_keys "Which of those has the shortest wavelength? Be brief." 0.3
    send_enter 0.3
    echo -e "  Waiting 15s for LLM response..."
    sleep 15

    local c2q2=$(capture_pane -30)
    if ! is_chat_prompt "$c2q2"; then
        show_capture "After Conv2 Q2" "$c2q2" 10
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

echo -e "${YELLOW}Basic integration test: debug, context, conversations, resume, delete${NC}"
run_tests 4
