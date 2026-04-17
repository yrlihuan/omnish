#!/usr/bin/env bash
#
# test_thread_sandbox.sh - Integration tests for /thread sandbox on|off (#535).
#
# Test cases:
#   1. /thread sandbox off → persists sandbox_disabled in thread meta file
#   2. /thread sandbox on  → clears sandbox_disabled override
#   3. /thread sandbox     → shows current state (on/off)
#   4. /thread stats shows "sandbox: off" when disabled

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. /thread sandbox off persists to meta file
  2. /thread sandbox on clears override
  3. /thread sandbox shows current state
  4. /thread stats shows "sandbox: off" when disabled
EOF
}

test_init "thread-sandbox" "$@"

THREADS_DIR="${OMNISH_HOME:-$HOME/.omnish}/threads"

# Helper: get the most recently modified .meta.json file in threads dir.
latest_meta_file() {
    ls -t "$THREADS_DIR"/*.meta.json 2>/dev/null | head -1
}

# Helper: check if a meta file has sandbox_disabled set to true.
meta_has_sandbox_disabled() {
    local file="$1"
    grep -q '"sandbox_disabled"' "$file" 2>/dev/null && \
        python3 -c "import json,sys; d=json.load(open('$file')); sys.exit(0 if d.get('sandbox_disabled')==True else 1)" 2>/dev/null
}

# Helper: check that a meta file does NOT have sandbox_disabled set.
meta_no_sandbox_disabled() {
    local file="$1"
    # Either the key is absent, or it's null/not true
    if ! grep -q '"sandbox_disabled"' "$file" 2>/dev/null; then
        return 0
    fi
    python3 -c "import json,sys; d=json.load(open('$file')); sys.exit(0 if d.get('sandbox_disabled') is None or d.get('sandbox_disabled')!=True else 1)" 2>/dev/null
}

# ── Test 1: /thread sandbox off → persists to meta ─────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: /thread sandbox off persists to meta file ===${NC}"

    restart_client
    wait_for_client

    # Enter chat and send a message to create a thread
    enter_chat
    send_keys "Reply with just the word: ok" 0.3
    send_enter 0.3
    if ! wait_for_chat_response; then
        show_capture "After first message" "$(capture_pane -20)" 10
        assert_fail "No chat response"
        return 1
    fi

    # Now set sandbox off
    send_keys "/thread sandbox off" 0.3
    send_enter 1

    local content
    content=$(capture_pane -20)
    show_capture "After /thread sandbox off" "$content" 10

    # Verify response confirms sandbox disabled
    if ! echo "$content" | grep -qi "sandbox.*disabled\|sandbox.*off"; then
        assert_fail "No confirmation of sandbox disabled"
        return 1
    fi

    # Check meta file on disk
    sleep 0.5
    local meta_file
    meta_file=$(latest_meta_file)
    if [[ -z "$meta_file" ]]; then
        assert_fail "No meta file found in $THREADS_DIR"
        return 1
    fi
    echo -e "  Meta file: $meta_file"

    if meta_has_sandbox_disabled "$meta_file"; then
        assert_pass "/thread sandbox off persists sandbox_disabled=true in meta file"
        return 0
    else
        echo -e "  Meta content:"
        cat "$meta_file" | sed 's/^/    /'
        assert_fail "sandbox_disabled not set to true in meta file"
        return 1
    fi
}

# ── Test 2: /thread sandbox on → clears override ───────────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: /thread sandbox on clears override ===${NC}"

    # Continue from test_1 - same thread should still be active
    local content
    content=$(capture_pane -5)
    if ! is_chat_prompt "$content"; then
        # Re-enter chat if needed
        restart_client
        wait_for_client
        enter_chat
        send_keys "Reply with just: ok" 0.3
        send_enter 0.3
        if ! wait_for_chat_response; then
            assert_fail "No chat response"
            return 1
        fi
        # Set off first so we can test on
        send_keys "/thread sandbox off" 0.3
        send_enter 1
    fi

    # Now set sandbox on
    send_keys "/thread sandbox on" 0.3
    send_enter 1

    content=$(capture_pane -20)
    show_capture "After /thread sandbox on" "$content" 10

    # Verify response confirms sandbox enabled
    if ! echo "$content" | grep -qi "sandbox.*enabled\|sandbox.*on"; then
        assert_fail "No confirmation of sandbox enabled"
        return 1
    fi

    # Check meta file - sandbox_disabled should be absent or not true
    sleep 0.5
    local meta_file
    meta_file=$(latest_meta_file)
    if [[ -z "$meta_file" ]]; then
        assert_fail "No meta file found"
        return 1
    fi

    if meta_no_sandbox_disabled "$meta_file"; then
        assert_pass "/thread sandbox on clears sandbox_disabled from meta file"
        return 0
    else
        echo -e "  Meta content:"
        cat "$meta_file" | sed 's/^/    /'
        assert_fail "sandbox_disabled still set in meta file after 'on'"
        return 1
    fi
}

# ── Test 3: /thread sandbox → shows current state ──────────────────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: /thread sandbox shows current state ===${NC}"

    # Continue from test_2 - sandbox should be on
    local content
    content=$(capture_pane -5)
    if ! is_chat_prompt "$content"; then
        restart_client
        wait_for_client
        enter_chat
        send_keys "Reply with just: ok" 0.3
        send_enter 0.3
        if ! wait_for_chat_response; then
            assert_fail "No chat response"
            return 1
        fi
    fi

    # Query sandbox state - should be "on" (from test_2)
    send_keys "/thread sandbox" 0.3
    send_enter 1

    content=$(capture_pane -20)
    show_capture "After /thread sandbox (query)" "$content" 10

    if echo "$content" | grep -q "sandbox: on"; then
        echo -e "  ${GREEN}Query shows sandbox: on${NC}"
    else
        assert_fail "Expected 'sandbox: on' in query output"
        return 1
    fi

    # Set to off, then query again
    send_keys "/thread sandbox off" 0.3
    send_enter 1
    send_keys "/thread sandbox" 0.3
    send_enter 1

    content=$(capture_pane -20)
    show_capture "After /thread sandbox (query off)" "$content" 10

    if echo "$content" | grep -q "sandbox: off"; then
        assert_pass "/thread sandbox query shows correct state"
        return 0
    else
        assert_fail "Expected 'sandbox: off' in query output after setting off"
        return 1
    fi
}

# ── Test 4: /thread stats shows "sandbox: off" when disabled ────────────────
test_4() {
    echo -e "\n${YELLOW}=== Test 4: /thread stats shows sandbox: off ===${NC}"

    # Continue from test_3 - sandbox should be off
    local content
    content=$(capture_pane -5)
    if ! is_chat_prompt "$content"; then
        restart_client
        wait_for_client
        enter_chat
        send_keys "Reply with just: ok" 0.3
        send_enter 0.3
        if ! wait_for_chat_response; then
            assert_fail "No chat response"
            return 1
        fi
        send_keys "/thread sandbox off" 0.3
        send_enter 1
    fi

    # Run /thread stats
    send_keys "/thread stats" 0.3
    send_enter 1

    content=$(capture_pane -30)
    show_capture "/thread stats output" "$content" 15

    if echo "$content" | grep -q "sandbox: off"; then
        assert_pass "/thread stats shows 'sandbox: off' when disabled"
        return 0
    else
        assert_fail "'sandbox: off' not found in /thread stats output"
        return 1
    fi
}

echo -e "${YELLOW}Thread sandbox integration test: off, on, query, stats${NC}"
run_tests 4
