#!/usr/bin/env bash
#
# verify_issue_665.sh - Integration test for issue #665:
#   "chat模式下，支持在LLM思考和tool执行过程中按ctrl+o"
#
# Ctrl+O opens the scroll-history browser. Previously it only worked at the
# chat input prompt. This test focuses on the two ways a turn can look when
# the user exits the browser (presses q) and control returns to the chat:
#
#   1. The turn is STILL in progress (LLM thinking / tool executing). The
#      user must NOT be able to type input - the background stdin watcher owns
#      stdin and discards ordinary keys until the turn ends.
#   2. The turn has COMPLETED (its output was produced while the browser was
#      open and buffered). Exiting the browser renders the new result and the
#      user can interact normally again.
#
# Note on timing: in this environment the sandboxed bash tool returns almost
# immediately, so the reliable long "in progress" window is the LLM thinking
# phase. The input-gating path (background stdin watcher) is identical for
# thinking and tool execution, so scenario 1 is exercised during thinking.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Exit browser while turn in progress -> keyboard input is blocked (#665)
  2. Exit browser after completion -> new result shown, chat interactive (#665)
EOF
}

test_init "issue-665" "$@"

# The browser's expanded view renders a hint line ("ctrl+o/esc quit",
# "page-up/down") and a scrollbar (▐ / │); none appear on the main chat
# screen, so their presence proves the browser opened.
BROWSE_MARKERS='ctrl\+o/esc quit|page-up/down|▐|│'

# Poll up to <timeout>s for the Thinking indicator (turn is in progress).
_wait_for_thinking() {
    local timeout="${1:-15}"
    local waited=0
    while [[ $waited -lt $timeout ]]; do
        if capture_pane -10 | grep -q 'Thinking'; then
            return 0
        fi
        sleep 0.25
        waited=$((waited + 1))
    done
    return 1
}

# ── Test 1: exit browser while turn in progress -> input blocked ─────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: exit browser while in progress -> input blocked (#665) ===${NC}"

    restart_client
    wait_for_client
    enter_chat

    # A tool prompt gives a long in-progress window (LLM thinking is many
    # seconds here), enough to browse and then probe input handling.
    send_keys "使用bash工具执行: sleep 30" 0.3
    send_enter 0.2

    if ! _wait_for_thinking 20; then
        show_capture "No thinking window" "$(capture_pane -30)" 15
        assert_fail "Thinking indicator never appeared"
        return 1
    fi

    echo -e "  Thinking visible, opening browser with Ctrl+O..."
    send_special C-o 1.5
    local browse_content
    browse_content=$(capture_pane -50)
    show_capture "Browser open (mid-turn)" "$browse_content" 12
    if ! echo "$browse_content" | grep -qE "$BROWSE_MARKERS"; then
        assert_fail "Browser did not open on Ctrl+O while in progress (#665)"
        return 1
    fi
    assert_pass "Ctrl+O opened the browser during an in-progress turn"

    # Exit the browser; the turn is still running on the daemon (it was frozen
    # while the browser blocked the client loop), so we should be back to the
    # Thinking indicator, NOT an editable prompt.
    send_keys "q" 0.8

    # Try to type while the turn is in progress. These keys must be swallowed
    # by the stdin watcher and never reach the input editor.
    local blocked_token="BLOCKEDINPUT777"
    send_keys "$blocked_token" 0.6

    local content
    content=$(capture_pane -20)
    show_capture "After typing during in-progress turn" "$content" 12

    # In progress = not back at an editable "> " prompt. The indicator may be
    # the Thinking spinner or a running tool spinner (e.g. "⠏ Bash(sleep 30)").
    if is_chat_prompt "$content"; then
        assert_fail "Turn was not in progress after browser exit (cannot test input blocking)"
        send_special C-c 2
        return 1
    fi
    if echo "$content" | grep -qE 'Bash\(sleep'; then
        echo -e "  ${GREEN}Observed tool still executing (⠏ Bash(sleep ...))${NC}"
    fi
    if echo "$content" | grep -q "$blocked_token"; then
        assert_fail "Typed text reached the editor - input NOT blocked while in progress (#665)"
        send_special C-c 2
        return 1
    fi
    assert_pass "Keyboard input is blocked while the turn is in progress"

    # Cancel to clean up; verify we land back at a usable chat prompt.
    send_special C-c 2
    local after_cancel
    after_cancel=$(capture_pane -15)
    show_capture "After Ctrl-C" "$after_cancel" 8
    if is_chat_prompt "$after_cancel"; then
        assert_pass "Ctrl-C returns to the chat prompt (watcher still responsive)"
        return 0
    else
        assert_fail "Did not return to chat prompt after Ctrl-C"
        return 1
    fi
}

# ── Test 2: exit browser after completion -> result shown + interactive ──
test_2() {
    echo -e "\n${YELLOW}=== Test 2: exit browser after completion -> result shown, interactive (#665) ===${NC}"

    restart_client
    wait_for_client
    enter_chat

    # Answer (579) is NOT present in the prompt, so finding it proves the
    # response (produced while the browser was open) rendered after exit.
    send_keys "What is 123 plus 456? Reply with just the number, nothing else." 0.3
    send_enter 0.2

    if ! _wait_for_thinking 20; then
        show_capture "No thinking window" "$(capture_pane -30)" 15
        assert_fail "Thinking indicator never appeared"
        return 1
    fi

    echo -e "  Thinking visible, opening browser with Ctrl+O..."
    send_special C-o 1.5
    local browse_content
    browse_content=$(capture_pane -50)
    show_capture "Browser open (mid-turn)" "$browse_content" 12
    if ! echo "$browse_content" | grep -qE "$BROWSE_MARKERS"; then
        assert_fail "Browser did not open on Ctrl+O (#665)"
        return 1
    fi
    assert_pass "Ctrl+O opened the browser"

    # Hold the browser open long enough for the turn to complete on the
    # daemon; its output buffers while the client loop is frozen.
    echo -e "  Holding browser open while the turn completes in the background..."
    sleep 6

    # Exit the browser. The buffered result must now render.
    send_keys "q" 1

    local settled=false
    local waited=0
    while [[ $waited -lt 180 ]]; do
        if capture_pane -40 | grep -q '579'; then
            settled=true
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done
    local content
    content=$(capture_pane -40)
    show_capture "After exiting browser" "$content" 15
    if ! $settled; then
        assert_fail "New result (579) not shown after exiting the browser (#665)"
        return 1
    fi
    assert_pass "New result rendered after exiting the browser"

    # Verify normal interaction resumes: a follow-up gets its own answer.
    send_keys "What is 111 plus 222? Reply with just the number, nothing else." 0.3
    send_enter 0.3
    if ! wait_for_chat_response; then
        show_capture "After follow-up" "$(capture_pane -30)" 15
        assert_fail "Chat not interactive after browsing - follow-up got no response"
        return 1
    fi
    if capture_pane -40 | grep -q '333'; then
        assert_pass "Chat is interactive after browsing (follow-up answered: 333)"
        return 0
    else
        assert_pass "Chat is interactive after browsing (follow-up got a response)"
        return 0
    fi
}

run_tests 2
