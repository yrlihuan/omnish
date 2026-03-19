#!/usr/bin/env bash
#
# test_cwd_chat.sh - Verify cwd is correct after cd then immediate chat entry (#354)
#
# Test case:
#   1. cd to a known path, immediately enter chat, dump context, verify cwd matches

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. cd to path then immediate chat — verify cwd in /context output
EOF
}

test_init "cwd-chat" "$@"

# ── Test 1: cd then immediate chat — cwd should match ────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: cd then immediate chat — cwd in context ===${NC}"

    start_client
    wait_for_client

    local test_dir="/tmp/omnish_cwd_test_$$"
    local out_file="/tmp/omnish_cwd_ctx_$$.txt"

    # Create the target directory
    send_keys "mkdir -p $test_dir" 0.3
    send_enter 1

    # cd to the target directory, then enter chat as quickly as possible
    # We need the shell prompt to appear first (so ":" doesn't merge with prior input),
    # but we want minimal delay to trigger the race condition in cwd tracking.
    send_keys "cd $test_dir" 0.3
    send_enter 0.3

    # Wait for the NEW prompt (containing the test dir path) to confirm cd completed
    local pw=0
    while [[ $pw -lt 20 ]]; do
        local pp
        pp=$(capture_pane -3)
        if echo "$pp" | grep -q "omnish_cwd_test.*\\$"; then
            break
        fi
        sleep 0.1
        pw=$((pw + 1))
    done
    # Enter chat mode — wait 1s for prompt to stabilize before sending ":"
    sleep 1
    send_keys ":" 1

    # Verify we're in chat mode ("> " prompt)
    local cw=0
    while [[ $cw -lt 10 ]]; do
        local cp
        cp=$(capture_pane -5)
        if is_chat_prompt "$cp"; then
            echo -e "  Chat prompt detected after ${cw}s"
            break
        fi
        sleep 1
        cw=$((cw + 1))
    done

    # Dump context to file — /context is an inspection command so it auto-exits chat
    send_keys "/context chat > $out_file" 0.3
    send_enter 1

    # Wait for auto-exit back to shell prompt (up to 15s)
    local waited=0
    while [[ $waited -lt 15 ]]; do
        local pane
        pane=$(capture_pane -10)
        if is_shell_prompt "$pane"; then
            echo -e "  Shell prompt detected after ${waited}s"
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done
    sleep 0.5

    # Read the output file and check for the correct cwd
    local ctx_content
    ctx_content=$(cat "$out_file" 2>/dev/null || echo "")

    if [[ -z "$ctx_content" ]]; then
        assert_fail "Context output file is empty or missing: $out_file"
        rm -rf "$test_dir" "$out_file"
        return 1
    fi

    echo -e "  Checking for workingDirectory in context output..."

    # Look for WORKING DIR in the context output
    if echo "$ctx_content" | grep -q "WORKING DIR:"; then
        local found_dir
        found_dir=$(echo "$ctx_content" | grep "WORKING DIR:" | sed 's/.*WORKING DIR: *//' | tr -d '[:space:]')
        echo -e "  Found WORKING DIR: ${YELLOW}${found_dir}${NC}"

        if [[ "$found_dir" == "$test_dir" ]]; then
            assert_pass "cwd correctly updated to $test_dir after cd + immediate chat"
            rm -rf "$test_dir" "$out_file"
            return 0
        else
            assert_fail "cwd is '$found_dir', expected '$test_dir'"
            rm -rf "$test_dir" "$out_file"
            return 1
        fi
    else
        echo -e "  ${YELLOW}Context output (last 20 lines):${NC}"
        echo "$ctx_content" | tail -20 | sed 's/^/    /'
        assert_fail "WORKING DIR not found in context output"
        rm -rf "$test_dir" "$out_file"
        return 1
    fi
}

echo -e "${YELLOW}CWD chat integration test: verify cwd after cd + immediate chat (#354)${NC}"
run_tests 1
