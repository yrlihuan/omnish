#!/usr/bin/env bash
#
# verify_issue_184.sh - Integration test for multi-line redraw bug
#
# Bug: after newline + backspace, the display shows duplicated lines
# instead of merging back to a single line.
#
# Test cases:
#   1. Newline then backspace: should show single line, not duplicated
#   2. Two newlines then two backspaces: should collapse back to one line

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Type "hello", Ctrl-J (newline), Backspace → single "> hello", no duplicate
  2. Type "abc", Ctrl-J, "def", Ctrl-J, "ghi", Home, BSpace×2 → "> abcdefghi"
EOF
}

test_init "issue-184" "$@"

# ── Test 1: Newline then backspace should not duplicate ──────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Newline + backspace = no duplicate lines ===${NC}"

    start_client
    wait_for_client

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Type "hello", then Ctrl-J for newline
    send_keys "hello" 0.3
    send_special C-j 0.3  # Ctrl-J = LF = newline

    local content=$(capture_pane -10)
    show_capture "After newline" "$content" 5

    # Now backspace to merge the empty line back
    send_backspace 0.5

    content=$(capture_pane -10)
    show_capture "After backspace merge" "$content" 8

    # Count how many times "> hello" appears — should be exactly 1
    local count
    count=$(echo "$content" | grep -c '> hello' || true)

    if [[ "$count" -eq 1 ]]; then
        assert_pass "Single '> hello' after newline+backspace (no duplicate)"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "Expected exactly 1 '> hello' but found $count (duplicate line bug)"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 2: Multiple newlines then backspaces collapse correctly ─────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Multiple newlines + backspaces collapse ===${NC}"

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Type "abc", Ctrl-J, "def", Ctrl-J, "ghi"
    send_keys "abc" 0.3
    send_special C-j 0.3
    send_keys "def" 0.3
    send_special C-j 0.3
    send_keys "ghi" 0.3

    local content=$(capture_pane -10)
    show_capture "After 3 lines" "$content" 5

    # Move to start of line 3, backspace to merge with line 2
    send_special C-a 0.2
    send_backspace 0.5

    # Move to start of (now) line 2, backspace to merge with line 1
    send_special C-a 0.2
    send_backspace 0.5

    content=$(capture_pane -10)
    show_capture "After collapsing all lines" "$content" 8

    # Should be single line "> abcdefghi"
    if echo "$content" | grep -q '> abcdefghi'; then
        # Also verify no leftover lines with "def" or "ghi" as separate lines
        local leftover
        leftover=$(echo "$content" | grep -c -E '^\s+(def|ghi)' || true)
        if [[ "$leftover" -eq 0 ]]; then
            assert_pass "All lines collapsed to single '> abcdefghi'"
        else
            assert_fail "Merged line correct but $leftover leftover lines remain"
        fi
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "Expected '> abcdefghi' after merging all lines"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

echo -e "${YELLOW}Issue #184: Multi-line redraw bug${NC}"
run_tests 2
