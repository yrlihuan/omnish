#!/usr/bin/env bash
#
# verify_issue_629.sh - Verify fix for issue #629
#
# Bug: when a ghost-text completion is long enough to wrap to one or more
# following lines, dismissing the completion (ESC, divergent typing, etc.)
# does not erase the wrapped portion on the lines below the prompt, leaving
# stale ghost characters visible and the apparent prompt row pushed down.
#
# Reproduction relies on the daemon's "omnish_debug N" debug shortcut: it
# returns a canned completion whose ghost suffix is exactly N characters,
# so we can deterministically force wrap regardless of LLM availability or
# context state.
#
# Test cases:
#   1. Long ghost wraps to multiple lines; ESC clears all wrapped portions

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Long ghost wraps; ESC clears all wrapped portions (#629)
EOF
}

test_init "issue-629" "$@"

# Start a client in a fixed 80x24 tmux pane so wrap math is deterministic.
start_client_fixed_size() {
    _tmux kill-session -t "$SESSION" 2>/dev/null || true
    _wait_for_server_gone
    local attempt
    for attempt in 1 2 3; do
        if _tmux new -d -s "$SESSION" -x 80 -y 24 -n test \
                "OMNISH_LANG=$OMNISH_LANG_DEFAULT $CLIENT" 2>/dev/null; then
            if _tmux has-session -t "$SESSION" 2>/dev/null; then
                return 0
            fi
        fi
        sleep 0.2
    done
    echo -e "  ${RED}start_client_fixed_size: failed after 3 attempts${NC}" >&2
    return 1
}

# Strip ANSI codes for plain-text inspection of a captured pane.
_strip_ansi() {
    sed 's/\x1b\[[0-9;?]*[A-Za-z]//g; s/\x1b[78]//g'
}

# ── Test 1: long wrapping ghost; ESC erases all wrapped lines ──
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Long ghost wraps; ESC clears wrapped portions (#629) ===${NC}"

    start_client_fixed_size
    wait_for_client

    # Request a 200-char ghost suffix. With an 80-col pane and a short
    # prompt+input (≈ "user@host:~$ omnish_debug 200" → ~30 cols), this
    # forces at least two wrap lines below the prompt row.
    send_keys "omnish_debug 200" 0.3

    # Poll for ghost text to appear (debounce ~500ms + round trip).
    sleep 0.5
    local content=""
    local ghost_appeared=false
    local attempt
    for attempt in $(seq 1 20); do
        sleep 0.5
        content=$(capture_pane -40)
        local stripped
        stripped=$(echo "$content" | _strip_ansi)
        # The N-char ghost is " xxxx...". Detect by looking for a run of
        # at least 50 'x' characters anywhere in the pane (covers both the
        # current line and any wrapped lines below).
        if echo "$stripped" | grep -qE 'x{50,}'; then
            ghost_appeared=true
            break
        fi
    done

    if ! $ghost_appeared; then
        show_capture "No ghost text" "$content" 24
        assert_fail "Ghost text did not appear for 'omnish_debug 200'"
        send_special C-c 0.5
        return 1
    fi

    show_capture "Ghost visible (wrapping)" "$content" 24

    # Find the prompt row (last line ending with shell prompt + typed input).
    local stripped
    stripped=$(echo "$content" | _strip_ansi)

    # Count how many wrap lines the ghost occupies. The 200-char suffix
    # written after ~30 cols of prompt+input on an 80-col pane should
    # produce at least two wrap lines worth of 'x' below the prompt row.
    local wrap_lines_before
    wrap_lines_before=$(echo "$stripped" | grep -cE 'x{50,}' || true)
    echo -e "  Lines containing dense ghost runs before ESC: ${wrap_lines_before}"
    if [[ $wrap_lines_before -lt 2 ]]; then
        assert_fail "Expected ghost to wrap across multiple lines (saw ${wrap_lines_before})"
        send_special C-c 0.5
        return 1
    fi

    # Record cursor row before dismiss.
    local cursor_y_before
    cursor_y_before=$(_tmux display-message -p -t "$PANE" '#{cursor_y}')
    echo -e "  cursor_y before ESC: ${cursor_y_before}"

    # Press ESC to dismiss ghost text.
    send_special Escape 0.5
    sleep 0.5

    content=$(capture_pane -40)
    show_capture "After ESC dismiss" "$content" 24

    stripped=$(echo "$content" | _strip_ansi)

    # After dismiss, NO line should contain a long 'x' run anymore.
    local wrap_lines_after
    wrap_lines_after=$(echo "$stripped" | grep -cE 'x{50,}' || true)
    echo -e "  Lines containing dense ghost runs after ESC: ${wrap_lines_after}"

    # Cursor row should match the prompt row (same as before, since ESC
    # only dismisses ghost; it must not move the prompt).
    local cursor_y_after
    cursor_y_after=$(_tmux display-message -p -t "$PANE" '#{cursor_y}')
    echo -e "  cursor_y after ESC: ${cursor_y_after}"

    local passed=true

    if [[ $wrap_lines_after -gt 0 ]]; then
        assert_fail "Stale ghost text remains on ${wrap_lines_after} line(s) after dismiss (#629)"
        passed=false
    fi

    if [[ "$cursor_y_before" != "$cursor_y_after" ]]; then
        assert_fail "Prompt row moved after ghost dismiss: ${cursor_y_before} -> ${cursor_y_after} (#629)"
        passed=false
    fi

    # Real input on the prompt row should still be present.
    local prompt_line
    prompt_line=$(echo "$stripped" | sed -n "$((cursor_y_after + 1))p")
    echo -e "  Prompt row after ESC: '${prompt_line}'"
    if ! echo "$prompt_line" | grep -q 'omnish_debug 200'; then
        assert_fail "Typed input 'omnish_debug 200' missing from prompt row after dismiss"
        passed=false
    fi

    send_special C-c 0.5

    if $passed; then
        assert_pass "Wrapped ghost fully cleared and cursor restored on dismiss (#629)"
        return 0
    fi
    return 1
}

echo -e "${YELLOW}Issue #629 verification: wrapped ghost text not cleared on dismiss${NC}"
run_tests 1
