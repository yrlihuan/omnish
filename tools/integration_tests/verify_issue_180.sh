#!/usr/bin/env bash
#
# verify_issue_180.sh - Integration test for line editor in chat mode
#
# Test cases:
#   1. Left/Right arrow cursor movement + mid-line insertion
#   2. Home/End (Ctrl-A/Ctrl-E) + Ctrl-U kill to start
#   3. Multi-line editing with Ctrl-J
#   4. Backspace merges lines
#   5. Shift+Enter inserts newline (issue #579)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Left arrow + mid-line insert: type "hllo", Left×3, insert "e" → "hello"
  2. Ctrl-A (home) + Ctrl-U (kill): type "hello world", Ctrl-A to home, Ctrl-E to end, Ctrl-U to kill
  3. Multi-line: type "line1", Ctrl-J, "line2", submit → content has both lines
  4. Backspace merges lines: type "ab", Ctrl-J, "cd", Backspace×2 at start → merged
  5. Shift+Enter inserts newline (issue #579): type "line1", S-Enter, "line2" → two lines, no submit
EOF
}

test_init "issue-180" "$@"

# ── Test 1: Left arrow + mid-line insert ─────────────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Left arrow + mid-line insert ===${NC}"

    start_client
    wait_for_client

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Type "hllo"
    send_keys "hllo" 0.3

    # Move left 3 times to position cursor after 'h'
    send_special Left 0.2
    send_special Left 0.2
    send_special Left 0.2

    # Insert 'e' at cursor position → "hello"
    send_keys "e" 0.3

    local content=$(capture_pane -10)
    show_capture "After mid-line insert" "$content" 5

    # The prompt line should show "> hello"
    if echo "$content" | grep -q '> hello'; then
        assert_pass "Mid-line insert: 'hllo' + Left×3 + 'e' = 'hello'"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "Expected '> hello' in output"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 2: Ctrl-A, Ctrl-E, Ctrl-U ──────────────────────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Ctrl-A (home), Ctrl-E (end), Ctrl-U (kill) ===${NC}"

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Type "hello world"
    send_keys "hello world" 0.3

    # Ctrl-A moves to start, then type "X" to prove we're at position 0
    send_special C-a 0.2
    send_keys "X" 0.3

    local content=$(capture_pane -10)
    show_capture "After Ctrl-A + insert X" "$content" 5

    if ! echo "$content" | grep -q '> Xhello world'; then
        assert_fail "Ctrl-A did not move to start (expected '> Xhello world')"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    # Ctrl-E moves to end, then type "!" to prove we're at the end
    send_special C-e 0.2
    send_keys "!" 0.3

    content=$(capture_pane -10)
    show_capture "After Ctrl-E + insert !" "$content" 5

    if ! echo "$content" | grep -q '> Xhello world!'; then
        assert_fail "Ctrl-E did not move to end (expected '> Xhello world!')"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    # Ctrl-U kills to start of line → should leave empty prompt
    send_special C-u 0.2

    content=$(capture_pane -10)
    show_capture "After Ctrl-U" "$content" 5

    # After Ctrl-U, "Xhello world!" should be gone from the prompt line.
    # The content before Ctrl-U had "Xhello world!" - verify it's no longer there.
    if echo "$content" | grep -q 'Xhello world'; then
        assert_fail "Ctrl-U did not kill line (text still visible)"
        send_special Escape 0.5
        sleep 1.5
        return 1
    else
        assert_pass "Ctrl-A/Ctrl-E/Ctrl-U work correctly"
        send_special Escape 0.5
        sleep 1.5
        return 0
    fi
}

# ── Test 3: Multi-line editing with Ctrl-J ───────────────────────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: Multi-line editing with Ctrl-J ===${NC}"

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Type "line1", Ctrl-J (newline), "line2"
    send_keys "line1" 0.3
    # Ctrl-J = LF = 0x0a
    send_special C-j 0.3
    send_keys "line2" 0.3

    local content=$(capture_pane -10)
    show_capture "After multi-line input" "$content" 5

    # Should see two lines: "> line1" and "  line2"
    local has_line1=false has_line2=false
    if echo "$content" | grep -q '> line1'; then
        has_line1=true
    fi
    if echo "$content" | grep -q '  line2'; then
        has_line2=true
    fi

    if [[ "$has_line1" == "true" && "$has_line2" == "true" ]]; then
        assert_pass "Multi-line: two lines visible with correct prefixes"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "Expected '> line1' and '  line2' (line1=$has_line1, line2=$has_line2)"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 4: Backspace merges lines ───────────────────────────────────────
test_4() {
    echo -e "\n${YELLOW}=== Test 4: Backspace merges lines ===${NC}"

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Type "ab", Ctrl-J (newline), "cd"
    send_keys "ab" 0.3
    send_special C-j 0.3
    send_keys "cd" 0.3

    # Verify two lines first
    local content=$(capture_pane -10)
    show_capture "Before backspace merge" "$content" 5

    # Move to start of line2 with Home
    send_special C-a 0.2
    # Backspace at start of line2 should merge with line1
    send_backspace 0.3

    content=$(capture_pane -10)
    show_capture "After backspace merge" "$content" 5

    # Should now be single line "> abcd"
    if echo "$content" | grep -q '> abcd'; then
        assert_pass "Backspace at line start merges into previous line"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "Expected '> abcd' after merge"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 5: Shift+Enter inserts newline (issue #579) ─────────────────────
# On Windows (and other terminals that don't opt into CSI u by default),
# Shift+Enter used to arrive as `\r` and submit the message. Chat mode now
# writes `\x1b[>4;2m` on entry to enable modifyOtherKeys, so tmux forwards
# S-Enter as `\x1b[13;2u` (requires `set -g extended-keys on`, set in lib.sh).
test_5() {
    echo -e "\n${YELLOW}=== Test 5: Shift+Enter should insert newline (issue #579) ===${NC}"

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Type "line1", Shift+Enter, "line2"
    send_keys "line1" 0.3
    send_special S-Enter 0.3
    send_keys "line2" 0.3

    local content=$(capture_pane -10)
    show_capture "After line1 <S-Enter> line2" "$content" 5

    # If S-Enter was treated as submit, "line2" would appear on a fresh
    # prompt, not as a continuation line ("  line2").
    local has_line1=false has_line2=false
    if echo "$content" | grep -q '> line1'; then
        has_line1=true
    fi
    if echo "$content" | grep -q '  line2'; then
        has_line2=true
    fi

    if [[ "$has_line1" == "true" && "$has_line2" == "true" ]]; then
        assert_pass "Shift+Enter: two lines visible, no submission"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "Expected '> line1' and '  line2' (line1=$has_line1, line2=$has_line2)"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

echo -e "${YELLOW}Issue #180: Line editor integration test${NC}"
run_tests 5
