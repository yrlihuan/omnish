#!/usr/bin/env bash
#
# verify_issue_669.sh - Integration test for issue #669:
#   "在ctrl+o模式下，增加一个ctrl+e的开关，开启后显示详细版工具调用历史，
#    否则显示默认compact历史。默认是compact"
#
# In the Ctrl+O history browser, Ctrl+E toggles the tool-call history between
# the default compact view and a detailed view. The hint line reflects the
# current state: it offers "ctrl+e detail" while compact (the default) and
# "ctrl+e compact" while detailed. Toggling keeps the viewport anchored on the
# same entry (covered by unit tests in scroll_view.rs).
#
# Test cases:
#   1. Browser opens in compact mode; Ctrl+E -> detailed -> Ctrl+E -> compact,
#      tracked via the hint line; q returns to the chat prompt.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Ctrl+E toggles the browser between compact and detailed tool history,
     tracked via the hint line; q returns to the chat prompt (#669)
EOF
}

test_init "issue-669" "$@"

# ── Test 1: Ctrl+E toggles compact/detailed in the browser ───────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Ctrl+E toggles compact/detailed tool history (#669) ===${NC}"

    restart_client
    wait_for_client
    enter_chat

    # A tool call with multi-line output so the compact (head) and detailed
    # (full) views actually differ.
    send_keys "使用bash工具执行: seq 1 30" 0.3
    send_enter 0.3
    if ! wait_for_chat_response; then
        show_capture "After tool request" "$(capture_pane -50)" 15
        assert_fail "No response to the tool request"
        return 1
    fi
    echo -e "  Tool-call turn completed"

    # Open the browser. Default view is compact -> hint offers "ctrl+e detail".
    send_special C-o 1.5
    local content
    content=$(capture_pane -50)
    show_capture "Browser open (default compact)" "$content" 15
    if echo "$content" | grep -qF 'ctrl+e detail'; then
        assert_pass "Browser opens in compact mode (hint: 'ctrl+e detail')"
    else
        assert_fail "Browser did not open in compact mode with 'ctrl+e detail' hint (#669)"
        return 1
    fi

    # Ctrl+E -> detailed. Hint now offers switching back: "ctrl+e compact".
    send_special C-e 1
    content=$(capture_pane -50)
    show_capture "After Ctrl+E (detailed)" "$content" 15
    if echo "$content" | grep -qF 'ctrl+e compact'; then
        assert_pass "Ctrl+E switches to detailed mode (hint: 'ctrl+e compact')"
    else
        assert_fail "Ctrl+E did not switch to detailed mode (#669)"
        return 1
    fi

    # Ctrl+E again -> back to compact.
    send_special C-e 1
    content=$(capture_pane -50)
    show_capture "After Ctrl+E again (compact)" "$content" 15
    if echo "$content" | grep -qF 'ctrl+e detail'; then
        assert_pass "Ctrl+E toggles back to compact mode (hint: 'ctrl+e detail')"
    else
        assert_fail "Ctrl+E did not toggle back to compact mode (#669)"
        return 1
    fi

    # Exit browser; verify we return to a usable chat prompt.
    send_keys "q" 1
    local after_quit
    after_quit=$(capture_pane -20)
    show_capture "After q (browser exit)" "$after_quit" 10
    if is_chat_prompt "$after_quit"; then
        assert_pass "q exits the browser back to the chat prompt"
        return 0
    else
        assert_fail "Chat prompt not restored after exiting the browser"
        return 1
    fi
}

run_tests 1
