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
  3. Non-bracketed paste with 3 leading CRs: paste "\r\r\rhello" → 3 empty lines + "hello"
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

    # Count how many times "> hello" appears - should be exactly 1
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

# ── Test 3: Non-bracketed paste with leading CRs ────────────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: Fast-paste detection with 3 leading CRs ===${NC}"

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Simulate non-bracketed paste: send raw bytes via -l (literal mode),
    # which bypasses tmux's bracketed paste wrapping.
    # Content: CR CR CR h e l l o - three newlines then "hello"
    echo -e "  Sending: ${YELLOW}(raw paste) \\r\\r\\rhello${NC}"
    _tmux send-keys -t "$PANE" -l $'\r\r\rhello'
    sleep 0.5

    local content=$(capture_pane -15)
    show_capture "After fast-paste with 3 leading CRs" "$content" 10

    # Expected: 4 editor lines - 3 empty + "hello"
    # "> "        (line 1, empty)
    # "  "        (line 2, empty)
    # "  "        (line 3, empty)
    # "  hello"   (line 4)
    if echo "$content" | grep -qE '^\s+hello'; then
        assert_pass "Fast-paste: 3 leading CRs + 'hello' rendered as multi-line"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "Fast-paste: expected multi-line with 'hello', got unexpected output"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 4: Large paste (≥10 lines) collapses to marker ──────────────
test_4() {
    echo -e "\n${YELLOW}=== Test 4: Large paste collapses to [pasted text] marker ===${NC}"

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Paste 12 lines via raw send (no bracketed paste markers)
    local paste_content=""
    for i in $(seq 1 12); do
        paste_content="${paste_content}line${i}"$'\r'
    done
    echo -e "  Sending: ${YELLOW}(raw paste 12 lines)${NC}"
    _tmux send-keys -t "$PANE" -l "$paste_content"
    sleep 0.5

    local content=$(capture_pane -10)
    show_capture "After pasting 12 lines" "$content" 8

    # Should see collapsed marker, not 12 individual lines
    if ! echo "$content" | grep -q 'pasted text #1'; then
        assert_fail "Expected [pasted text #1] marker for 12-line paste"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
    assert_pass "Large paste collapsed to [pasted text #1] marker"

    # First backspace: should go back to marker line (not delete block)
    send_backspace 0.5

    content=$(capture_pane -10)
    show_capture "After first backspace (merge to marker)" "$content" 5

    if ! echo "$content" | grep -q 'pasted text #1'; then
        assert_fail "First backspace should NOT delete paste block"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
    assert_pass "First backspace: paste block still visible"

    # Second backspace: should delete entire paste block
    send_backspace 0.5

    content=$(capture_pane -10)
    show_capture "After second backspace (delete block)" "$content" 5

    if echo "$content" | grep -q 'pasted text'; then
        assert_fail "Second backspace should delete paste block"
        send_special Escape 0.5
        sleep 1.5
        return 1
    else
        assert_pass "Second backspace deleted entire paste block"
        send_special Escape 0.5
        sleep 1.5
        return 0
    fi
}

echo -e "${YELLOW}Issue #184: Multi-line redraw bug${NC}"
run_tests 4
