#!/usr/bin/env bash
#
# test_capture.sh - Integration test for the /test capture screen-capture command.
#
# /test capture mirrors PTY output through an in-memory vt100 emulator (similar
# to tmux's capture-pane). The bare command returns the visible screen; with an
# integer argument it returns that many rows from the most recent history.
# It must support the same redirect/pipe grammar as other slash commands.
#
# Test cases:
#   1. /test capture -> visible screen contains recent shell output
#   2. /test capture N -> recent N rows include lines no longer on the visible screen
#   3. /test capture | tail K -> pipe limits the output
#   4. \r-driven progress bar -> capture shows ONLY final state, no intermediates
#   5. python tqdm progress bar -> only final tqdm line is captured (skip if no tqdm)
#   6. ANSI clear-screen (CSI 2J + cursor home) -> capture drops content above the clear
#   7. ANSI cursor-up + line-clear (multi-line redraw) -> capture shows final lines only

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. /test capture writes visible screen content
  2. /test capture N captures rows from scrollback
  3. /test capture supports | tail K and > redirect
  4. \\r-driven progress bar: capture shows only final state
  5. python tqdm progress bar: only final state in capture (skipped if tqdm absent)
  6. ANSI clear-screen drops everything above the clear
  7. ANSI cursor-up + clear-line: multi-line redraw shows final state only
EOF
}

test_init "test-capture" "$@"

# Wait until <file> exists (and is non-empty), up to <timeout> seconds.
_wait_for_file() {
    local file="$1"
    local timeout="${2:-5}"
    local deadline=$(($(date +%s) + timeout))
    while (( $(date +%s) < deadline )); do
        if [[ -s "$file" ]]; then
            return 0
        fi
        sleep 0.2
    done
    return 1
}

# ── Test 1: /test capture (visible screen) ────────────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: /test capture writes visible screen ===${NC}"

    restart_client
    wait_for_client

    # Generate distinctive shell output that should appear on the visible screen.
    local token="CAPVIS_TOKEN_$$"
    send_keys "echo $token" 0.3
    send_enter 1

    enter_chat

    local out_file="/tmp/omnish_capvis_$$.txt"
    rm -f "$out_file"

    send_keys "/test capture > $out_file" 0.3
    send_enter 1.5

    if ! _wait_for_file "$out_file" 5; then
        assert_fail "/test capture did not produce $out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    if grep -q "$token" "$out_file"; then
        assert_pass "/test capture visible output contains shell token $token"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "/test capture missing token $token"
        echo "    --- file contents ---"
        sed 's/^/    /' "$out_file" | head -30
        echo "    ----------------------"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 2: /test capture N (scrollback history) ──────────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: /test capture N captures scrollback ===${NC}"

    restart_client
    wait_for_client

    # Print enough lines that the early ones definitely scroll off the visible
    # screen (default tmux pane is 80x24).
    send_keys "for i in \$(seq 1 80); do echo HIST_\$i; done" 0.3
    send_enter 3

    enter_chat

    local out_file="/tmp/omnish_caphist_$$.txt"
    rm -f "$out_file"

    # Ask for the last 100 rows of history; should include both recent
    # (HIST_80) and older (HIST_5) lines.
    send_keys "/test capture 100 > $out_file" 0.3
    send_enter 2

    if ! _wait_for_file "$out_file" 5; then
        assert_fail "/test capture 100 did not produce $out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    local missing_recent=0 missing_older=0
    grep -q "HIST_80\b" "$out_file" || missing_recent=1
    grep -q "HIST_5\b" "$out_file" || missing_older=1

    if [[ $missing_recent -eq 0 && $missing_older -eq 0 ]]; then
        assert_pass "/test capture 100 contains recent (HIST_80) and old (HIST_5) lines"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "history missing: recent=$missing_recent older=$missing_older"
        echo "    --- file contents (first 40 lines) ---"
        sed 's/^/    /' "$out_file" | head -40
        echo "    ---------------------------------------"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 3: /test capture supports pipe limits ────────────────────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: /test capture | tail K + redirect ===${NC}"

    restart_client
    wait_for_client

    send_keys "for i in \$(seq 1 60); do echo PIPELN_\$i; done" 0.3
    send_enter 3

    enter_chat

    local out_file="/tmp/omnish_cappipe_$$.txt"
    rm -f "$out_file"

    # | tail 5 should keep at most 5 trailing lines of the captured content.
    # The last printed shell line should be PIPELN_60.
    send_keys "/test capture 100 | tail 5 > $out_file" 0.3
    send_enter 2

    if ! _wait_for_file "$out_file" 5; then
        assert_fail "/test capture | tail 5 did not produce $out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    local lines
    lines=$(wc -l < "$out_file" | tr -d ' ')

    # tail 5 keeps 5 lines; allow up to 6 to tolerate a trailing newline.
    if (( lines <= 6 )) && grep -q "PIPELN_60\b" "$out_file"; then
        assert_pass "/test capture | tail 5 keeps <=5 lines and includes PIPELN_60 (lines=$lines)"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "expected <=5 lines containing PIPELN_60, got lines=$lines"
        echo "    --- file contents ---"
        sed 's/^/    /' "$out_file"
        echo "    ----------------------"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 4: \r-driven progress bar (tqdm semantics) ───────────────────────
