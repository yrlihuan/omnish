#!/usr/bin/env bash
#
# verify_issue_288.sh - Bash tool display: no truncation in browse mode
#
# Reproduces: prompt LLM to run an echo command outputting 40+ lines,
# then press Ctrl-O to enter browse mode and verify the bash(...)
# header is NOT truncated (no "…" marker).
#
# On the normal terminal display, long bash(...) headers are truncated
# to terminal width with "…". In browse mode (Ctrl-O), the full header
# should be rendered without our "…" truncation — the terminal clips
# at the right edge instead.
#
# Test cases:
#   1. Long echo command — normal display truncated, browse mode not truncated

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Run echo with 40+ lines, verify browse mode bash(...) header has no "…"
EOF
}

test_init "issue288" "$@"

test_1() {
    echo -e "\n${YELLOW}=== Test 1: Long echo — browse mode shows un-truncated header ===${NC}"

    restart_client
    wait_for_client

    # Use narrow width so the bash(...) header is definitely truncated on screen
    _tmux resize-window -t "$SESSION" -x 80 -y 30

    send_keys ":" 0.5
    wait_for_prompt

    # Ask LLM to run a long echo that produces 45 lines
    local cmd='Run this exact bash command: echo -e "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nline11\nline12\nline13\nline14\nline15\nline16\nline17\nline18\nline19\nline20\nline21\nline22\nline23\nline24\nline25\nline26\nline27\nline28\nline29\nline30\nline31\nline32\nline33\nline34\nline35\nline36\nline37\nline38\nline39\nline40\nline41\nline42\nline43\nline44\nline45"'
    send_keys "$cmd" 0.3
    send_enter 0.3

    if ! wait_for_chat_response 45; then
        show_capture "After send" "$(capture_pane -50)" 15
        assert_fail "No chat prompt after sending command"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    # Check the normal screen — header should be truncated with …
    local content
    content=$(capture_pane -200)

    local screen_header
    screen_header=$(echo "$content" | grep '● bash(' | head -1)
    echo -e "  Screen header: ${screen_header}"

    if ! echo "$content" | grep -q '● bash('; then
        assert_fail "No '● bash(' found in output"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    # Normal display should be truncated (contains …)
    if ! echo "$screen_header" | grep -q '…'; then
        echo -e "  ${YELLOW}Note: screen header not truncated (terminal may be wide enough)${NC}"
    else
        echo -e "  ${GREEN}Screen header is truncated as expected${NC}"
    fi

    # Enter browse mode with Ctrl-O
    send_special C-o 1.0

    # Capture the alternate screen
    local browse_content
    browse_content=$(_tmux capture-pane -p -J -t "$PANE" -S -200)
    show_capture "Browse mode" "$browse_content" 15

    local browse_header
    browse_header=$(echo "$browse_content" | grep '● bash(' | head -1)
    echo -e "  Browse header: ${browse_header}"

    if [[ -z "$browse_header" ]]; then
        assert_fail "No '● bash(' found in browse mode"
        send_keys "q" 0.5
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    # Browse mode header should NOT have our "…" truncation marker
    if echo "$browse_header" | grep -q '…'; then
        assert_fail "Browse mode header is truncated (contains …)"
        send_keys "q" 0.5
        send_special Escape 0.5
        sleep 1.5
        return 1
    else
        assert_pass "Browse mode header is not truncated (no … marker)"
        send_keys "q" 0.5
        send_special Escape 0.5
        sleep 1.5
        return 0
    fi
}

echo -e "${YELLOW}Issue #288: bash tool display — full command in browse mode${NC}"
run_tests 1
