#!/usr/bin/env bash
#
# verify_issue_697.sh - Integration test for issue #697:
#   "单独按ESC后, readline报告序列[13337~被当作命令输入"
#
# The client asks bash for its readline state by writing \x1b[13337~ into the
# pty when a completion response arrives. A lone ESC typed just before is
# held by readline as a Meta prefix, so the two merged into ESC ESC [13337~
# and "[13337~" landed on the command line ("[13337~: command not found").
# The client now skips the trigger while the forwarded input is inside an
# escape sequence.
#
# The race needs the ESC between the new prompt (which sends a completion
# request) and that request's response, so leave chat with ESC and press a
# second ESC 0.1s later, then Enter. Repeated a few rounds.
#
# Test cases:
#   1. ESC right after a new prompt, then Enter: no "[13337~" typed

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. ESC right after a new prompt, then Enter: no "[13337~" typed (#697)
EOF
}

test_init "issue-697" "$@"

test_1() {
    echo -e "\n${YELLOW}=== Test 1: lone ESC is not merged with the readline trigger (#697) ===${NC}"
    local rounds=6 round
    for round in $(seq 1 $rounds); do
        restart_client
        wait_for_client
        enter_chat
        # First ESC leaves chat (new prompt + completion request), the second
        # reaches bash as a lone ESC before the completion response.
        _tmux send-keys -t "$PANE" Escape
        sleep 0.1
        _tmux send-keys -t "$PANE" Escape
        sleep 3
        send_enter 1
        local content
        content=$(capture_pane -50)
        if echo "$content" | grep -q '13337'; then
            show_capture "Round $round" "$content" 10
            assert_fail "Round $round: readline trigger leaked onto the command line"
            return 1
        fi
    done
    assert_pass "No readline trigger leaked after a lone ESC ($rounds rounds)"
}

run_tests 1
