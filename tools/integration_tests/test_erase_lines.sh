#!/usr/bin/env bash
#
# test_erase_lines.sh - Tests for multi-line terminal erasure behavior (#537).
#
# Verifies that erasure sequences correctly handle content spanning multiple
# visual rows (from wrapping or multi-line input).
#
# Test cases:
#   1. Early cancel with wrapping input — check for orphaned echo lines
#   2. Early cancel with short input — baseline (single row, should work)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Early cancel with wrapping input — orphaned line detection (#537)
  2. Early cancel with short input — baseline (single row)
EOF
}

test_init "erase-lines" "$@"

# ── Test 1: Early cancel with wrapping input ────────────────────────────
# When the user echo wraps to 2+ visual rows but only 1 row is erased,
# the orphaned row(s) remain visible above the re-displayed editor.
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Early cancel with wrapping input (#537) ===${NC}"

    restart_client
    wait_for_client

    enter_chat

    # Build a message that wraps to 2 visual rows at 80 cols.
    # "> " prefix = 2 display chars, so 100 content chars → 102 total → 2 rows.
    # Use a unique marker so we can count occurrences reliably.
    local marker="ERASETEST537"
    local padding
    padding=$(python3 -c "print('x' * (100 - len('$marker')))")
    local msg="${marker}${padding}"

    send_keys "$msg" 0.3
    send_enter 0.3

    # Cancel early — before the LLM has a chance to respond.
    sleep 1
    echo -e "  Sending Ctrl-C to cancel early..."
    send_special C-c 1

    local content
    content=$(capture_pane -20)

    # Strip ANSI codes for reliable matching
    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    show_capture "After early cancel (wrapping)" "$content" 8

    # Count lines containing the marker.
    # Correct behaviour: marker appears on exactly 1 visual row (the editor's
    # first row of the restored input: "> ERASETEST537xxx...").
    # Bug: the orphaned first row of the old echo also contains the marker → 2 lines.
    local marker_lines
    marker_lines=$(echo "$stripped" | grep -c "$marker" || true)

    echo -e "  Lines containing marker: $marker_lines (expected 1)"
    if [[ $marker_lines -le 1 ]]; then
        assert_pass "No orphaned echo lines (marker on $marker_lines line(s))"
        return 0
    else
        assert_fail "Orphaned echo line detected: marker on $marker_lines lines (expected 1)"
        return 1
    fi
}

# ── Test 2: Early cancel with short (non-wrapping) input — baseline ─────
# A short input fits on 1 visual row.  The existing single-line erase is
# sufficient, so this should always pass.
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Early cancel with short input (baseline) ===${NC}"

    restart_client
    wait_for_client

    enter_chat

    local msg="SHORTTEST hello world"
    send_keys "$msg" 0.3
    send_enter 0.3

    sleep 1
    echo -e "  Sending Ctrl-C to cancel early..."
    send_special C-c 1

    local content
    content=$(capture_pane -10)
    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    show_capture "After early cancel (short)" "$content" 5

    # The restored editor should show the input once.
    local marker_lines
    marker_lines=$(echo "$stripped" | grep -c "SHORTTEST" || true)

    echo -e "  Lines containing marker: $marker_lines (expected 1)"
    if [[ $marker_lines -eq 1 ]]; then
        assert_pass "Short input restored cleanly"
    else
        assert_fail "Expected 1 marker line, found $marker_lines"
        return 1
    fi

    # No "User interrupted" message should appear for early cancel.
    if echo "$stripped" | grep -q "User interrupted"; then
        assert_fail "Should not show 'User interrupted' on early cancel"
        return 1
    fi

    assert_pass "No 'User interrupted' message"
    return 0
}

run_tests 2
