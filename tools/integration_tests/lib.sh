#!/usr/bin/env bash
#
# lib.sh - Shared library for omnish integration tests
#
# Usage in test scripts:
#   SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
#   source "$SCRIPT_DIR/lib.sh"
#   test_init "test-name" "$@"
#
# Provides:
#   - Tmux session management (start_client, restart_client, cleanup)
#   - Key sending (send_keys, send_enter, send_special)
#   - Pane capture and assertion helpers
#   - Test runner with pass/fail tracking and summary
#   - Thread cleanup: snapshots thread count before tests, deletes new ones after
#   - Common CLI flags: -w (wait), -t N (test selection), -h (help)
#
# Test scripts define functions named test_1, test_2, etc. and call run_tests.

set -uo pipefail

# ── Colors ───────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# ── Project paths ────────────────────────────────────────────────────────
# SCRIPT_DIR must be set by the caller before sourcing this file.
PROJECT_ROOT="$(dirname "$(dirname "${SCRIPT_DIR:?SCRIPT_DIR must be set before sourcing lib.sh}")")"
CLIENT="$PROJECT_ROOT/target/release/omnish-client"

# ── Tmux config (override default shell to avoid running installed omnish) ──
TMUX_CONF="$(mktemp /tmp/omnish-test-tmux.XXXXXX.conf)"
echo "set -g default-shell /bin/bash" > "$TMUX_CONF"

# Shorthand: all tmux calls go through this to ensure correct config + socket.
_tmux() {
    tmux -f "$TMUX_CONF" -S "$SOCKET" "$@"
}

# ── Tmux socket / session ───────────────────────────────────────────────
SOCKET_DIR="${CLAUDE_TMUX_SOCKET_DIR:-/tmp/claude-tmux-sockets}"
SOCKET=""   # set by test_init
SESSION=""  # set by test_init
PANE=""     # convenience: "$SESSION:0.0"

# ── Internal state ──────────────────────────────────────────────────────
_TEST_PASSED=0
_TEST_TOTAL=0
_WAIT_FOR_USER=false
_WAIT_CLIENT_STARTED=false  # true if -w already started the client
_TEST_CASES="all"
_TEST_MAX=0  # set by caller via run_tests
_THREADS_BEFORE=0  # thread count before tests, for cleanup

# ── Dependency check ────────────────────────────────────────────────────
_check_deps() {
    if ! command -v tmux &>/dev/null; then
        echo -e "${RED}Error: tmux is not installed${NC}"
        exit 1
    fi
    if [[ ! -f "$CLIENT" ]]; then
        echo -e "${RED}Error: omnish-client not found at $CLIENT${NC}"
        echo -e "${YELLOW}Hint: Run 'cargo build' first${NC}"
        exit 1
    fi
}

# ── Cleanup ──────────────────────────────────────────────────────────────
cleanup() {
    echo -e "${YELLOW}Cleaning up tmux session...${NC}"
    _tmux kill-session -t "$SESSION" 2>/dev/null || true
    rm -f "$TMUX_CONF"
}

# ── Initialization ───────────────────────────────────────────────────────
# test_init <test-name> "$@"
#   Sets up socket/session names, checks deps, parses common flags,
#   registers cleanup trap.
test_init() {
    local name="$1"; shift

    SESSION="omnish-test-${name}"
    SOCKET="$SOCKET_DIR/${SESSION}.sock"
    PANE="$SESSION:0.0"
    mkdir -p "$SOCKET_DIR"

    _check_deps

    # Parse common flags; remaining args are ignored (caller can parse more)
    while [[ $# -gt 0 ]]; do
        case $1 in
            -w)          _WAIT_FOR_USER=true; shift ;;
            -t)          _TEST_CASES="${2:?-t requires an argument}"; shift 2 ;;
            -h|--help)   _show_help; exit 0 ;;
            *)           echo "Unknown option: $1"; exit 1 ;;
        esac
    done

    trap cleanup EXIT
}

_show_help() {
    echo "Usage: $(basename "$0") [-w] [-t TEST_CASE] [-h]"
    echo ""
    echo "Options:"
    echo "  -w             Wait for user confirmation before starting tests"
    echo "  -t TEST_CASE   Run specific test (number or 'all', default: all)"
    echo "  -h, --help     Show this help message"
    # Caller can override show_usage to add test-specific docs
    if declare -F show_usage &>/dev/null; then
        echo ""
        show_usage
    fi
}

# ── Client lifecycle ─────────────────────────────────────────────────────

# Start a fresh omnish-client in the tmux session.
# Kills any existing session first.
# If -w already started the client, skip the first call.
start_client() {
    if [[ "$_WAIT_CLIENT_STARTED" == "true" ]]; then
        _WAIT_CLIENT_STARTED=false
        return
    fi
    _tmux kill-session -t "$SESSION" 2>/dev/null || true
    _tmux new -d -s "$SESSION" -n test "$CLIENT"
}

# Alias: kill + start (useful between test cases).
restart_client() {
    start_client
}

