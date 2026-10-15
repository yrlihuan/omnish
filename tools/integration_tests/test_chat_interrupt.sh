#!/usr/bin/env bash
#
# test_chat_interrupt.sh - Integration tests for Ctrl-C interrupt in chat mode.
#
# Test cases:
#   1. Late interrupt (during tool execution) → next input echo displayed correctly (#534)
#   2. Early interrupt (before LLM output) → input restored for re-editing (#536)
#   3. Ctrl-C right as a turn ends → the first key typed afterwards is not
#      lost (#696). Repeated, since it is a race with the turn's stdin watcher.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Late interrupt during tool execution, verify next input echo (#534)
  2. Early interrupt before LLM output, verify input restored for editing (#536)
  3. Ctrl-C right as a turn ends, next input keeps its first character (#696)
EOF
}

test_init "chat-interrupt" "$@"

# ── Test 1: Late interrupt - next input echo displayed correctly (#534) ──
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Late interrupt during tool execution (#534) ===${NC}"

    restart_client
    wait_for_client

    # Enter chat mode
    enter_chat

    # Ask LLM to run a long command that we can interrupt
    send_keys "使用bash工具执行: sleep 30" 0.3
    send_enter 0.3

    # Poll for the Bash tool to actually dispatch before interrupting. Fixed
    # sleep silently degrades to "early interrupt during LLM call" when the
    # upstream API backs off on 429 (issue #568), which is Test 2's scenario,
    # not Test 1's. 90s tolerates one upstream 60s retry backoff.
    local waited=0
    local tool_started=false
    while [[ $waited -lt 90 ]]; do
        # Any status icon: while running the header shows a spinner frame,
        # and only turns into "●" once the tool has finished.
        if capture_pane -30 | grep -qE 'Bash\(sleep'; then
            tool_started=true
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done
    if ! $tool_started; then
        show_capture "Tool never dispatched" "$(capture_pane -50)" 25
        assert_fail "Bash(sleep ...) tool did not dispatch within 90s (likely upstream 429)"
        return 1
    fi
    echo -e "  Tool dispatched after ${waited}s, sending Ctrl-C..."
    send_special C-c 2

    # Verify we're back at the chat prompt
    local content
    content=$(capture_pane -20)
    if ! is_chat_prompt "$content"; then
        show_capture "After Ctrl-C" "$content" 10
        assert_fail "Not at chat prompt after Ctrl-C"
        return 1
    fi
    echo -e "  Back at chat prompt after interrupt"

    # Type a test message and send it
    local test_msg="hello world"
    send_keys "$test_msg" 0.3
    send_enter 0.3

    # Wait for LLM response
    if ! wait_for_chat_response; then
        show_capture "After sending message" "$(capture_pane -30)" 15
        assert_fail "No chat response after sending message"
        return 1
    fi

    # Capture a wide window: long LLM responses can push the echo line past
    # the default view (issue #568).
    content=$(capture_pane -200)
    show_capture "After response" "$content" 15

    # Strip ANSI codes
    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    # Check for user input echo line: "> hello world"
    # This must appear as a distinct line, not just inside LLM thinking text.
    if echo "$stripped" | grep -qE '^>?\s*hello world\s*$'; then
        assert_pass "Input echo line displayed correctly"
        return 0
    else
        assert_fail "Input echo line '> hello world' missing from output (#534)"
        return 1
    fi
}

# ── Test 2: Early interrupt - input restored for re-editing (#536) ───────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Early interrupt before LLM output (#536) ===${NC}"

    restart_client
    wait_for_client

    # Enter chat mode
    enter_chat

    # Type a message and send it, then Ctrl-C as fast as possible
    local test_msg="使用bash工具执行: sleep 30"
    send_keys "$test_msg" 0.3
    send_enter 0.1

    # Interrupt immediately (before LLM returns first output)
    sleep 0.2
    echo -e "  Sending Ctrl-C to cancel early..."
    send_special C-c 1

    # Capture the pane - the input should be restored in the editor
    local content
    content=$(capture_pane -10)
    show_capture "After early Ctrl-C" "$content" 5

    # Strip ANSI codes
    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    # The last non-empty line should show "> " with the original input restored
    local last_line
    last_line=$(last_nonempty_line "$stripped")

    if echo "$last_line" | grep -qF "$test_msg"; then
        assert_pass "Input restored for re-editing: '$last_line'"
    else
        assert_fail "Input not restored. Expected '$test_msg' in: '$last_line'"
        return 1
    fi

    assert_pass "No 'User interrupted' message generated"
    return 0
}

# ── Test 3: Ctrl-C at the end of a turn keeps the next key (#696) ──────
# The turn's stdin watcher used to stay blocked in read() when the input loop
# took the bytes of a key arriving right at the end of the turn, and then
# swallowed the next key typed, however much later ("hello world" became
# "ello world"). Hitting the window needs the key at the very moment the
# reply is drawn, so poll fast for it and repeat a few rounds.
test_3() {
    echo -e "\n${YELLOW}=== Test 3: Ctrl-C at the end of a turn keeps the next key (#696) ===${NC}"
    local rounds=4 round
    restart_client
    wait_for_client
    enter_chat
    for round in $(seq 1 $rounds); do
        local word="DONE$round"
        send_keys "Use the bash tool to run: sleep 1. Then reply with only the word $word." 0.3
        send_enter 0.1
        local seen=false i
        for i in $(seq 1 3000); do   # up to ~60s
            if capture_pane -40 | grep -qE "^● $word\b"; then seen=true; break; fi
            sleep 0.02
        done
        if ! $seen; then
            show_capture "Round $round" "$(capture_pane -40)" 20
            assert_fail "Round $round: reply '$word' never appeared"
            return 1
        fi
        send_special C-c 2
        local msg="hello world $round"
        send_keys "$msg" 0.3
        send_enter 0.3
        if ! wait_for_chat_response; then
            assert_fail "Round $round: no response to '$msg'"
            return 1
        fi
        if ! capture_pane -200 | sed 's/\x1b\[[0-9;]*m//g' | grep -qE "^>?\s*$msg\s*$"; then
            show_capture "Round $round" "$(capture_pane -40)" 20
            assert_fail "Round $round: echo of '$msg' lost a character (#696)"
            return 1
        fi
        echo -e "  ${GREEN}Round $round: '$msg' echoed intact${NC}"
    done
    send_special Escape 0.5
    assert_pass "Next input intact after Ctrl-C at turn end ($rounds rounds)"
}

run_tests 3
