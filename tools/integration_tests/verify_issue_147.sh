#!/usr/bin/env bash
#
# verify_issue_147.sh - Test that /debug client and /context show the same cwd
#
# Verifies fix for issue #147: "context中的current dir和/debug client中的current dir不同"
#
# Repro: start omnish, cd /tmp, enter chat, compare cwd in /debug client vs /context

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. After cd /tmp: /debug client should show shell_cwd = /tmp
  2. After cd /tmp: /context | tail should show <current_path> containing /tmp
  3. Both cwds should match
EOF
}

test_init "147" "$@"

DEBUG_CWD=""
CONTEXT_CWD=""

# ── Test 1: /debug client shows correct cwd after cd ─────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: /debug client should show shell_cwd = /tmp after cd ===${NC}"

    start_client
    wait_for_client

    # cd to /tmp
    send_keys "cd /tmp" 0.3
    send_enter 1

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Run /debug client
    send_keys "/debug client" 0.3
    send_enter 3

    local content=$(capture_pane -30)
    show_capture "After /debug client" "$content" 15

    # Extract shell_cwd value
    DEBUG_CWD=$(echo "$content" | grep -oP 'shell_cwd\s*[:=]\s*\K\S+' | head -1)
    if [[ -z "$DEBUG_CWD" ]]; then
        # Try alternate format
        DEBUG_CWD=$(echo "$content" | grep -oP 'cwd\s*[:=]\s*\K\S+' | head -1)
    fi

    if echo "$DEBUG_CWD" | grep -q '/tmp'; then
        assert_pass "/debug client shows cwd containing /tmp (got: $DEBUG_CWD)"
        return 0
    else
        assert_fail "/debug client does not show /tmp (got: '$DEBUG_CWD')"
        return 1
    fi
}

# ── Test 2: /context shows correct cwd after cd ─────────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: /context should show <current_path> containing /tmp ===${NC}"

    # Re-enter chat mode (previous /debug client exited chat)
    send_keys ":" 0.5
    wait_for_prompt

    # Run /context | tail -n 5
    send_keys "/context | tail -n 5" 0.3
    send_enter 3

    local content=$(capture_pane -20)
    show_capture "After /context | tail -n 5" "$content" 10

    # Extract workingDirectory value from <system-reminder>
    CONTEXT_CWD=$(echo "$content" | awk '/<system-reminder>/{flag=1; next} /<\/system-reminder>/{flag=0} flag && /# workingDirectory/{getline; print $1}')

    if echo "$CONTEXT_CWD" | grep -q 'tmp'; then
        assert_pass "/context shows workingDirectory containing /tmp (got: $CONTEXT_CWD)"
        return 0
    else
        assert_fail "/context workingDirectory does not contain /tmp (got: '$CONTEXT_CWD')"
        return 1
    fi
}

# ── Test 3: both cwds should match ───────────────────────────────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: /debug client cwd and /context cwd should both refer to /tmp ===${NC}"

    # Normalize: expand ~ and strip trailing slash
    local norm_debug=$(echo "$DEBUG_CWD" | sed 's|~|'"$HOME"'|; s|/$||')
    local norm_context=$(echo "$CONTEXT_CWD" | sed 's|~|'"$HOME"'|; s|/$||')

    echo -e "  /debug client cwd: ${YELLOW}${DEBUG_CWD}${NC} (normalized: ${norm_debug})"
    echo -e "  /context workingDirectory: ${YELLOW}${CONTEXT_CWD}${NC} (normalized: ${norm_context})"

    if [[ "$norm_debug" == "$norm_context" ]]; then
        assert_pass "Both cwds match: $norm_debug"
        return 0
    else
        assert_fail "Cwds differ: debug='$norm_debug' vs context='$norm_context'"
        return 1
    fi
}

echo -e "${YELLOW}Testing issue #147: /debug client and /context should show same cwd${NC}"
run_tests 3