# Each iteration emits "\rProgress: NN/20" so every update lands on the same
# row. A naive byte-stream capture would show every intermediate (Progress 1,
# 2, 3, ...); a real terminal emulator shows only "Progress: 20/20".
test_4() {
    echo -e "\n${YELLOW}=== Test 4: carriage-return progress bar shows only final state ===${NC}"

    restart_client
    wait_for_client

    # Run a 20-step "progress bar" using printf '\r'. Sleep keeps each
    # update visible long enough to be flushed through the PTY individually
    # (so vt100 must collapse them rather than us getting lucky with batching).
    send_keys "for i in \$(seq 1 20); do printf '\\rProgress: %2d/20' \$i; sleep 0.05; done; printf '\\n'" 0.3
    send_enter 3

    enter_chat

    local out_file="/tmp/omnish_capprog_$$.txt"
    rm -f "$out_file"

    send_keys "/test capture 200 > $out_file" 0.3
    send_enter 2

    if ! _wait_for_file "$out_file" 5; then
        assert_fail "/test capture did not produce $out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    # The final state must appear as its own row. Anchoring matters: the
    # echoed command line contains the literal "Progress: %2d/20" pattern,
    # so an unanchored substring match would false-positive on
    # `printf '\rProgress: %2d/20'` (especially on CI where the long
    # hostname prompt wraps the command across multiple visible rows).
    if ! grep -qE "^Progress: 20/20\$" "$out_file"; then
        assert_fail "final 'Progress: 20/20' missing as a standalone row"
        echo "    --- file contents ---"
        sed 's/^/    /' "$out_file" | tail -20
        echo "    ----------------------"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    # Intermediate states must NOT appear as standalone rows (they were
    # overwritten by \r). Spot-check several middle values.
    local leaked=""
    local i
    for i in 1 5 10 15 19; do
        local pad
        pad="$(printf '%2d' $i)"
        if grep -qE "^Progress: ${pad}/20\$" "$out_file"; then
            leaked+="Progress: ${pad}/20; "
        fi
    done

    if [[ -z "$leaked" ]]; then
        assert_pass "carriage-return bar collapsed: no intermediate rows leaked"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "intermediates leaked: $leaked"
        echo "    --- file contents ---"
        sed 's/^/    /' "$out_file" | tail -25
        echo "    ----------------------"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 5: real tqdm progress bar ─────────────────────────────────────────
# Same goal as test 4 but using python's tqdm, which decorates with ANSI
# colors and unicode block glyphs. Skips when tqdm isn't installed in the
# CI image.
test_5() {
    echo -e "\n${YELLOW}=== Test 5: python tqdm progress bar shows only final state ===${NC}"

    if ! command -v python3 >/dev/null 2>&1 \
       || ! python3 -c "import tqdm" 2>/dev/null; then
        echo -e "  ${YELLOW}tqdm not available, skipping${NC}"
        assert_pass "tqdm test skipped (python3+tqdm not installed)"
        return 0
    fi

    restart_client
    wait_for_client

    # Run a tqdm loop. mininterval=0 so every iteration redraws (forces
    # the emulator to handle ALL the intermediate \r updates rather than
    # tqdm internally throttling them).
    send_keys "python3 -c \"import time,tqdm; [time.sleep(0.02) for _ in tqdm.tqdm(range(20), mininterval=0, ncols=40)]\"" 0.3
    send_enter 4

    enter_chat

    local out_file="/tmp/omnish_captqdm_$$.txt"
    rm -f "$out_file"

    send_keys "/test capture 200 > $out_file" 0.3
    send_enter 2

    if ! _wait_for_file "$out_file" 5; then
        assert_fail "/test capture did not produce $out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    # Final tqdm line shows "20/20" once it completes.
    if ! grep -q "20/20" "$out_file"; then
        assert_fail "final '20/20' missing from tqdm capture"
        echo "    --- file contents ---"
        sed 's/^/    /' "$out_file" | tail -25
        echo "    ----------------------"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    # tqdm prints "<n>/20" once per redraw; in raw bytes that's 20+ rows.
    # The emulator must collapse them to ONE row.
    local progress_rows
    progress_rows=$(grep -c "/20" "$out_file" || true)

    if (( progress_rows == 1 )); then
        assert_pass "tqdm progress bar collapsed to single final row (rows=$progress_rows)"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "expected 1 tqdm progress row, got $progress_rows"
        echo "    --- file contents ---"
        sed 's/^/    /' "$out_file" | tail -25
        echo "    ----------------------"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 6: ANSI clear-screen drops content above the clear ───────────────
# `clear` (or printf '\x1b[2J\x1b[H') wipes the visible region. After the
# clear we print BELOWCLEAR. Capturing the visible screen must not contain
# ABOVECLEAR (it was wiped).
test_6() {
    echo -e "\n${YELLOW}=== Test 6: ANSI clear wipes visible region ===${NC}"

    restart_client
    wait_for_client

    send_keys "echo ABOVECLEAR_$$" 0.3
    send_enter 0.5
    send_keys "clear" 0.3
    send_enter 0.5
    send_keys "echo BELOWCLEAR_$$" 0.3
    send_enter 1

    enter_chat

    local out_file="/tmp/omnish_capclear_$$.txt"
    rm -f "$out_file"

    # Visible-only capture (no N argument).
    send_keys "/test capture > $out_file" 0.3
    send_enter 1.5

    if ! _wait_for_file "$out_file" 5; then
        assert_fail "/test capture did not produce $out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    local has_above has_below
    has_above=$(grep -c "ABOVECLEAR_$$" "$out_file" || true)
    has_below=$(grep -c "BELOWCLEAR_$$" "$out_file" || true)

    if (( has_below >= 1 )) && (( has_above == 0 )); then
        assert_pass "clear wiped ABOVECLEAR; BELOWCLEAR remains visible"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "clear handling wrong: above=$has_above below=$has_below"
        echo "    --- file contents ---"
        sed 's/^/    /' "$out_file" | tail -20
        echo "    ----------------------"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

# ── Test 7: cursor-up + line-clear (in-place redraw) ──────────────────────
# Emit two rows, then in the SAME printf go cursor-up + erase-line + rewrite
# the second row. The emulator must replace the original second row in place
# (not append a new line). Doing both writes in one printf avoids the shell
# prompt landing between them, which would otherwise displace the cursor.
test_7() {
    echo -e "\n${YELLOW}=== Test 7: cursor-up + line-clear redraws in place ===${NC}"

    restart_client
    wait_for_client

    local tag="REDRAW$$"

    # printf sequence:
    #   KEEP_TAG\n         row 0: KEEP_TAG, cursor row 1
    #   OLD_TAG\n          row 1: OLD_TAG,  cursor row 2
    #   \x1b[1A             cursor up 1 -> row 1
    #   \x1b[2K             erase row 1
    #   NEW_TAG\n          row 1: NEW_TAG, cursor row 2
    # Final visible: KEEP_TAG, NEW_TAG. OLD_TAG must be gone.
    send_keys "printf 'KEEP_${tag}\\nOLD_${tag}\\n\\x1b[1A\\x1b[2KNEW_${tag}\\n'" 0.3
    send_enter 1

    enter_chat

    local out_file="/tmp/omnish_capredraw_$$.txt"
    rm -f "$out_file"

    send_keys "/test capture 100 > $out_file" 0.3
    send_enter 2

    if ! _wait_for_file "$out_file" 5; then
        assert_fail "/test capture did not produce $out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi

    local has_keep has_new has_old
    has_keep=$(grep -cE "^KEEP_${tag}\$" "$out_file" || true)
    has_new=$(grep -cE "^NEW_${tag}\$" "$out_file" || true)
    has_old=$(grep -cE "^OLD_${tag}\$" "$out_file" || true)

    if (( has_keep == 1 )) && (( has_new == 1 )) && (( has_old == 0 )); then
        assert_pass "in-place redraw: KEEP and NEW present, OLD overwritten"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 0
    else
        assert_fail "redraw wrong: KEEP=$has_keep NEW=$has_new OLD=$has_old (want 1/1/0)"
        echo "    --- file contents ---"
        sed 's/^/    /' "$out_file" | tail -20
        echo "    ----------------------"
        rm -f "$out_file"
        send_special Escape 0.5
        sleep 1.5
        return 1
    fi
}

echo -e "${YELLOW}Capture integration test: /test capture, /test capture N, pipe + redirect${NC}"
run_tests 7
