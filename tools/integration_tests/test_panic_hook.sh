#!/usr/bin/env bash
#
# test_panic_hook.sh - Integration test for the client panic hook.
#
# A client panic used to leave nothing behind: the message went to stderr,
# i.e. the tmux pane, which closes when the client exits. The panic hook
# writes the message, a backtrace and the recent event log to client.log,
# and for a main thread panic (which ends the client) restores the terminal
# and sends SessionEnd.
#
# The client runs under a wrapper shell so the pane survives its exit and we
# can check the exit code and the terminal mode afterwards.
#
# Test cases:
#   1. Panic in a tokio task: logged as non-fatal, the client keeps running
#   2. Panic on the main thread: logged as fatal, terminal restored,
#      SessionEnd reaches the daemon

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Panic in a tokio task: logged, client keeps running
  2. Panic on the main thread: logged, terminal restored, SessionEnd sent
EOF
}

test_init "panic-hook" "$@"

OMNISH_DIR="${OMNISH_HOME:-$HOME/.omnish}"
CLIENT_LOG="$OMNISH_DIR/client.log"

# Start the client under bash; after it exits, print its exit code and
# whether the tty is back in canonical mode with echo.
start_wrapped_client() {
    _tmux kill-session -t "$SESSION" 2>/dev/null || true
    _wait_for_server_gone
    _tmux new -d -s "$SESSION" -n test \
        "OMNISH_LANG=$OMNISH_LANG_DEFAULT $CLIENT; echo \"CLIENT_EXIT=\$?\"; stty -a | tr ' ' '\n' | grep -xE -- '-?(icanon|echo)' | tr '\n' ' '; echo; exec sleep 300"
}

# Lines appended to client.log since byte offset $1.
client_log_since() {
    tail -c +"$(( $1 + 1 ))" "$CLIENT_LOG" 2>/dev/null
}

client_log_size() {
    stat -c %s "$CLIENT_LOG" 2>/dev/null || echo 0
}

test_1() {
    echo -e "\n${YELLOW}=== Test 1: task panic is logged, client keeps running ===${NC}"
    start_wrapped_client
    wait_for_client 2
    local offset; offset=$(client_log_size)
    enter_chat
    send_keys "/test panic task" 0.3
    send_enter 1.5
    local log; log=$(client_log_since "$offset")
    if ! echo "$log" | grep -q "PANIC thread=.* fatal=false"; then
        echo "$log" | tail -20
        assert_fail "No non-fatal PANIC report in client.log"
        return 1
    fi
    if ! echo "$log" | grep -q "/test panic task"; then
        assert_fail "Panic message missing from the report"
        return 1
    fi
    # Still alive: chat answers another command.
    send_keys "/test" 0.3
    send_enter 1
    local content; content=$(capture_pane -40)
    if echo "$content" | grep -q "CLIENT_EXIT="; then
        show_capture "Pane" "$content" 15
        assert_fail "Client exited after a task panic"
        return 1
    fi
    if ! echo "$content" | grep -q "/test panic \[task\]"; then
        show_capture "Pane" "$content" 15
        assert_fail "Client did not answer /test after the task panic"
        return 1
    fi
    assert_pass "Task panic logged as non-fatal, client still running"
}

test_2() {
    echo -e "\n${YELLOW}=== Test 2: main thread panic is logged, terminal restored, SessionEnd sent ===${NC}"
    start_wrapped_client
    wait_for_client 2
    local content sid
    content=$(capture_pane -40)
    sid=$(echo "$content" | grep -oE 'session: [0-9a-f]{8}' | head -1 | awk '{print $2}')
    if [[ -z "$sid" ]]; then
        show_capture "Pane" "$content" 15
        assert_fail "Could not read the session id"
        return 1
    fi
    local offset; offset=$(client_log_size)
    enter_chat
    send_keys "/test panic" 0.3
    send_enter 2
    content=$(capture_pane -40)
    if ! echo "$content" | grep -q "CLIENT_EXIT=101"; then
        show_capture "Pane" "$content" 15
        assert_fail "Client did not exit with the panic code 101"
        return 1
    fi
    if ! echo "$content" | grep -qE "^icanon echo"; then
        show_capture "Pane" "$content" 15
        assert_fail "Terminal not restored (expected icanon echo)"
        return 1
    fi
    local log; log=$(client_log_since "$offset")
    if ! echo "$log" | grep -q "PANIC thread=main fatal=true"; then
        echo "$log" | tail -20
        assert_fail "No fatal PANIC report in client.log"
        return 1
    fi
    if ! echo "$log" | grep -q "^backtrace:" || ! echo "$log" | grep -q "^recent events ("; then
        echo "$log" | tail -20
        assert_fail "Report lacks the backtrace or the recent events"
        return 1
    fi
    local meta
    meta=$(ls -d "$OMNISH_DIR"/sessions/*_"$sid" 2>/dev/null | head -1)/meta.json
    local ended=""
    for _ in 1 2 3 4 5 6; do
        ended=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('ended_at') or '')" "$meta" 2>/dev/null)
        [[ -n "$ended" ]] && break
        sleep 0.5
    done
    if [[ -z "$ended" ]]; then
        assert_fail "Session $sid has no ended_at: SessionEnd not received"
        return 1
    fi
    assert_pass "Main thread panic logged, terminal restored, session $sid ended"
}

run_tests 2
