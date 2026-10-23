#!/usr/bin/env bash
#
# test_tool_display.sh - Integration test for tool status display issues.
#
# Test cases:
#   1. Tool headers not duplicated when bash command length exceeds one line (#386)
#   2. Long tool output (result_compact) truncated to max 3 terminal rows (#386)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Tool headers not duplicated when bash command exceeds terminal width (#386)
  2. Long tool output truncated to max 3 terminal rows (#386)
EOF
}

test_init "tool-display" "$@"

# ── Test 1: Tool status with long bash command (#386) ────────────────────
# When the bash command (echo argument) is very long, the tool header
# and output may exceed terminal width. The display should not show
# duplicate/orphaned tool headers due to cursor math errors.
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Tool display with long bash command (#386) ===${NC}"

    restart_client
    wait_for_client

    send_keys ":" 0.5
    wait_for_prompt

    # Generate a 260-char string for the echo argument (digits only, no special chars)
    local long_str
    long_str=$(seq -s '' 1 100 | head -c 260)

    # Ask the LLM to run 5 separate echo commands (not a loop)
    send_keys "分别运行5次 echo \"$long_str\", 每次单独调用bash工具, 不要用循环" 0.3
    send_enter 0.3

    if ! wait_for_chat_response; then
        show_capture "After tool execution" "$(capture_pane -100)" 50
        assert_fail "No chat response after tool execution"
        return 1
    fi

    local content
    content=$(capture_pane -300)
    show_capture "Tool display output" "$content" 50

    # Strip ANSI codes for reliable matching
    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    # #386 left orphaned headers: a "● Bash(" redrawn without the cursor
    # going back far enough stays on screen with no result line of its own.
    # Every real call has its header followed by a result line (└ or ⎿)
    # before the next header. Checking that pairing instead of a fixed count
    # keeps the test independent of how many calls the model makes (CI job
    # 10521 ran 10).
    local counts header_count result_count orphans
    counts=$(echo "$stripped" | awk '
        /● Bash\(/                { if (pending) orphans++; headers++; pending = 1; next }
        /^[[:space:]]*(└|⎿)/       { if (pending) results++; pending = 0 }
        END { if (pending) orphans++; print headers + 0, results + 0, orphans + 0 }')
    read -r header_count result_count orphans <<<"$counts"

    echo -e "  Bash tool headers: $header_count, result lines: $result_count, orphaned headers: $orphans"

    if [[ $header_count -eq 0 ]]; then
        assert_fail "No '● Bash(' header: the LLM made no tool call"
        return 1
    fi
    [[ $header_count -ne 5 ]] && echo -e "  ${YELLOW}LLM made $header_count tool calls instead of 5${NC}"
    if [[ $orphans -eq 0 && $header_count -eq $result_count ]]; then
        assert_pass "Every '● Bash(' header has its result line ($header_count calls)"
        return 0
    fi
    assert_fail "Duplicate tool headers: $orphans of $header_count '● Bash(' headers have no result line (#386)"
    return 1
}

# ── Test 2: Long output truncated to max 3 rows (#386) ───────────────────
# When tool output exceeds 3 terminal rows, result_compact should be
# truncated with "…" instead of wrapping indefinitely.
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Long tool output truncated to 3 rows (#386) ===${NC}"

    restart_client
    wait_for_client

    send_keys ":" 0.5
    wait_for_prompt

    # Generate a string ~500 chars long - at 80 cols, prefix "  └  " (or "  ⎿  ") = 5 chars,
    # so content area is 75 chars/row. 3 rows = 225 chars. 500 chars >> 3 rows.
    local long_str
    long_str=$(seq -s '' 1 200 | head -c 500)

    send_keys "使用bash工具执行: echo \"$long_str\"" 0.3
    send_enter 0.3

    if ! wait_for_chat_response; then
        show_capture "After tool execution" "$(capture_pane -60)" 30
        assert_fail "No chat response after tool execution"
        return 1
    fi

    local content
    content=$(capture_pane -60)
    show_capture "Tool output" "$content" 30

    # Strip ANSI codes
    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    # Find the tool output line (└ or ⎿ depending on extended_unicode) and check it ends with …
    local output_line
    output_line=$(echo "$stripped" | grep -E '[└⎿]' | head -1)

    if [[ -z "$output_line" ]]; then
        assert_fail "No tool output line (└/⎿) found"
        return 1
    fi

    echo -e "  Output line length: ${#output_line}"

    # The output should be truncated (contains …) since 500 chars > 3 rows
    if echo "$output_line" | grep -q '…'; then
        assert_pass "Long output truncated with … (len=${#output_line})"
        return 0
    else
        # Check if the full 500-char string is present (not truncated)
        if echo "$output_line" | grep -q "$long_str"; then
            assert_fail "Output not truncated - full string present"
            return 1
        fi
        assert_pass "Output appears truncated (no … but string is shorter)"
        return 0
    fi
}

echo -e "${YELLOW}Tool display integration test (#386)${NC}"
run_tests 2
