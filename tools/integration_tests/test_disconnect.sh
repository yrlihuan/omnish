#!/usr/bin/env bash
#
# test_disconnect.sh - Integration test for daemon disconnect handling (#494, #495)
#
# Test cases:
#   1. Disconnect during tool execution — error feedback and prompt recovery
#   2. Disconnect + reconnect — chat resumes normally
#   3. Disconnect during tool execution → reconnect → chat resumes

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Daemon disconnect during tool execution shows error and recovers prompt (#494)
  2. Disconnect + auto reconnect, then chat works normally
  3. Disconnect during tool execution, reconnect, then chat works normally
EOF
}

test_init "disconnect" "$@"

# ── Test 1: Disconnect during tool execution ──────────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Disconnect during tool execution (#494) ===${NC}"

    restart_client
    wait_for_client
    enter_chat

    send_keys "/test disconnect 15 60" 0.3
    send_enter 0.3
    wait_for_prompt 1

    send_keys "运行 sleep 15" 0.3
    send_enter 0.3

    # Wait for spinner — confirms tool started
    local waited=0
    local content=""
    while [[ $waited -lt 30 ]]; do
        content=$(capture_pane -20)
        if echo "$content" | grep -qE '(⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏).*Bash'; then
            echo -e "  Spinner detected — tool is running"
            break
        fi
        sleep 0.5
        waited=$((waited + 1))
    done

    if [[ $waited -ge 30 ]]; then
        show_capture "Pane (no spinner)" "$content" 20
        assert_fail "Tool did not start within 15s"
        return 1
    fi

    if ! wait_for_chat_response 60; then
        show_capture "After disconnect" "$(capture_pane -30)" 30
        assert_fail "Chat prompt did not return after disconnect"
        return 1
    fi

    content=$(capture_pane -30)
    show_capture "After disconnect" "$content" 30

    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    if echo "$stripped" | grep -qiE 'connection lost|[Ff]ailed to send'; then
        assert_pass "Disconnect error message displayed"
    else
        assert_fail "No disconnect error message found"
        return 1
    fi

    return 0
}

# ── Test 2: Disconnect + reconnect + chat resumes ────────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Disconnect + reconnect, chat resumes ===${NC}"

    restart_client
    wait_for_client
    enter_chat

    # Disconnect in 2s, no reconnect suppression → auto reconnect
    send_keys "/test disconnect 2" 0.3
    send_enter 0.3
    wait_for_prompt 1

    # Wait for disconnect + reconnect (~5s)
    echo -e "  Waiting for disconnect + reconnect..."
    sleep 5

    # Send a simple query to verify chat works after reconnection
    send_keys "说hello" 0.3
    send_enter 0.3

    if ! wait_for_chat_response 30; then
        show_capture "After reconnect query" "$(capture_pane -30)" 30
        assert_fail "No response after reconnect"
        return 1
    fi

    local content
    content=$(capture_pane -30)
    show_capture "After reconnect query" "$content" 20

    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    # Verify LLM responded (look for the ● bullet or any text after the query)
    if echo "$stripped" | grep -q '●'; then
        assert_pass "Chat works after disconnect + reconnect"
    else
        assert_fail "No LLM response after reconnect"
        return 1
    fi

    return 0
}

# ── Test 3: Disconnect during tool → reconnect → chat resumes ────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: Disconnect during tool, reconnect, chat resumes ===${NC}"

    restart_client
    wait_for_client
    enter_chat

    # Disconnect in 15s, no reconnect suppression
    send_keys "/test disconnect 15" 0.3
    send_enter 0.3
    wait_for_prompt 1

    # Run a 15s tool — disconnect fires during execution
    send_keys "运行 sleep 15" 0.3
    send_enter 0.3

    # Wait for spinner
    local waited=0
    local content=""
    while [[ $waited -lt 30 ]]; do
        content=$(capture_pane -20)
        if echo "$content" | grep -qE '(⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏).*Bash'; then
            echo -e "  Spinner detected — tool is running"
            break
        fi
        sleep 0.5
        waited=$((waited + 1))
    done

    if [[ $waited -ge 30 ]]; then
        show_capture "Pane (no spinner)" "$content" 20
        assert_fail "Tool did not start within 15s"
        return 1
    fi

    # Wait for error + prompt recovery
    if ! wait_for_chat_response 60; then
        show_capture "After disconnect" "$(capture_pane -30)" 30
        assert_fail "Chat prompt did not return after tool disconnect"
        return 1
    fi

    content=$(capture_pane -30)
    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    if echo "$stripped" | grep -qiE 'connection lost|[Ff]ailed to send'; then
        echo -e "  Disconnect error confirmed"
    else
        echo -e "  ${YELLOW}Note: no explicit error message (may have been fast)${NC}"
    fi

    # Wait for auto reconnect (~5s)
    echo -e "  Waiting for reconnect..."
    sleep 5

    # Send a new query to verify recovery
    send_keys "说OK" 0.3
    send_enter 0.3

    if ! wait_for_chat_response 30; then
        show_capture "After recovery query" "$(capture_pane -30)" 30
        assert_fail "No response after recovery"
        return 1
    fi

    content=$(capture_pane -30)
    show_capture "After recovery query" "$content" 20

    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    if echo "$stripped" | grep -q '●'; then
        assert_pass "Chat works after tool disconnect + reconnect"
    else
        assert_fail "No LLM response after recovery"
        return 1
    fi

    return 0
}

echo -e "${YELLOW}Daemon disconnect integration test (#494, #495)${NC}"
run_tests 3
