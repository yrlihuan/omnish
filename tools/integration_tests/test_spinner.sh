#!/usr/bin/env bash
#
# test_spinner.sh - Integration test for tool status spinner animation (#478)
#
# Test cases:
#   1. Running tool shows animated spinner that changes over time

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Running tool shows animated spinner that changes between captures (#478)
EOF
}

test_init "spinner" "$@"

# ── Test 1: Spinner animation during tool execution (#478) ──────────────
# When a tool is running, the status icon should be an animated spinner
# (braille characters cycling). We verify by capturing the pane at two
# different times and checking that the icon character changes.
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Spinner animation during tool execution (#478) ===${NC}"

    restart_client
    wait_for_client

    enter_chat

    # Ask LLM to run sleep 10
    send_keys "运行 sleep 10" 0.3
    send_enter 0.3

    # Wait for the tool status line to appear (Bash tool header with spinner)
    local waited=0
    local content=""
    while [[ $waited -lt 30 ]]; do
        content=$(capture_pane -20)
        if echo "$content" | grep -qE '(⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏).*Bash'; then
            break
        fi
        sleep 0.5
        waited=$((waited + 1))
    done

    if [[ $waited -ge 30 ]]; then
        show_capture "Pane content (no spinner found)" "$content" 20
        assert_fail "No spinner character found in tool header within 15s"
        return 1
    fi

    echo -e "  Spinner detected, capturing two frames..."

    # Capture first frame
    local frame1
    frame1=$(capture_pane -20 | grep -oE '[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]' | head -1)
    echo -e "  Frame 1: '$frame1'"

    # Wait for spinner to advance (> 200ms interval, use 1s to be safe)
    sleep 1

    # Capture second frame
    local frame2
    frame2=$(capture_pane -20 | grep -oE '[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]' | head -1)
    echo -e "  Frame 2: '$frame2'"

    if [[ -n "$frame1" && -n "$frame2" && "$frame1" != "$frame2" ]]; then
        assert_pass "Spinner animated: '$frame1' → '$frame2'"
    elif [[ -n "$frame1" && -n "$frame2" ]]; then
        # Same frame captured — try once more with longer wait
        sleep 1.5
        local frame3
        frame3=$(capture_pane -20 | grep -oE '[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]' | head -1)
        echo -e "  Frame 3: '$frame3'"
        if [[ "$frame1" != "$frame3" || "$frame2" != "$frame3" ]]; then
            assert_pass "Spinner animated (3 captures): '$frame1' → '$frame2' → '$frame3'"
        else
            assert_fail "Spinner not animating: all frames identical ('$frame1')"
            return 1
        fi
    else
        assert_fail "Could not capture spinner frames (frame1='$frame1', frame2='$frame2')"
        return 1
    fi

    # Wait for sleep to finish and LLM to respond
    if ! wait_for_chat_response 30; then
        show_capture "After sleep" "$(capture_pane -20)" 20
        assert_fail "No chat response after sleep 10"
        return 1
    fi

    # After completion, spinner should be replaced with a static icon (●)
    content=$(capture_pane -30)
    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    # Check that the Bash tool header now has ● (success) instead of spinner
    if echo "$stripped" | grep -qE '● Bash\('; then
        assert_pass "Tool completed with static ● icon"
    else
        show_capture "Final state" "$content" 20
        # Not a hard failure — the tool section may have been cleared
        echo -e "  ${YELLOW}Note: Could not verify final static icon (section may be cleared)${NC}"
    fi

    return 0
}

echo -e "${YELLOW}Spinner animation integration test (#478)${NC}"
run_tests 1
