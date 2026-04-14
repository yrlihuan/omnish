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
CLIENT="$PROJECT_ROOT/target/release/omnish"

# ── Tmux config (override default shell to avoid running installed omnish) ──
TMUX_CONF="$(mktemp /tmp/omnish-test-tmux.XXXXXX.conf)"
# ── Shell selection (TEST_SHELL env var, default: bash) ─────────────────
TEST_SHELL="${TEST_SHELL:-bash}"
_resolve_test_shell() {
    case "$TEST_SHELL" in
        bash) echo "/bin/bash" ;;
        zsh)  echo "/bin/zsh" ;;
        *)    echo "/bin/$TEST_SHELL" ;;
    esac
}
TEST_SHELL_PATH="$(_resolve_test_shell)"
if [[ ! -x "$TEST_SHELL_PATH" ]]; then
    echo -e "${YELLOW}SKIP: $TEST_SHELL_PATH not found${NC}"
    exit 0
fi
echo "set -g default-shell $TEST_SHELL_PATH" > "$TMUX_CONF"

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
        echo -e "${RED}Error: omnish not found at $CLIENT${NC}"
        echo -e "${YELLOW}Hint: Run 'cargo build --release -p omnish-client' first${NC}"
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
    echo -e "${YELLOW}Shell: $TEST_SHELL ($TEST_SHELL_PATH)${NC}"

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
    echo "  -w             Wait for user confirmation before and after each test"
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

