#!/usr/bin/env bash
#
# verify_issue_635.sh - Verify fix for issue #635
#
# Bug: when a ghost-text completion wraps below the prompt row, pressing an
# arrow key that ends-of-input (Home / Left / End) goes through the
# "readline key" erase path. If erase_ghost_text() does not honour the
# full wrap row count, the wrapped portion below the prompt remains as a
# stale artifact (e.g. "es" stranded on row 2 after pressing Home over a
# " yes" ghost that wrapped two cells past column 80).
#
# The originating test_basic Test 13 hit this via an LLM race: a slow
# background completion for input="" returned a long suggestion that
# wrapped, then the canned " yes" suggestion overlapped and Home failed to
# clean the prior wrap row. This script removes the timing dependency by
# using the daemon's "omnish_debug N" debug shortcut to deterministically
# request a multi-row wrapping ghost, then verifies that each arrow-key
# erase path leaves the wrap rows clean.
#
# Test cases:
#   1. Long ghost wraps; Home clears all wrapped portions (#635)
#   2. Long ghost wraps; Left arrow clears all wrapped portions
#   3. Long ghost wraps; End key clears all wrapped portions
#   4. Delayed wrapping ghost (delay + length combined form) cleared by Home

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Long ghost wraps; Home clears all wrapped portions (#635)
  2. Long ghost wraps; Left arrow clears all wrapped portions
  3. Long ghost wraps; End key clears all wrapped portions
  4. Delayed wrapping ghost (delay + length combined form) cleared by Home
EOF
}

test_init "issue-635" "$@"

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

# Capture pane WITHOUT -J so wrapped rows stay as separate physical lines.
_capture_pane_unwrapped() {
    local lines="${1:--40}"
    _tmux capture-pane -p -t "$PANE" -S "$lines"
}

# Count physical pane rows that contain >= 50 consecutive 'x' characters.
# "omnish_debug N" emits a ghost suffix of " " + (N-1) 'x's; each wrap row
# that holds a packed slice of that suffix contains a long 'x' run.
_dense_x_row_count() {
    _capture_pane_unwrapped -40 | _strip_ansi | grep -cE 'x{50,}' || true
}

# Render a wrapping ghost via "omnish_debug 200" and assert it actually
# wrapped across at least two rows. Returns 0 on success, 1 if the ghost
# never rendered or did not wrap.
#
# On success, leaves the prompt with the input "omnish_debug 200" typed
# and the ghost text visible. On failure, sends C-c.
_render_wrapping_ghost() {
    send_keys "omnish_debug 200" 0.3

    sleep 0.5
    local ghost_appeared=false
    local attempt
    for attempt in $(seq 1 20); do
        sleep 0.5
        if [[ $(_dense_x_row_count) -ge 1 ]]; then
            ghost_appeared=true
            break
        fi
    done

    if ! $ghost_appeared; then
        local raw
        raw=$(_capture_pane_unwrapped -40)
        show_capture "No ghost text" "$raw" 24
        send_special C-c 0.5
        return 1
    fi

    local wrap_lines_before
    wrap_lines_before=$(_dense_x_row_count)
    if [[ $wrap_lines_before -lt 2 ]]; then
        local raw
        raw=$(_capture_pane_unwrapped -40)
        show_capture "Ghost did not wrap enough" "$raw" 24
        echo -e "  Rows with dense ghost runs: ${wrap_lines_before} (expected >= 2)"
        send_special C-c 0.5
        return 1
    fi

    return 0
}

# Press the named arrow key on top of a wrapping ghost and assert that
# every wrap row is cleaned and the cursor stays on the prompt row. The
# typed input "omnish_debug 200" must still be visible after the keypress.
_arrow_key_clears_wrapping_ghost() {
    local key="$1"
    local label="$2"

    start_client_fixed_size
    wait_for_client

    if ! _render_wrapping_ghost; then
        assert_fail "Ghost text did not wrap for ${label}"
        return 1
    fi

    local raw_before
    raw_before=$(_capture_pane_unwrapped -40)
    show_capture "Ghost visible (wrapping, before ${label})" "$raw_before" 24

    local cursor_y_before
    cursor_y_before=$(_tmux display-message -p -t "$PANE" '#{cursor_y}')
    echo -e "  cursor_y before ${label}: ${cursor_y_before}"

    send_special "$key" 0.5
    sleep 0.5

    local raw_after
    raw_after=$(_capture_pane_unwrapped -40)
    show_capture "After ${label}" "$raw_after" 24

    local wrap_lines_after
    wrap_lines_after=$(_dense_x_row_count)
    echo -e "  Rows with dense ghost runs after ${label}: ${wrap_lines_after}"

    local cursor_y_after
    cursor_y_after=$(_tmux display-message -p -t "$PANE" '#{cursor_y}')
    echo -e "  cursor_y after ${label}: ${cursor_y_after}"

    local passed=true

    if [[ $wrap_lines_after -gt 0 ]]; then
        assert_fail "Stale wrapped ghost remains on ${wrap_lines_after} line(s) after ${label} (#635)"
        passed=false
    fi

    if [[ "$cursor_y_before" != "$cursor_y_after" ]]; then
        assert_fail "Prompt row moved after ${label}: ${cursor_y_before} -> ${cursor_y_after} (#635)"
        passed=false
    fi

    local prompt_line
    prompt_line=$(echo "$raw_after" | _strip_ansi | sed -n "$((cursor_y_after + 1))p")
    echo -e "  Prompt row after ${label}: '${prompt_line}'"
    if ! echo "$prompt_line" | grep -q 'omnish_debug 200'; then
        assert_fail "Typed input 'omnish_debug 200' missing from prompt row after ${label}"
        passed=false
    fi

    send_special C-c 0.5

    if $passed; then
        assert_pass "Wrapped ghost cleared by ${label} (#635)"
        return 0
    fi
    return 1
}

