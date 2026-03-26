#!/usr/bin/env bash
#
# verify_issue_127.sh - Test that backspace correctly exits chat mode only in phase 1
#
# Verifies fix for issue #127: "backspace退出chat模式，仅当用户没有发出首轮对话的时候有效"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Phase 1 (mode selection): backspace exits chat mode when no message sent
  2. Phase 2 (chat loop): backspace is ignored after first message sent
  3. Phase 3 (/resume): backspace is ignored after resuming a conversation
EOF
}

test_init "127" "$@"

# ── Test 1: backspace exits in phase 1 ───────────────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Phase 1 - backspace should exit chat mode ===${NC}"

    start_client
    wait_for_client

    # Clear terminal to dismiss any ghost text that could wrap and merge
    # with the chat prompt when tmux captures with -J (join wrapped lines).
    send_keys "clear" 0.3
    send_enter 1

    send_keys ":" 0.5
    wait_for_prompt

    local before=$(capture_pane -20)
    show_capture "Before backspace" "$before"

    send_keys $'\x7f' 0.5

    local after=$(capture_pane -20)
    show_capture "After backspace" "$after"

    if is_chat_prompt "$before" && is_shell_prompt "$after"; then
        assert_pass "Chat prompt disappeared after backspace (exited chat mode)"
        return 0
    elif ! is_chat_prompt "$before"; then
        assert_fail "Chat prompt not found before backspace"
        return 1
    else
        assert_fail "Chat prompt still present after backspace (did not exit)"
        return 1
    fi
}

# ── Test 2: backspace ignored in phase 2 ─────────────────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Phase 2 - backspace should be ignored after first message ===${NC}"

    restart_client
    wait_for_client

    send_keys ":" 0.5
    wait_for_prompt

    send_keys "Hello, this is a test message" 0.3
    send_enter 0.3

    if ! wait_for_chat_response; then
        show_capture "Timeout waiting for response" "$(capture_pane -30)" 10
        assert_fail "LLM response timeout"
        return 1
    fi

    local before=$(capture_pane -30)
    show_capture "Before backspace" "$before" 15

    send_keys $'\x7f' 0.5

    local after=$(capture_pane -30)
    show_capture "After backspace" "$after" 15

    if is_chat_prompt "$before" && is_chat_prompt "$after"; then
        assert_pass "Chat prompt still present after backspace (backspace ignored)"
        return 0
    elif is_shell_prompt "$after"; then
        assert_fail "Chat prompt disappeared (backspace caused exit)"
        return 1
    else
        local last_line
        last_line=$(last_nonempty_line "$after")
        assert_fail "Expected chat prompt '> ' as last line, got: '${last_line}'"
        return 1
    fi
}

# ── Test 3: backspace ignored after /resume ──────────────────────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: /resume - backspace should be ignored after resuming ===${NC}"

    restart_client
    wait_for_client

    send_keys ":" 0.5
    wait_for_prompt

    send_keys "/resume" 0.3
    send_enter 1

    local after_resume=$(capture_pane -20)
    show_capture "After /resume" "$after_resume"

    # Type 'x', then backspace twice:
    # - first backspace deletes 'x'
    # - second backspace on empty buffer should be ignored
    send_keys "x" 0.3
    send_backspace 0.3
    send_backspace 0.5

    local after=$(capture_pane)
    show_capture "After 2x backspace" "$after" 5

    if is_chat_prompt "$after"; then
        assert_pass "Chat prompt still present after backspace (backspace ignored)"
        return 0
    elif is_shell_prompt "$after"; then
        assert_fail "Chat prompt disappeared (backspace caused exit)"
        return 1
    else
        # Might show /resume output or error — as long as NOT a shell prompt
        if ! is_shell_prompt "$after"; then
            assert_pass "Still in chat mode after backspace"
            return 0
        fi
        assert_fail "Unexpected state after backspace"
        return 1
    fi
}

echo -e "${YELLOW}Testing issue #127: backspace退出chat模式${NC}"
run_tests 3
