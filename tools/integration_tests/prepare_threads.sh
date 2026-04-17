#!/usr/bin/env bash
#
# prepare_threads.sh - Populate chat threads before integration tests.
#
# Creates a few conversation threads so tests run against a more realistic
# state (existing threads, /thread list non-empty, /resume targets, etc.).
#
# Usage:
#   bash tools/integration_tests/prepare_threads.sh
#
# Requires: tmux, omnish (release build), running omnish-daemon.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

test_init "prepare" "$@"

# ── Helpers ─────────────────────────────────────────────────────────────

# chat_exchange <message> [timeout=120]
#   Send a message in chat mode and wait for the LLM response.
chat_exchange() {
    local msg="$1"
    local timeout="${2:-120}"
    send_keys "$msg" 0.3
    send_enter 0.3
    if ! wait_for_chat_response "$timeout"; then
        echo -e "${RED}  Timeout waiting for response to: $msg${NC}"
        return 1
    fi
    return 0
}

# ── Main ────────────────────────────────────────────────────────────────

start_client
wait_for_client

echo -e "${YELLOW}=== Preparing chat threads ===${NC}"

# ── Thread 1: Simple math conversation (2 turns) ──
echo -e "${YELLOW}--- Thread 1: Math Q&A ---${NC}"
enter_chat
chat_exchange "What is 7 * 8? Reply with just the number." || true
chat_exchange "Add 6 to that. Reply with just the number." || true
send_special Escape 0.5
sleep 1.5  # exceed intercept_gap_ms

# ── Thread 2: General knowledge (2 turns) ──
echo -e "${YELLOW}--- Thread 2: General knowledge ---${NC}"
enter_chat
chat_exchange "Name the four largest planets in our solar system. Be brief." || true
chat_exchange "Which one has the most moons? Be brief." || true
send_special Escape 0.5
sleep 1.5

# ── Thread 3: Code-related (1 turn) ──
echo -e "${YELLOW}--- Thread 3: Code snippet ---${NC}"
enter_chat
chat_exchange "Write a one-line bash command that counts the number of .rs files in the current directory recursively. Just the command, no explanation." || true
send_special Escape 0.5
sleep 1.5

# ── Verify ──
echo -e "${YELLOW}--- Verifying threads ---${NC}"
enter_chat
send_keys "/thread list" 0.3
send_enter 1

content=$(capture_pane -30)
thread_count=$(echo "$content" | grep -cE '^\s*\[[0-9]+\]') || true
echo -e "${GREEN}Created $thread_count thread(s)${NC}"

if [[ $thread_count -ge 3 ]]; then
    echo -e "${GREEN}Thread preparation complete.${NC}"
else
    echo -e "${YELLOW}Warning: Expected 3 threads, found $thread_count${NC}"
fi

send_special Escape 0.5

# Clean exit (don't delete threads - that's the whole point)
trap - EXIT
_tmux kill-session -t "$SESSION" 2>/dev/null || true
rm -f "$TMUX_CONF"
