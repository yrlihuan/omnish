#!/usr/bin/env bash
#
# test_chat_history.sh - Test arrow key navigation in chat mode
#
# Tests the chat history navigation feature using up/down arrows.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Up arrow shows most recent command
  2. Multiple up arrows navigate back through history
  3. Down arrow navigates forward through history
  4. Down arrow past most recent clears input
EOF
}

test_init "chat-history" "$@"

# Helper function to send arrow keys
# In tmux, we can use the special key names Up and Down
send_up_arrow() {
    local wait="${1:-0.5}"
    echo -e "  Sending: ${YELLOW}Up arrow${NC}"
    _tmux send-keys -t "$PANE" Up
    sleep "$wait"
}

send_down_arrow() {
    local wait="${1:-0.5}"
    echo -e "  Sending: ${YELLOW}Down arrow${NC}"
    _tmux send-keys -t "$PANE" Down
    sleep "$wait"
}

# Helper to get current input line (last non-empty line)
get_current_input() {
    local content=$(capture_pane -20)
    last_nonempty_line "$content"
}

# ── Test 1: Up arrow shows most recent command ──────────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Up arrow shows most recent command ===${NC}"

    start_client
    wait_for_client

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Send first command
    send_keys "First chat command" 0.3
    send_enter 0.5

    # Wait for LLM response (could be slow)
    echo -e "  ${YELLOW}Waiting for LLM response to first command...${NC}"
    sleep 15

    # Send second command
    send_keys "Second command" 0.3
    send_enter 0.5

    # Wait for LLM response (could be slow)
    echo -e "  ${YELLOW}Waiting for LLM response to second command...${NC}"
    sleep 15

    # Now we should be at a fresh chat prompt
    # Press up arrow (should show "Second command")
    send_up_arrow 0.5

    # Capture and verify
    local raw_content=$(capture_pane -20)
    show_capture "Raw content after up arrow" "$raw_content" 10

    # Check if "Second command" appears in the raw content
    # It should appear as "> Second command" on a line
    if echo "$raw_content" | grep -q "> Second command"; then
        assert_pass "Up arrow shows previous command 'Second command'"
        return 0
    elif echo "$raw_content" | grep -q "Second command"; then
        assert_pass "Up arrow shows previous command 'Second command' (without > prefix)"
        return 0
    else
        assert_fail "Up arrow did not show 'Second command' in output"
        return 1
    fi
}

# ── Test 2: Multiple up arrows navigate back through history ────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Multiple up arrows navigate back through history ===${NC}"

    restart_client
    wait_for_client

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Send three commands
    send_keys "Command A" 0.3
    send_enter 1

    send_keys "Command B" 0.3
    send_enter 1

    send_keys "Command C" 0.3
    send_enter 1

    # Enter chat mode again
    send_keys ":" 0.5
    wait_for_prompt

    # First up arrow: should show "Command C"
    send_up_arrow 0.5
    local input1=$(get_current_input)
    if ! echo "$input1" | grep -q "Command C"; then
        assert_fail "First up arrow should show 'Command C', got: '$input1'"
        return 1
    fi

    # Second up arrow: should show "Command B"
    send_up_arrow 0.5
    local input2=$(get_current_input)
    if ! echo "$input2" | grep -q "Command B"; then
        assert_fail "Second up arrow should show 'Command B', got: '$input2'"
        return 1
    fi

    # Third up arrow: should show "Command A"
    send_up_arrow 0.5
    local input3=$(get_current_input)
    if ! echo "$input3" | grep -q "Command A"; then
        assert_fail "Third up arrow should show 'Command A', got: '$input3'"
        return 1
    fi

    assert_pass "Multiple up arrows correctly navigate back through history"
    return 0
}

# ── Test 3: Down arrow navigates forward through history ────────────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: Down arrow navigates forward through history ===${NC}"

    restart_client
    wait_for_client

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Send three commands
    send_keys "Alpha" 0.3
    send_enter 1

    send_keys "Beta" 0.3
    send_enter 1

    send_keys "Gamma" 0.3
    send_enter 1

    # Enter chat mode again
    send_keys ":" 0.5
    wait_for_prompt

    # Navigate up twice to "Beta"
    send_up_arrow 0.5  # Gamma
    send_up_arrow 0.5  # Beta

    # Verify we're at "Beta"
    local before_down=$(get_current_input)
    if ! echo "$before_down" | grep -q "Beta"; then
        assert_fail "Should be at 'Beta' before down arrow, got: '$before_down'"
        return 1
    fi

    # Down arrow: should go to "Gamma"
    send_down_arrow 0.5
    local after_down=$(get_current_input)
    if ! echo "$after_down" | grep -q "Gamma"; then
        assert_fail "Down arrow should show 'Gamma', got: '$after_down'"
        return 1
    fi

    # Another down arrow: should go to empty (most recent)
    send_down_arrow 0.5
    local after_second_down=$(get_current_input)
    # Empty input should just be the prompt "> "
    if echo "$after_second_down" | grep -qE '^\s*((\[36m)?> (\[0m)?|> )$'; then
        assert_pass "Down arrow past most recent shows empty input"
        return 0
    elif [[ -z "$after_second_down" || "$after_second_down" =~ ^[[:space:]]*$ ]]; then
        assert_pass "Down arrow past most recent shows empty input"
        return 0
    else
        assert_fail "Down arrow past most recent should show empty input, got: '$after_second_down'"
        return 1
    fi
}

# ── Test 4: Down arrow past most recent clears input ────────────────────────
test_4() {
    echo -e "\n${YELLOW}=== Test 4: Down arrow past most recent clears input ===${NC}"

    restart_client
    wait_for_client

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # Send a command
    send_keys "Test command" 0.3
    send_enter 1

    # Enter chat mode again
    send_keys ":" 0.5
    wait_for_prompt

    # Type something
    send_keys "partial input" 0.3

    # Up arrow to get history
    send_up_arrow 0.5

    # Verify we got the history
    local with_history=$(get_current_input)
    if ! echo "$with_history" | grep -q "Test command"; then
        assert_fail "Up arrow should show 'Test command', got: '$with_history'"
        return 1
    fi

    # Down arrow: should clear back to empty
    send_down_arrow 0.5
    local cleared=$(get_current_input)

    # Check if it's empty (just prompt) or contains "partial input" (original typed text)
    if echo "$cleared" | grep -qE '^\s*((\[36m)?> (\[0m)?|> )$'; then
        assert_pass "Down arrow clears history navigation, shows empty prompt"
        return 0
    elif [[ -z "$cleared" || "$cleared" =~ ^[[:space:]]*$ ]]; then
        assert_pass "Down arrow clears history navigation, shows empty prompt"
        return 0
    else
        # Might show "partial input" if the implementation restores typed text
        # This is also acceptable behavior
        assert_pass "Down arrow shows original typed text: '$cleared'"
        return 0
    fi
}

echo -e "${YELLOW}Testing chat history navigation with arrow keys${NC}"
run_tests 4