# ── Test 1: Home over wrapping ghost ──
# Home is the exact key that triggered the original #635 failure: it goes
# through needs_readline_report() in main.rs and erases via the
# "readline_key" path, which must honour the full wrap row count.
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Wrapped ghost cleared by Home (#635) ===${NC}"
    _arrow_key_clears_wrapping_ghost Home "Home key"
}

# ── Test 2: Left arrow over wrapping ghost ──
# Left arrow also enters the readline_key erase path. The original
# test_basic Test 13 first sub-test (Left) passed because the ghost was
# only 4 chars and did not wrap in its environment. With a forced wrap,
# the same code path must still clean every row.
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Wrapped ghost cleared by Left arrow ===${NC}"
    _arrow_key_clears_wrapping_ghost Left "Left arrow"
}

# ── Test 3: End key over wrapping ghost ──
# End is symmetric to Home and exercises the same erase path. Covers the
# remaining "cursor-jumping" arrow key handled by needs_readline_report().
test_3() {
    echo -e "\n${YELLOW}=== Test 3: Wrapped ghost cleared by End key ===${NC}"
    _arrow_key_clears_wrapping_ghost End "End key"
}

# ── Test 4: delay + length combined form ──
# Uses the "omnish_debug delay <ms> length <N>" form (extended for #635)
# to stage a slow wrapping response. The 1s daemon sleep pushes rendering
# past the 500ms pending-completion timeout in main.rs, so the ghost lands
# via the timeout-render path (line 1819) instead of the ReadlineLine path.
# Verifies that the wrap-row tracking is correct in BOTH render paths.
test_4() {
    echo -e "\n${YELLOW}=== Test 4: Delayed wrapping ghost cleared by Home (delay+length) ===${NC}"

    start_client_fixed_size
    wait_for_client

    # Request a 200-char ghost after a 1000ms server-side delay.
    send_keys "omnish_debug delay 1000 length 200" 0.3

    # Wait through: 500ms debounce + 1000ms daemon delay + ~100ms transport.
    # Then poll for the wrapping ghost to appear.
    sleep 1.5
    local ghost_appeared=false
    local attempt
    for attempt in $(seq 1 20); do
        sleep 0.5
        if [[ $(_dense_x_row_count) -ge 1 ]]; then
            ghost_appeared=true
            break
        fi
    done

    if ! $ghost_appeared; then
        local raw
        raw=$(_capture_pane_unwrapped -40)
        show_capture "No ghost after delayed response" "$raw" 24
        assert_fail "Delayed ghost text did not appear"
        send_special C-c 0.5
        return 1
    fi

    local wrap_lines_before
    wrap_lines_before=$(_dense_x_row_count)
    if [[ $wrap_lines_before -lt 2 ]]; then
        local raw
        raw=$(_capture_pane_unwrapped -40)
        show_capture "Delayed ghost did not wrap enough" "$raw" 24
        assert_fail "Expected delayed ghost to wrap across multiple rows (saw ${wrap_lines_before})"
        send_special C-c 0.5
        return 1
    fi

    local raw_before
    raw_before=$(_capture_pane_unwrapped -40)
    show_capture "Delayed wrapping ghost visible" "$raw_before" 24

    local cursor_y_before
    cursor_y_before=$(_tmux display-message -p -t "$PANE" '#{cursor_y}')

    send_special Home 0.5
    sleep 0.5

    local raw_after
    raw_after=$(_capture_pane_unwrapped -40)
    show_capture "After Home" "$raw_after" 24

    local wrap_lines_after
    wrap_lines_after=$(_dense_x_row_count)
    local cursor_y_after
    cursor_y_after=$(_tmux display-message -p -t "$PANE" '#{cursor_y}')

    local passed=true

    if [[ $wrap_lines_after -gt 0 ]]; then
        assert_fail "Stale wrapped ghost remains on ${wrap_lines_after} line(s) after Home (delay+length path)"
        passed=false
    fi

    if [[ "$cursor_y_before" != "$cursor_y_after" ]]; then
        assert_fail "Prompt row moved after Home: ${cursor_y_before} -> ${cursor_y_after}"
        passed=false
    fi

    # Real input "omnish_debug delay 1000 length 200" must still be on the prompt row.
    local prompt_line
    prompt_line=$(echo "$raw_after" | _strip_ansi | sed -n "$((cursor_y_after + 1))p")
    echo -e "  Prompt row after Home: '${prompt_line}'"
    if ! echo "$prompt_line" | grep -q 'omnish_debug delay 1000 length 200'; then
        assert_fail "Typed input missing from prompt row after Home"
        passed=false
    fi

    send_special C-c 0.5

    if $passed; then
        assert_pass "Delayed wrapping ghost cleared by Home (delay+length form)"
        return 0
    fi
    return 1
}

echo -e "${YELLOW}Issue #635 verification: arrow keys must clear all wrapped ghost rows${NC}"
run_tests 4