# ── Sending input ────────────────────────────────────────────────────────

# send_keys <text> [wait_seconds=0.5]
#   Sends literal text to the pane.
send_keys() {
    local keys="$1"
    local wait="${2:-0.5}"
    echo -e "  Sending: ${YELLOW}${keys}${NC}"
    _tmux send-keys -t "$PANE" -- "$keys"
    sleep "$wait"
}

# send_enter [wait_seconds=0.5]
send_enter() {
    local wait="${1:-0.5}"
    echo -e "  Sending: ${YELLOW}Enter${NC}"
    _tmux send-keys -t "$PANE" Enter
    sleep "$wait"
}

# send_special <tmux-key-name> [wait_seconds=0.5]
#   e.g. send_special BSpace, send_special Escape, send_special C-d
send_special() {
    local key="$1"
    local wait="${2:-0.5}"
    echo -e "  Sending: ${YELLOW}${key}${NC}"
    _tmux send-keys -t "$PANE" "$key"
    sleep "$wait"
}

# send_backspace [wait_seconds=0.5]
send_backspace() {
    send_special BSpace "${1:-0.5}"
}

# ── Pane capture ─────────────────────────────────────────────────────────

# capture_pane [history_lines=-100]
#   Prints captured pane content to stdout.
capture_pane() {
    local lines="${1:--100}"
    _tmux capture-pane -p -J -t "$PANE" -S "$lines"
}

# last_nonempty_line <content>
#   Prints the last non-empty line from the given text.
last_nonempty_line() {
    echo "$1" | grep -v '^[[:space:]]*$' | tail -1
}

# ── Prompt detection ─────────────────────────────────────────────────────

# is_chat_prompt <content>
#   Returns 0 if the last non-empty line looks like a chat prompt "> ".
is_chat_prompt() {
    local last
    last=$(last_nonempty_line "$1")
    echo "$last" | grep -qE '^\s*((\[36m)?> (\[0m)?|> )$'
}

# is_shell_prompt <content>
#   Returns 0 if the last non-empty line looks like a shell prompt ending in $.
is_shell_prompt() {
    local last
    last=$(last_nonempty_line "$1")
    echo "$last" | grep -q '\$ $\|\$$'
}

# ── Waiting helpers ──────────────────────────────────────────────────────

# wait_for_client [seconds=1.5]
#   Wait for omnish-client to be ready after start.
wait_for_client() {
    sleep "${1:-1.5}"
}

# wait_for_prompt [seconds=0.5]
#   Short wait for a prompt to appear.
wait_for_prompt() {
    sleep "${1:-0.5}"
}

# ── Assertions ───────────────────────────────────────────────────────────

# assert_pass <message>
assert_pass() {
    echo -e "  ${GREEN}PASS: $1${NC}"
}

# assert_fail <message>
assert_fail() {
    echo -e "  ${RED}FAIL: $1${NC}"
}

# show_capture <label> <content> [tail_lines=10]
#   Pretty-print captured pane content for debugging.
show_capture() {
    local label="$1"
    local content="$2"
    local lines="${3:-10}"
    echo -e "  ${label}:"
    echo "$content" | tail -"$lines" | sed 's/^/    /'
}

# ── Thread cleanup ───────────────────────────────────────────────────────
# Uses a separate tmux window ("cleanup") with its own omnish-client
# so thread operations don't interfere with the test pane.

_CLEANUP_PANE=""  # set by _start_cleanup_client

# _start_cleanup_client
#   Create a new tmux window in the test session running omnish-client.
_start_cleanup_client() {
    _tmux new-window -t "$SESSION" -n cleanup "$CLIENT" 2>/dev/null
    _CLEANUP_PANE="$SESSION:cleanup.0"
    sleep 1.5  # wait for client to connect
}

# _kill_cleanup_client
_kill_cleanup_client() {
    _tmux kill-window -t "$SESSION:cleanup" 2>/dev/null || true
    _CLEANUP_PANE=""
}

# _count_threads
#   Start a cleanup client, enter chat, run /threads, count [N] lines, kill it.
#   Prints the count to stdout.
_count_threads() {
    _start_cleanup_client
    _tmux send-keys -t "$_CLEANUP_PANE" -- ":" 2>/dev/null
    sleep 0.5
    _tmux send-keys -t "$_CLEANUP_PANE" -- "/threads" 2>/dev/null
    _tmux send-keys -t "$_CLEANUP_PANE" Enter 2>/dev/null
    sleep 1
    local content
    content=$(_tmux capture-pane -p -J -t "$_CLEANUP_PANE" -S -100 2>/dev/null) || { _kill_cleanup_client; echo 0; return; }
    local count
    count=$(echo "$content" | grep -cE '^\s*\[[0-9]+\]') || true
    _kill_cleanup_client
    echo "$count"
}

# _snapshot_thread_count
#   Record current thread count before tests start.
_snapshot_thread_count() {
    _THREADS_BEFORE=$(_count_threads)
    echo -e "${YELLOW}Threads before tests: ${_THREADS_BEFORE}${NC}"
}

