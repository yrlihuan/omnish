#!/usr/bin/env bash
#
# test_plugin_cwd.sh - Test omnish-plugin bash tool CWD fallback (#540)
#
# Verifies that when the working directory no longer exists (e.g. unmounted),
# the bash tool falls back to $HOME instead of failing with ENOENT.
#
# This test invokes the omnish-plugin binary directly (no tmux/client needed).

set -uo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"
PLUGIN="$PROJECT_ROOT/target/release/omnish-plugin"

if [[ ! -f "$PLUGIN" ]]; then
    echo -e "${RED}Error: omnish-plugin not found at $PLUGIN${NC}"
    echo -e "${YELLOW}Hint: Run 'cargo build --release -p omnish-plugin' first${NC}"
    exit 1
fi

PASSED=0
TOTAL=0

assert_pass() { ((TOTAL++)); ((PASSED++)); echo -e "  ${GREEN}PASS: $1${NC}"; }
assert_fail() { ((TOTAL++)); echo -e "  ${RED}FAIL: $1${NC}"; }

# Helper: call omnish-plugin bash tool and return JSON response
call_bash() {
    local command="$1"
    local cwd="$2"
    echo "{\"name\":\"bash\",\"input\":{\"command\":\"$command\",\"cwd\":\"$cwd\"}}" \
        | "$PLUGIN"
}

# ── Test 1: valid CWD works normally ──────────────────────────────────────
echo -e "\n${YELLOW}=== Test 1: valid CWD works normally ===${NC}"
TMPDIR_TEST=$(mktemp -d /tmp/omnish-cwd-test.XXXXXX)

resp=$(call_bash "echo hello" "$TMPDIR_TEST")
echo "  response: $resp"

if echo "$resp" | grep -q '"is_error":false' && echo "$resp" | grep -q 'hello'; then
    assert_pass "bash tool works with valid CWD"
else
    assert_fail "bash tool failed with valid CWD: $resp"
fi

# ── Test 2: invalid CWD falls back gracefully ────────────────────────────
echo -e "\n${YELLOW}=== Test 2: invalid CWD falls back instead of ENOENT ===${NC}"
rmdir "$TMPDIR_TEST"

resp=$(call_bash "echo recovered" "$TMPDIR_TEST")
echo "  response: $resp"

if echo "$resp" | grep -q '"is_error":true'; then
    assert_fail "bash tool returned is_error:true for invalid CWD (no fallback): $resp"
elif echo "$resp" | grep -q 'recovered'; then
    assert_pass "bash tool executed command despite invalid CWD"
else
    assert_fail "unexpected response: $resp"
fi

# ── Test 3: fallback note mentions the invalid directory ──────────────────
echo -e "\n${YELLOW}=== Test 3: output includes fallback note ===${NC}"

if echo "$resp" | grep -q 'no longer exists'; then
    assert_pass "output includes 'no longer exists' note"
else
    assert_fail "output missing fallback note: $resp"
fi

# ── Test 4: pwd in fallback CWD matches HOME ─────────────────────────────
echo -e "\n${YELLOW}=== Test 4: fallback CWD is HOME ===${NC}"
GONE_DIR="/tmp/omnish-cwd-gone-$$"

resp=$(call_bash "pwd" "$GONE_DIR")
echo "  response: $resp"

if echo "$resp" | grep -q "$HOME"; then
    assert_pass "fallback CWD is \$HOME"
else
    assert_fail "fallback CWD is not \$HOME: $resp"
fi

# ── Summary ───────────────────────────────────────────────────────────────
echo ""
if [[ $PASSED -eq $TOTAL ]]; then
    echo -e "${GREEN}All $TOTAL tests passed${NC}"
    exit 0
else
    echo -e "${RED}$PASSED/$TOTAL tests passed${NC}"
    exit 1
fi
