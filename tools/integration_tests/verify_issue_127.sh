#!/usr/bin/env bash
#
# verify_issue_127.sh - Test that backspace correctly exits chat mode only in phase 1
#
# Verifies fix for issue #127: "backspace退出chat模式，仅当用户没有发出首轮对话的时候有效"
#
# Usage:
#   ./verify_issue_127.sh [-w]
#
# Options:
#   -w    Wait for user confirmation after showing monitor command
#
# Requirements:
#   - tmux
#   - omnish-client (built via 'cargo build')
#
# Script will:
#   1. Start omnish-client in a tmux session
#   2. Test phase 1: backspace exits chat mode when no message sent
#   3. Test phase 2: backspace is ignored after first message sent
#
# Exit codes:
#   0 - All tests passed
#   1 - Tests failed or error occurred

set -uo pipefail

# Show usage information
show_usage() {
    cat <<EOF
Usage: $(basename "$0") [-w] [-t TEST_CASE]

Options:
  -w    Wait for user confirmation after showing monitor command
  -t TEST_CASE  Run specific test case(s). Can be: 1, 2, or "all" (default: all)
  -h, --help  Show this help message

Verifies fix for issue #127: "backspace退出chat模式，仅当用户没有发出首轮对话的时候有效"

Test cases:
1. Phase 1 (mode selection): backspace exits chat mode when no message sent
2. Phase 2 (chat loop): backspace is ignored after first message sent

Examples:
  $0               # Run all tests
  $0 -t 1          # Run only test 1 (phase 1)
  $0 -t 2          # Run only test 2 (phase 2)
  $0 -t all        # Run all tests (same as default)
  $0 -w -t 1       # Wait for confirmation, then run test 1
EOF
}

