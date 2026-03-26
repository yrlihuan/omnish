#!/usr/bin/env bash
#
# verify_issue_286.sh - Chat output from previous queries should not disappear
#
# Reproduces: enter chat, send multiple queries, verify all outputs remain.
# Bug: the second response replaces the scroll_view entirely, losing the first.
#
# Test cases:
#   1. Two simple Q&A rounds — both user inputs remain visible
#   2. Three bash tool calls with 40-line output each — all results preserved,
#      prompt near screen bottom
#   3. Three short bash commands — all three "● bash(" visible on screen

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Two Q&A in one session — both user inputs visible after second response
  2. Three bash runs (40 lines each) — all outputs preserved, prompt near bottom
  3. Three short bash runs — all three "● bash(" visible on screen
EOF
}

test_init "issue286" "$@"

test_1() {
    echo -e "\n${YELLOW}=== Test 1: Two Q&A — both outputs preserved ===${NC}"

    restart_client
    wait_for_client

    # Enter chat mode
    send_keys ":" 0.5
    wait_for_prompt

    # First question
    echo -e "  ${YELLOW}--- Q1 ---${NC}"
    send_keys "What is 2+2? Reply with just the number." 0.3
    send_enter 0.3
    if ! wait_for_chat_response; then
        show_capture "After Q1" "$(capture_pane -50)" 10
        assert_fail "No chat prompt after Q1"
        return 1
    fi

    local after_q1=$(capture_pane -50)
    show_capture "After Q1 response" "$after_q1" 10

    # Verify Q1 user input is shown
    if ! echo "$after_q1" | grep -q "2+2"; then
        assert_fail "Q1 user input not visible after Q1"
        return 1
    fi

    # Second question
    echo -e "  ${YELLOW}--- Q2 ---${NC}"
    send_keys "What is 3+3? Reply with just the number." 0.3
    send_enter 0.3
    if ! wait_for_chat_response; then
        show_capture "After Q2" "$(capture_pane -50)" 10
        assert_fail "No chat prompt after Q2"
        return 1
    fi

    local after_q2=$(capture_pane -100)
    show_capture "After Q2 response" "$after_q2" 20

    # Both user inputs should still be visible in the pane history
    local has_q1 has_q2
    has_q1=$(echo "$after_q2" | grep -c "2+2") || true
    has_q2=$(echo "$after_q2" | grep -c "3+3") || true

    if [[ $has_q1 -gt 0 && $has_q2 -gt 0 ]]; then
        assert_pass "Both Q1 and Q2 content visible after second response"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "Q1 visible=$has_q1, Q2 visible=$has_q2 — first output lost"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

test_2() {
    echo -e "\n${YELLOW}=== Test 2: Three bash tool calls (40 lines each) ===${NC}"

    restart_client
    wait_for_client

    send_keys ":" 0.5
    wait_for_prompt

    local cmd="Run this bash command and show the output: for i in {1..40}; do echo \$i; done"

    for round in 1 2 3; do
        echo -e "  ${YELLOW}--- Round $round ---${NC}"
        send_keys "$cmd" 0.3
        send_enter 0.3
        if ! wait_for_chat_response; then
            show_capture "After round $round" "$(capture_pane -50)" 15
            assert_fail "No chat prompt after round $round"
            send_special Escape 0.5
            sleep 1.5
            return 1
        fi
    done

    # Capture a large history window
    local content=$(capture_pane -500)
    show_capture "After 3 rounds" "$content" 30

    # Count separator lines (────) — expect at least 3 (one per response)
    local sep_count
    sep_count=$(echo "$content" | grep -c '────') || true
    echo -e "  Separator lines: $sep_count"

    if [[ $sep_count -lt 3 ]]; then
        assert_fail "Expected at least 3 separator lines, got $sep_count — earlier outputs lost"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    # Check that the ">" prompt is in the last 5 lines of visible content
    local rows
    rows=$(_tmux display-message -p -t "$PANE" '#{pane_height}')
    # Capture only the visible screen (no scrollback)
    local visible=$(_tmux capture-pane -p -J -t "$PANE")
    show_capture "Visible screen" "$visible" 10

    local prompt_line
    prompt_line=$(echo "$visible" | grep -nE '^\s*> \s*$' | tail -1 | cut -d: -f1)
    echo -e "  Prompt '> ' at visible line: ${prompt_line:-not found}, pane height: $rows"

    if [[ -n "$prompt_line" && $prompt_line -gt $((rows / 2)) ]]; then
        assert_pass "Three bash outputs preserved, prompt near bottom (line $prompt_line/$rows)"
        send_special Escape 0.5
        sleep 1.5
        return 0
    elif [[ -n "$prompt_line" ]]; then
        assert_fail "Prompt at line $prompt_line/$rows — expected in lower half"
        send_special Escape 0.5
        sleep 1.5
        return 1
    else
        assert_fail "Prompt '> ' not found on visible screen"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

test_3() {
    echo -e "\n${YELLOW}=== Test 3: Three short bash — all outputs visible on screen ===${NC}"

    restart_client
    wait_for_client

    # Resize window + pane to 60 rows so all 3 rounds fit on screen
    _tmux resize-window -t "$SESSION" -y 60

    send_keys ":" 0.5
    wait_for_prompt

    local cmd="Run: echo hello"

    for round in 1 2 3; do
        echo -e "  ${YELLOW}--- Round $round ---${NC}"
        send_keys "$cmd" 0.3
        send_enter 0.3
        if ! wait_for_chat_response; then
            show_capture "After round $round" "$(capture_pane -50)" 15
            assert_fail "No chat prompt after round $round"
            send_special Escape 0.5
            sleep 1.5
            return 1
        fi
    done

    # Capture only the visible screen (no scrollback)
    local visible=$(_tmux capture-pane -p -J -t "$PANE")
    show_capture "Visible screen after 3 short rounds" "$visible" 30

    # Count "● Bash(" lines on the visible screen (case-insensitive for compatibility)
    local bash_count
    bash_count=$(echo "$visible" | grep -ic '● bash(' || true)
    echo -e "  '● Bash(' lines on screen: $bash_count"

    if [[ $bash_count -ge 3 ]]; then
        assert_pass "All three '● Bash(' visible on screen ($bash_count found)"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "Expected at least 3 '● Bash(' on screen, got $bash_count"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

echo -e "${YELLOW}Issue #286: chat output from previous queries should not disappear${NC}"
run_tests 3