# enter_chat [wait_before=1]
#   Wait for intercept gap then send ":" to enter chat mode.
enter_chat() {
    local wait="${1:-1}"
    sleep "$wait"
    send_keys ":" 0.5
    wait_for_prompt
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

# ── Picker helpers ──────────────────────────────────────────────────────

# pick_option <target_text> [wait=0.5]
#   Reads the currently open picker from the pane, navigates to the option
#   matching <target_text>, and confirms with Enter.
#   Must be called after the picker has been opened (send_enter on a Select).
#   Returns 1 if the target option is not found.
pick_option() {
    local target="$1"
    local wait="${2:-0.5}"

    sleep 0.3  # let picker render
    local content
    content=$(capture_pane -30)

    # Picker format (rendered by picker.rs):
    #   Title
    #   ──────────
    #   > selected_option     ("> " prefix)
    #     other_option         ("  " prefix)
    #   ──────────
    #   ↑↓ move  Enter confirm  ESC cancel
    #
    # Locate the picker by finding the hint line, then scanning upward
    # for the two ─── separators that bracket the items.

    local -a lines
    mapfile -t lines <<< "$content"
    local total=${#lines[@]}

    # Find hint line (last line containing both "confirm" and "cancel")
    local hint_idx=-1
    for ((i = total - 1; i >= 0; i--)); do
        if [[ "${lines[$i]}" == *confirm* && "${lines[$i]}" == *cancel* ]]; then
            hint_idx=$i
            break
        fi
    done
    if [[ $hint_idx -eq -1 ]]; then
        echo -e "  ${RED}pick_option: picker hint line not found${NC}"
        show_capture "Picker pane" "$content" 15
        return 1
    fi

    # Find bottom separator (first ─ line above hint)
    local bottom_sep=-1
    for ((i = hint_idx - 1; i >= 0; i--)); do
        if [[ "${lines[$i]}" == *─* ]]; then
            bottom_sep=$i; break
        fi
    done

    # Find top separator (next ─ line above items)
    local top_sep=-1
    for ((i = bottom_sep - 1; i >= 0; i--)); do
        if [[ "${lines[$i]}" == *─* ]]; then
            top_sep=$i; break
        fi
    done

    if [[ $top_sep -eq -1 || $bottom_sep -eq -1 ]]; then
        echo -e "  ${RED}pick_option: picker separators not found${NC}"
        show_capture "Picker pane" "$content" 15
        return 1
    fi

    # Parse items between separators
    local current_idx=-1
    local target_idx=-1
    local idx=0

    for ((i = top_sep + 1; i < bottom_sep; i++)); do
        local line="${lines[$i]}"
        # Strip leading/trailing whitespace
        local trimmed
        trimmed=$(echo "$line" | sed 's/^[[:space:]]*//' | sed 's/[[:space:]]*$//')
        [[ -z "$trimmed" ]] && continue

        # Detect current selection ("> " prefix)
        if [[ "$trimmed" == ">"* ]]; then
            current_idx=$idx
        fi

        # Extract option text (remove "> " or leading spaces)
        local item_text
        item_text=$(echo "$trimmed" | sed 's/^> //' | sed 's/^[[:space:]]*//' | sed 's/[[:space:]]*$//')

        if [[ "$item_text" == "$target" ]]; then
            target_idx=$idx
        fi

        idx=$((idx + 1))
    done

    if [[ $target_idx -eq -1 ]]; then
        echo -e "  ${RED}pick_option: '$target' not found in picker options${NC}"
        for ((i = top_sep + 1; i < bottom_sep; i++)); do
            echo -e "    option: '${lines[$i]}'"
        done
        return 1
    fi

    [[ $current_idx -eq -1 ]] && current_idx=0

    echo -e "  Picking: ${YELLOW}${target}${NC} (from=${current_idx}, to=${target_idx})"

    local delta=$((target_idx - current_idx))
    if [[ $delta -gt 0 ]]; then
        for _ in $(seq 1 $delta); do
            send_special Down 0.15
        done
    elif [[ $delta -lt 0 ]]; then
        for _ in $(seq 1 $((-delta))); do
            send_special Up 0.15
        done
    fi

    send_enter "$wait"
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
#   Matches both plain "> " and "> " with hint text (e.g., "> type to start...")
is_chat_prompt() {
    local last
    last=$(last_nonempty_line "$1")
    # Match "> " at start of line, optionally with ANSI color codes
    # Allow any text after "> " (e.g., hint text)
    echo "$last" | grep -qE '^\s*(\[36m)?> (\[0m)?'
}

# is_shell_prompt <content>
#   Returns 0 if the last non-empty line looks like a shell prompt.
#   Matches bash ($ or #) and zsh (% or #) prompt endings.
#   Also matches when ghost text (completion suggestion) follows the prompt.
is_shell_prompt() {
    local last
    last=$(last_nonempty_line "$1")
    echo "$last" | grep -qE '[\$#%] |[\$#%]$'
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

# wait_for_chat_response [timeout=180] [interval=2]
#   Poll until chat prompt "> " appears in the pane or timeout.
#   Returns 0 on success, 1 on timeout.
wait_for_chat_response() {
    local timeout="${1:-180}"
    local interval="${2:-2}"
    local elapsed=0
    echo -e "  Waiting up to ${timeout}s for LLM response..."
    while [[ $elapsed -lt $timeout ]]; do
        local content
        content=$(capture_pane -10)
        if is_chat_prompt "$content"; then
            sleep 1  # brief pause for visual observation
            return 0
        fi
        sleep "$interval"
        elapsed=$((elapsed + interval))
    done
    return 1
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

# ── Context helpers ──────────────────────────────────────────────────────

# dump_context_to_file [type="chat"] [timeout=10]
#   Runs /context [type] in chat mode, captures output to a temp file.
#   Must be called when already in chat mode (after send_keys ":").
#   Usage: local ctx_file; ctx_file=$(dump_context_to_file); content=$(cat "$ctx_file")
dump_context_to_file() {
    local ctx_type="${1:-chat}"
    local timeout="${2:-10}"
    local out_file="/tmp/omnish_ctx_${ctx_type}_$$.txt"

    # Clear any existing output file
    rm -f "$out_file"

    # Run /context command in chat mode
    send_keys "/context $ctx_type" 0.3
    send_enter 3

    # Wait for output to appear (look for system-reminder end tag or WORKING DIR)
    local waited=0
    local content=""
    while [[ $waited -lt $timeout ]]; do
        content=$(capture_pane -50)
        # Check if context output is complete (has WORKING DIR or system-reminder)
        if echo "$content" | grep -q "WORKING DIR:" || echo "$content" | grep -q '</system-reminder>'; then
            break
        fi
        sleep 0.5
        waited=$((waited + 1))
    done

    # Save captured content to file
    echo "$content" > "$out_file"

    # Exit chat mode (auto-exits after /context command)
    send_special Escape 0.3

    echo "$out_file"
}

# get_working_dir_from_context <context_content>
#   Extracts WORKING DIR value from context output.
#   Usage: local cwd; cwd=$(get_working_dir_from_context "$ctx_content")
get_working_dir_from_context() {
    local content="$1"
    echo "$content" | grep "WORKING DIR:" | sed 's/.*WORKING DIR: *//' | tr -d '[:space:]' | head -1
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
#   Start a cleanup client, enter chat, run /thread list, count [N] lines, kill it.
#   Polls up to 5s for /thread list output to appear before counting.
#   Prints the count to stdout.
_count_threads() {
    _start_cleanup_client
    _tmux send-keys -t "$_CLEANUP_PANE" -- ":" 2>/dev/null
    sleep 0.5
    _tmux send-keys -t "$_CLEANUP_PANE" -- "/thread list" 2>/dev/null
    _tmux send-keys -t "$_CLEANUP_PANE" Enter 2>/dev/null
    # Poll for output (either [N] lines or "No conversations") up to 5s
    local content="" count=0
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        sleep 0.5
        content=$(_tmux capture-pane -p -J -t "$_CLEANUP_PANE" -S -100 2>/dev/null) || continue
        if echo "$content" | grep -qE '^\s*\[[0-9]+\]|No conversations'; then
            break
        fi
    done
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
    # Skip if initial count was 0 to avoid deleting all threads on faulty detection
    if [[ $_THREADS_BEFORE -eq 0 ]]; then
        echo -e "${YELLOW}Warning: initial thread count was 0, skipping cleanup${NC}"
        return
    fi

    _start_cleanup_client
    # Enter chat, list threads
    _tmux send-keys -t "$_CLEANUP_PANE" -- ":" 2>/dev/null
    sleep 0.5
    _tmux send-keys -t "$_CLEANUP_PANE" -- "/thread list" 2>/dev/null
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
    _tmux send-keys -t "$_CLEANUP_PANE" -- "/thread del 1-$new_count" 2>/dev/null
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
            if [[ "$_WAIT_FOR_USER" == "true" ]]; then
                echo -e "${YELLOW}Test $i done. Press Enter to continue...${NC}"
                read -r
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