# _cleanup_new_threads
#   Delete threads created during tests (new threads appear at top = low indices).
#   Uses a separate cleanup client window.
_cleanup_new_threads() {
    # Protection: if initial thread count is 0, skip deletion to avoid deleting
    # all threads when detection might be faulty (issue #159)
    if [[ $_THREADS_BEFORE -eq 0 ]]; then
        echo -e "${YELLOW}Warning: Initial thread count is 0, skipping cleanup to prevent accidental deletion${NC}"
        echo -e "${YELLOW}This may leave test threads behind, but avoids deleting potentially existing threads${NC}"
        return
    fi

    _start_cleanup_client
    # Enter chat, list threads
    _tmux send-keys -t "$_CLEANUP_PANE" -- ":" 2>/dev/null
    sleep 0.5
    _tmux send-keys -t "$_CLEANUP_PANE" -- "/threads" 2>/dev/null
    _tmux send-keys -t "$_CLEANUP_PANE" Enter 2>/dev/null
    sleep 1
    local content
    content=$(_tmux capture-pane -p -J -t "$_CLEANUP_PANE" -S -100 2>/dev/null) || { _kill_cleanup_client; return; }
    local after
    after=$(echo "$content" | grep -cE '^\s*\[[0-9]+\]') || true
    local new_count=$((after - _THREADS_BEFORE))
    if [[ $new_count -le 0 ]]; then
        echo -e "${YELLOW}No new threads to clean up (before=$_THREADS_BEFORE, after=$after)${NC}"
        _kill_cleanup_client
        return
    fi
    echo -e "${YELLOW}Cleaning up $new_count new thread(s) (before=$_THREADS_BEFORE, after=$after)...${NC}"
    # Delete indices 1..new_count in one command (new threads are at top)
    _tmux send-keys -t "$_CLEANUP_PANE" -- "/threads del 1-$new_count" 2>/dev/null
    _tmux send-keys -t "$_CLEANUP_PANE" Enter 2>/dev/null
    sleep 1
    _kill_cleanup_client
    echo -e "${YELLOW}Thread cleanup done${NC}"
}

# ── Test runner ──────────────────────────────────────────────────────────

# run_tests <max_test_number>
#   Discovers and runs test functions named test_1 .. test_N.
#   Respects -t flag for selective execution.
#   Prints summary and exits with 0 (all pass) or 1 (some fail).
run_tests() {
    _TEST_MAX="$1"

    # Validate -t argument
    if [[ "$_TEST_CASES" != "all" ]]; then
        if ! [[ "$_TEST_CASES" =~ ^[0-9]+$ ]] || (( _TEST_CASES < 1 || _TEST_CASES > _TEST_MAX )); then
            echo -e "${RED}Error: Invalid test case '$_TEST_CASES'. Must be 1..$_TEST_MAX or 'all'${NC}"
            exit 1
        fi
    fi

    echo -e "${YELLOW}Using tmux socket: $SOCKET${NC}"
    echo -e "${YELLOW}To monitor: tmux -f '$TMUX_CONF' -S '$SOCKET' attach -t $SESSION${NC}"

    if [[ "$_WAIT_FOR_USER" == "true" ]]; then
        # Start client early so the user can attach and observe
        start_client
        wait_for_client
        echo -e "${YELLOW}Tmux session started. Attach with:${NC}"
        echo -e "${YELLOW}  tmux -f '$TMUX_CONF' -S '$SOCKET' attach -t $SESSION${NC}"
        echo -e "${YELLOW}Press Enter to start tests...${NC}"
        read -r
        _WAIT_CLIENT_STARTED=true
    fi

    # Snapshot thread count before tests
    if ! _tmux has-session -t "$SESSION" 2>/dev/null; then
        start_client
        wait_for_client
    fi
    _snapshot_thread_count

    _TEST_PASSED=0
    _TEST_TOTAL=0

    for i in $(seq 1 "$_TEST_MAX"); do
        if [[ "$_TEST_CASES" == "all" || "$_TEST_CASES" == "$i" ]]; then
            ((_TEST_TOTAL++))
            if "test_$i"; then
                ((_TEST_PASSED++))
            fi
        fi
    done

    # Clean up threads created during tests
    if _tmux has-session -t "$SESSION" 2>/dev/null; then
        _cleanup_new_threads
    fi

    # Summary
    echo -e "\n${YELLOW}=== Test Summary ===${NC}"
    if [[ $_TEST_TOTAL -eq 0 ]]; then
        echo -e "  ${YELLOW}No tests were run.${NC}"
        exit 0
    fi

    echo -e "  Passed: ${GREEN}$_TEST_PASSED${NC} / $_TEST_TOTAL"

    if [[ $_TEST_PASSED -eq $_TEST_TOTAL ]]; then
        echo -e "${GREEN}All tests passed!${NC}"
        exit 0
    else
        echo -e "${RED}Some tests failed.${NC}"
        exit 1
    fi
}
