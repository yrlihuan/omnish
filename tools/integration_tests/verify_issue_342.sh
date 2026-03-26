#!/usr/bin/env bash
#
# verify_issue_342.sh - Test parallel tool call status icon rendering
#
# Issue #342: When multiple tools are called in parallel, completion status
# icons create new lines instead of updating the original header in place,
# resulting in duplicate entries.
#
# Tests that:
#   1. Ask LLM to run parallel bash commands
#   2. After completion, each tool header appears exactly once (no duplicates)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Parallel tool calls render without duplicate headers (issue #342)
EOF
}

test_init "issue-342" "$@"

# Count lines matching tool header pattern: "● Name(..." after stripping ANSI
_count_tool_headers() {
    echo "$1" | sed 's/\x1b\[[0-9;]*m//g' | grep -cE '^\s*● ' || true
}

# ── Test 1: Parallel tool calls ──────────────────────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Parallel tool calls render without duplicate headers ===${NC}"

    restart_client
    wait_for_client

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Ask for parallel bash calls (short sleep so they complete quickly)
    send_keys "Run these 3 commands in parallel using bash tool: 'echo AAA', 'echo BBB', 'echo CCC'" 0.3
    send_enter 0.3

    # Wait for LLM to respond (tool calls + final response)
    if ! wait_for_chat_response; then
        show_capture "Timeout" "$(capture_pane -50)" 20
        assert_fail "No chat response after parallel tool request"
        return 1
    fi

    local content=$(capture_pane -50)
    show_capture "After parallel tools" "$content" 25

    # Strip ANSI codes for analysis
    local stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    # Count tool header lines (● Bash(...) or similar)
    local header_count=$(_count_tool_headers "$content")
    echo -e "  Tool header count: $header_count"

    if [[ $header_count -eq 0 ]]; then
        assert_fail "No tool headers found — LLM may not have used parallel tools"
        return 1
    fi

    # Count output lines containing our markers
    local aaa_count=$(echo "$stripped" | grep -c 'AAA' || true)
    local bbb_count=$(echo "$stripped" | grep -c 'BBB' || true)
    local ccc_count=$(echo "$stripped" | grep -c 'CCC' || true)
    echo -e "  Output markers: AAA=$aaa_count BBB=$bbb_count CCC=$ccc_count"

    # The key assertion: each tool should have exactly 1 header line.
    # With 3 parallel tools, we expect exactly 3 headers.
    # The bug would show 6 (3 stale running + 3 new completed).
    if [[ $header_count -le 3 ]]; then
        assert_pass "Tool headers: $header_count (no duplicates)"
        return 0
    fi

    # Allow some tolerance: LLM might have called more tools than requested,
    # but if headers are roughly double the expected count, that's the bug.
    # Count unique header texts to detect actual duplicates.
    local unique_headers=$(echo "$stripped" | grep -E '^\s*● ' | sort -u | wc -l)
    echo -e "  Unique header texts: $unique_headers"

    if [[ $header_count -gt $((unique_headers * 2 - 1)) ]]; then
        assert_fail "Duplicate tool headers detected: $header_count total, $unique_headers unique"
        return 1
    fi

    assert_pass "Tool headers: $header_count total, $unique_headers unique (acceptable)"
    return 0
}

run_tests 1