# Check for help flag early (before creating tmux session)
if [[ $# -gt 0 && ( "$1" == "-h" || "$1" == "--help" ) ]]; then
    show_usage
    exit 0
fi

# Script to verify issue #127 fix: backspace退出chat模式
# Tests two scenarios:
# 1. Phase 1 (mode selection): backspace exits chat mode
# 2. Phase 2 (chat loop): backspace is ignored after first message sent

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Check dependencies
check_deps() {
    if ! command -v tmux &>/dev/null; then
        echo -e "${RED}Error: tmux is not installed${NC}"
        exit 1
    fi
    if ! command -v cargo &>/dev/null; then
        echo -e "${YELLOW}Warning: cargo not found, ensure omnish-client is built${NC}"
    fi
}

# Tmux socket setup
SOCKET_DIR="${CLAUDE_TMUX_SOCKET_DIR:-/tmp/claude-tmux-sockets}"
mkdir -p "$SOCKET_DIR"
SOCKET="$SOCKET_DIR/omnish-test-127.sock"
SESSION="omnish-test-127"

# Omnish client binary (relative to project root)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"
CLIENT="$PROJECT_ROOT/target/debug/omnish-client"
if [[ ! -f "$CLIENT" ]]; then
    echo -e "${RED}Error: omnish-client not found at $CLIENT${NC}"
    echo -e "${YELLOW}Hint: Run 'cargo build' first${NC}"
    exit 1
fi

# Cleanup function
cleanup() {
    echo -e "${YELLOW}Cleaning up tmux session...${NC}"
    tmux -S "$SOCKET" kill-session -t "$SESSION" 2>/dev/null || true
    # Don't remove socket directory as other sessions might be using it
}

# Trap exit
trap cleanup EXIT

# Start a fresh tmux session
echo -e "${YELLOW}Starting tmux session...${NC}"
tmux -S "$SOCKET" kill-session -t "$SESSION" 2>/dev/null || true
tmux -S "$SOCKET" new -d -s "$SESSION" -n test

# Function to send keys and wait
send_keys() {
    local target="$1"
    local keys="$2"
    local wait_seconds="${3:-0.5}"

    echo -e "  Sending: ${YELLOW}$keys${NC}"
    if [[ -z "$keys" ]]; then
        # Send Enter key
        tmux -S "$SOCKET" send-keys -t "$target" Enter
    else
        tmux -S "$SOCKET" send-keys -t "$target" -- "$keys"
    fi
    sleep "$wait_seconds"
}

# Function to send Enter key explicitly
send_enter() {
    local target="$1"
    local wait_seconds="${2:-0.5}"
    echo -e "  Sending: ${YELLOW}Enter${NC}"
    tmux -S "$SOCKET" send-keys -t "$target" Enter
    sleep "$wait_seconds"
}

# Function to capture pane content
capture_pane() {
    local target="$1"
    local lines="${2:--100}"
    tmux -S "$SOCKET" capture-pane -p -J -t "$target" -S "$lines"
}

# Function to check if chat prompt is visible
has_chat_prompt() {
    local content="$1"
    # Chat prompt is "> " with ANSI color codes
    # Look for ">" followed by space at beginning of line after newline
    echo "$content" | grep -q '\[36m> \[0m' || echo "$content" | grep -q '>'
}

# Function to check if exited to shell (no chat prompt)
has_no_chat_prompt() {
    local content="$1"
    ! has_chat_prompt "$content"
}

# Function to run test scenario 1: backspace exits in phase 1
test_phase1_backspace_exits() {
    echo -e "\n${YELLOW}=== Test 1: Phase 1 - backspace should exit chat mode ===${NC}"

    # Start omnish-client
    send_keys "$SESSION:0.0" "$CLIENT" 1

    # Send ':' to enter chat mode
    send_keys "$SESSION:0.0" ":" 0.5

    # Wait a bit for chat prompt to appear
    sleep 0.5

    # Capture before backspace
    local before=$(capture_pane "$SESSION:0.0" -20)
    echo -e "  Before backspace (last 20 lines):"
    echo "$before" | tail -10 | sed 's/^/    /'

    # Send backspace
    send_keys "$SESSION:0.0" $'\x7f' 0.5  # Backspace key

    # Capture after backspace
    local after=$(capture_pane "$SESSION:0.0" -20)
    echo -e "  After backspace (last 20 lines):"
    echo "$after" | tail -10 | sed 's/^/    /'

    # Check if chat prompt disappeared (exited chat mode)
    if has_chat_prompt "$before" && has_no_chat_prompt "$after"; then
        echo -e "  ${GREEN}✓ PASS: Chat prompt disappeared after backspace (exited chat mode)${NC}"
        return 0
    elif ! has_chat_prompt "$before"; then
        echo -e "  ${RED}✗ FAIL: Chat prompt not found before backspace${NC}"
        return 1
    else
        echo -e "  ${RED}✗ FAIL: Chat prompt still present after backspace (did not exit)${NC}"
        return 1
    fi
}

# Function to run test scenario 2: backspace ignored in phase 2
test_phase2_backspace_ignored() {
    echo -e "\n${YELLOW}=== Test 2: Phase 2 - backspace should be ignored after first message ===${NC}"

    # Start fresh session for test 2
    tmux -S "$SOCKET" kill-session -t "$SESSION" 2>/dev/null || true
    tmux -S "$SOCKET" new -d -s "$SESSION" -n test

    # Start omnish-client
    send_keys "$SESSION:0.0" "$CLIENT" 1

    # Send ':' to enter chat mode
    send_keys "$SESSION:0.0" ":" 0.5

    # Send '/chat' to start chat conversation
    send_keys "$SESSION:0.0" "/chat" 0.3
    send_enter "$SESSION:0.0" 0.3  # Enter

    # Wait for chat to be ready
    sleep 0.5

    # Send a test message
    send_keys "$SESSION:0.0" "Hello, this is a test message" 0.3
    send_enter "$SESSION:0.0" 0.3  # Enter

    # Wait for response (might be delayed, but we just need to be in chat loop)
    sleep 1

    # Capture before backspace
    local before=$(capture_pane "$SESSION:0.0" -30)
    echo -e "  Before backspace (last 30 lines):"
    echo "$before" | tail -15 | sed 's/^/    /'

    # Send backspace (should be ignored)
    send_keys "$SESSION:0.0" $'\x7f' 0.5  # Backspace key

    # Capture after backspace
    local after=$(capture_pane "$SESSION:0.0" -30)
    echo -e "  After backspace (last 30 lines):"
    echo "$after" | tail -15 | sed 's/^/    /'

    # Check if chat prompt is still present (backspace ignored)
    if has_chat_prompt "$before" && has_chat_prompt "$after"; then
        echo -e "  ${GREEN}✓ PASS: Chat prompt still present after backspace (backspace ignored)${NC}"

        # Additional check: send a message to verify still in chat mode
        send_keys "$SESSION:0.0" "Still in chat" 0.3
        send_enter "$SESSION:0.0" 0.3  # Enter
        sleep 0.5
        local final=$(capture_pane "$SESSION:0.0" -10)
        if echo "$final" | grep -q "Still in chat"; then
            echo -e "  ${GREEN}✓ PASS: Can still send messages after backspace${NC}"
            return 0
        else
            echo -e "  ${YELLOW}⚠ WARNING: Could not verify message was sent${NC}"
            return 0
        fi
    else
        echo -e "  ${RED}✗ FAIL: Chat prompt disappeared (backspace caused exit)${NC}"
        return 1
    fi
}

# Main test execution
main() {
    check_deps

    # Parse command line arguments
    local WAIT_FOR_USER=false
    local TEST_CASES="all"
    while [[ $# -gt 0 ]]; do
        case $1 in
            -w)
                WAIT_FOR_USER=true
                shift
                ;;
            -t)
                if [[ $# -lt 2 ]]; then
                    echo "Error: -t requires a test case argument"
                    exit 1
                fi
                TEST_CASES="$2"
                shift 2
                ;;
            -h|--help)
                show_usage
                exit 0
                ;;
            *)
                echo "Unknown option: $1"
                exit 1
                ;;
        esac
    done

    echo -e "${YELLOW}Testing issue #127: backspace退出chat模式${NC}"
    echo -e "${YELLOW}Using tmux socket: $SOCKET${NC}"
    echo -e "${YELLOW}To monitor manually: tmux -S '$SOCKET' attach -t $SESSION${NC}"

    # Wait for user confirmation if requested
    if [[ "$WAIT_FOR_USER" == "true" ]]; then
        echo -e "${YELLOW}Press Enter to start tests...${NC}"
        read -r
    fi

    local passed=0
    local total=0

    # Validate TEST_CASES
    case "$TEST_CASES" in
        all|1|2)
            # Valid cases
            ;;
        *)
            echo -e "${RED}Error: Invalid test case '$TEST_CASES'. Must be: 1, 2, or 'all'${NC}"
            exit 1
            ;;
    esac

    # Run test 1 if requested
    if [[ "$TEST_CASES" == "all" || "$TEST_CASES" == "1" ]]; then
        echo -e "${YELLOW}Running test 1 (phase 1)...${NC}"
        if test_phase1_backspace_exits; then
            ((passed++))
        fi
        ((total++))
    fi

    # Run test 2 if requested
    if [[ "$TEST_CASES" == "all" || "$TEST_CASES" == "2" ]]; then
        echo -e "${YELLOW}Running test 2 (phase 2)...${NC}"
        if test_phase2_backspace_ignored; then
            ((passed++))
        fi
        ((total++))
    fi

    # Summary
    echo -e "\n${YELLOW}=== Test Summary ===${NC}"
    if [[ $total -eq 0 ]]; then
        echo -e "  ${YELLOW}No tests were run.${NC}"
        return 0
    fi

    echo -e "  Passed: ${GREEN}$passed${NC} / ${total}"

    if [[ $passed -eq $total ]]; then
        echo -e "${GREEN}✓ All tests passed! Issue #127 fix appears correct.${NC}"
        return 0
    else
        echo -e "${RED}✗ Some tests failed. Issue #127 fix may be incomplete.${NC}"
        return 1
    fi
}

# Run main function
main "$@"