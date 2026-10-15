#!/usr/bin/env bash
#
# test_auto_compact.sh - Integration test for auto compaction (#691).
#
# Needs a daemon built with auto compaction whose chat backend sets
# context_window. The test lowers [llm] auto_compact_percent in daemon.toml
# (hot reloaded) to 1 so a ~7K token tool result crosses the threshold, and
# restores the file on exit.
#
# Test cases:
#   1. Before a turn: the thread compacts before the new message is answered
#   2. Mid-turn: the thread compacts between tool results and the next LLM
#      call, and the model still finishes the task
#   3. /resume shows the auto compaction as "/compact (auto)"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Auto compaction before a turn
  2. Auto compaction in the middle of a turn
  3. /resume shows "/compact (auto)"
EOF
}

test_init "auto-compact" "$@"

OMNISH_DIR="${OMNISH_HOME:-$HOME/.omnish}"
DAEMON_TOML="$OMNISH_DIR/daemon.toml"
TOML_BACKUP="$(mktemp /tmp/omnish-test-daemon-toml.XXXXXX)"
cp "$DAEMON_TOML" "$TOML_BACKUP"
trap 'cp "$TOML_BACKUP" "$DAEMON_TOML"; rm -f "$TOML_BACKUP"; cleanup' EXIT

# ~29K chars of tool output, about 7K tokens: over the 1% threshold of a
# window up to ~1.4M tokens, and big enough to pass the "messages are at
# least half the threshold" guard.
BIG_CMD="seq 100000 104199"
# Models like to shorten a long-output command (e.g. append `| tail -n 5`),
# which keeps the context small and nothing compacts.
VERBATIM="exactly as written, without pipes, head/tail or any other change, since I need its full output"

now_utc() {
    date -u +%Y-%m-%dT%H:%M:%S
}

# set_auto_compact_percent <n> - rewrite the key under [llm] and wait for the
# daemon's config watcher to pick it up.
set_auto_compact_percent() {
    local n="$1"
    if grep -qE "^auto_compact_percent = $n\$" "$DAEMON_TOML"; then
        return 0  # unchanged: the daemon sees no change and logs no reload
    fi
    local since
    since=$(now_utc)
    python3 - "$DAEMON_TOML" "$n" <<'PY'
import re, sys
path, n = sys.argv[1], sys.argv[2]
text = open(path).read()
text = re.sub(r'(?m)^auto_compact_percent\s*=.*\n', '', text)
if re.search(r'(?m)^\[llm\]\s*$', text):
    text = re.sub(r'(?m)^\[llm\]\s*$', f'[llm]\nauto_compact_percent = {n}', text, count=1)
else:
    text += f'\n[llm]\nauto_compact_percent = {n}\n'
open(path, 'w').write(text)
PY
    local deadline=$(($(date +%s) + 15))
    while (( $(date +%s) < deadline )); do
        daemon_log_since "$since" 'config section changed: Llm' >/dev/null && return 0
        sleep 0.3
    done
    echo "  (no config reload seen in the daemon log, continuing)"
}

# ── Test 1: compaction before a turn ─────────────────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: auto compaction before a turn ===${NC}"
    set_auto_compact_percent 0

    restart_client
    wait_for_client
    enter_chat
    send_keys "Use the bash tool to run this command $VERBATIM: $BIG_CMD. Then reply with only the word OK." 0.3
    send_enter 1
    if ! wait_for_chat_response; then
        assert_fail "No chat response for the seed turn"
        return 1
    fi
    if capture_pane -80 | grep -q 'compacting\.\.\.'; then
        assert_fail "Compacted while auto compaction was disabled"
        return 1
    fi

    set_auto_compact_percent 1
    send_keys "Reply with only the word TWO." 0.3
    send_enter 1
    if ! wait_for_chat_response 300; then
        show_capture "Second turn" "$(capture_pane -40)" 30
        assert_fail "No chat response after auto compaction"
        return 1
    fi
    local content
    content=$(capture_pane -40)
    show_capture "Second turn" "$content" 20
    if ! echo "$content" | grep -qE 'Context at [0-9]+% of .* tokens, compacting\.\.\.'; then
        assert_fail "No 'compacting...' note before the second turn"
        return 1
    fi
    if ! echo "$content" | grep -q 'Context compacted: '; then
        assert_fail "No 'Context compacted' note"
        return 1
    fi
    if ! echo "$content" | grep -E '^\s*●' | tail -1 | grep -qi 'TWO'; then
        assert_fail "The question asked with the compaction was not answered"
        return 1
    fi
    assert_pass "Compacted before the turn and answered the new message"
}

# ── Test 2: compaction in the middle of a turn ───────────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: auto compaction in the middle of a turn ===${NC}"
    set_auto_compact_percent 1

    restart_client
    wait_for_client
    enter_chat
    local since
    since=$(now_utc)
    send_keys "Use the bash tool to run this command $VERBATIM: $BIG_CMD. After that, use the bash tool again to run: echo marker-\$((6*7)). Then reply with only the output of the second command." 0.3
    send_enter 1
    if ! wait_for_chat_response 300; then
        show_capture "Mid-turn" "$(capture_pane -60)" 40
        assert_fail "No chat response"
        return 1
    fi
    local content
    content=$(capture_pane -80)
    show_capture "Mid-turn" "$content" 30
    if ! echo "$content" | grep -qF "Bash($BIG_CMD)"; then
        assert_fail "The model changed the command (expected 'Bash($BIG_CMD)'), so the context stayed small"
        return 1
    fi
    if ! echo "$content" | grep -qE 'Context at [0-9]+% of .* tokens, compacting\.\.\.'; then
        assert_fail "No 'compacting...' note during the turn"
        return 1
    fi
    if ! daemon_log_since "$since" 'Auto compaction \(AutoMidTurn\)' >/dev/null; then
        assert_fail "Daemon log has no mid-turn auto compaction"
        return 1
    fi
    if ! echo "$content" | grep -E '^\s*●' | tail -1 | grep -q 'marker-42'; then
        assert_fail "The task did not continue after the compaction"
        return 1
    fi
    assert_pass "Compacted mid-turn and the model finished the task"
}

# ── Test 3: /resume label ────────────────────────────────────────────────
# Resumes the most recent thread, which test 1 or 2 compacted (run either
# first when running this case alone).
test_3() {
    echo -e "\n${YELLOW}=== Test 3: /resume shows /compact (auto) ===${NC}"
    restart_client
    wait_for_client
    enter_chat
    send_keys "/resume 1" 0.3
    send_enter 3
    local content
    content=$(capture_pane -200)
    show_capture "/resume 1" "$content" 30
    if echo "$content" | grep -qF '/compact (auto)'; then
        assert_pass "Resumed history labels the auto compaction"
    else
        assert_fail "Resumed history has no '/compact (auto)' entry"
        return 1
    fi
}

run_tests 3
