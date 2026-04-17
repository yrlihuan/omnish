#!/usr/bin/env bash
#
# test_spinner.sh - Integration tests for chat-mode spinner animations
#
# Test cases:
#   1. Running tool shows animated spinner that changes over time (#478)
#   2. Thinking animation after sending a chat message (#551)
#   3. Thinking animation between tool completion and LLM response (#551)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Running tool shows animated spinner that changes between captures (#478)
  2. Thinking animation after sending a chat message (#551)
  3. Thinking animation between tool completion and LLM response (#551)
EOF
}

test_init "spinner" "$@"

SPINNER_CHARS='⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏'

# capture_thinking_frame
#   Returns the spinner character currently on the "Thinking" line, or "".
capture_thinking_frame() {
    local content
    content=$(capture_pane -20)
    local line
    line=$(echo "$content" | grep -m1 'Thinking' || true)
    [[ -z "$line" ]] && { echo ""; return; }
    echo "$line" | grep -oE "[$SPINNER_CHARS]" | head -1
}

# ── Test 1: Spinner animation during tool execution (#478) ──────────────
# When a tool is running, the status icon should be an animated spinner
# (braille characters cycling). We verify by capturing the pane at two
# different times and checking that the icon character changes.
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Spinner animation during tool execution (#478) ===${NC}"

    restart_client
    wait_for_client

    enter_chat

    # Ask LLM to run sleep 10
    send_keys "运行 sleep 10" 0.3
    send_enter 0.3

    # Wait for the tool status line to appear (Bash tool header with spinner)
    local waited=0
    local content=""
    while [[ $waited -lt 30 ]]; do
        content=$(capture_pane -20)
        if echo "$content" | grep -qE "[$SPINNER_CHARS].*Bash"; then
            break
        fi
        sleep 0.5
        waited=$((waited + 1))
    done

    if [[ $waited -ge 30 ]]; then
        show_capture "Pane content (no spinner found)" "$content" 20
        assert_fail "No spinner character found in tool header within 15s"
        return 1
    fi

    echo -e "  Spinner detected, capturing two frames..."

    # Capture first frame (from the Bash line to avoid picking up the
    # Thinking spinner from other tests).
    local frame1
    frame1=$(capture_pane -20 | grep -E 'Bash' | grep -oE "[$SPINNER_CHARS]" | head -1)
    echo -e "  Frame 1: '$frame1'"

    # Wait for spinner to advance (> 200ms interval, use 1s to be safe)
    sleep 1

    # Capture second frame
    local frame2
    frame2=$(capture_pane -20 | grep -E 'Bash' | grep -oE "[$SPINNER_CHARS]" | head -1)
    echo -e "  Frame 2: '$frame2'"

    if [[ -n "$frame1" && -n "$frame2" && "$frame1" != "$frame2" ]]; then
        assert_pass "Spinner animated: '$frame1' → '$frame2'"
    elif [[ -n "$frame1" && -n "$frame2" ]]; then
        # Same frame captured — try once more with longer wait
        sleep 1.5
        local frame3
        frame3=$(capture_pane -20 | grep -E 'Bash' | grep -oE "[$SPINNER_CHARS]" | head -1)
        echo -e "  Frame 3: '$frame3'"
        if [[ "$frame1" != "$frame3" || "$frame2" != "$frame3" ]]; then
            assert_pass "Spinner animated (3 captures): '$frame1' → '$frame2' → '$frame3'"
        else
            assert_fail "Spinner not animating: all frames identical ('$frame1')"
            return 1
        fi
    else
        assert_fail "Could not capture spinner frames (frame1='$frame1', frame2='$frame2')"
        return 1
    fi

    # Wait for sleep to finish and LLM to respond.
    # Use 120s to tolerate one 429 backoff retry (60s) + response.
    if ! wait_for_chat_response 120; then
        show_capture "After sleep" "$(capture_pane -20)" 20
        assert_fail "No chat response after sleep 10"
        return 1
    fi

    # After completion, spinner should be replaced with a static icon (●)
    content=$(capture_pane -30)
    local stripped
    stripped=$(echo "$content" | sed 's/\x1b\[[0-9;]*m//g')

    # Check that the Bash tool header now has ● (success) instead of spinner
    if echo "$stripped" | grep -qE '● Bash\('; then
        assert_pass "Tool completed with static ● icon"
    else
        show_capture "Final state" "$content" 20
        # Not a hard failure — the tool section may have been cleared
        echo -e "  ${YELLOW}Note: Could not verify final static icon (section may be cleared)${NC}"
    fi

    return 0
}

# ── Test 2: Thinking animation after sending a chat message (#551) ──────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Thinking animation on new chat message (#551) ===${NC}"

    restart_client
    wait_for_client

    enter_chat

    # Simple query; LLM will think for a bit before streaming anything back.
    send_keys "用一句话介绍你自己" 0.3
    send_enter 0.1

    # Poll for Thinking label to appear (up to ~2s).
    local waited=0
    local found=false
    while [[ $waited -lt 10 ]]; do
        local content
        content=$(capture_pane -20)
        if echo "$content" | grep -q 'Thinking'; then
            found=true
            break
        fi
        sleep 0.2
        waited=$((waited + 1))
    done

    if [[ "$found" != "true" ]]; then
        show_capture "Pane content (no Thinking found)" "$(capture_pane -20)" 20
        assert_fail "Thinking label did not appear after sending message"
        return 1
    fi

    # Capture two frames separated by >200ms to verify animation.
    local frame1
    frame1=$(capture_thinking_frame)
    echo -e "  Frame 1: '$frame1'"

    sleep 0.6
    local frame2
    frame2=$(capture_thinking_frame)
    echo -e "  Frame 2: '$frame2'"

    if [[ -z "$frame1" || -z "$frame2" ]]; then
        show_capture "Pane content" "$(capture_pane -20)" 20
        assert_fail "Could not capture spinner frames on Thinking line"
        return 1
    fi

    if [[ "$frame1" == "$frame2" ]]; then
        sleep 0.8
        local frame3
        frame3=$(capture_thinking_frame)
        echo -e "  Frame 3: '$frame3'"
        if [[ -n "$frame3" && "$frame3" != "$frame1" ]]; then
            assert_pass "Thinking spinner animated: '$frame1' → '$frame3'"
        else
            assert_fail "Thinking spinner not animating: '$frame1' == '$frame2' == '$frame3'"
            return 1
        fi
    else
        assert_pass "Thinking spinner animated: '$frame1' → '$frame2'"
    fi

    # Wait for response to complete; Thinking must be erased afterwards.
    # Use 120s to tolerate one 429 backoff retry (60s) + response.
    if ! wait_for_chat_response 120; then
        show_capture "No chat response" "$(capture_pane -20)" 20
        assert_fail "No chat response received"
        return 1
    fi

    local final
    final=$(capture_pane -30)
    if echo "$final" | grep -q 'Thinking'; then
        show_capture "Thinking still visible" "$final" 20
        assert_fail "Thinking label not erased after response"
        return 1
    fi
    assert_pass "Thinking erased after LLM response"

    return 0
}

# ── Test 3: Thinking between tool completion and response (#551) ────────
# The post-tool Thinking window is typically <1s (Claude's first-token
# latency after a tool result). Periodic capture-pane snapshots can miss
# it, so we use `tmux pipe-pane` to log the raw byte stream — capturing
# every character ever written to the pane including transient text that
# gets overwritten. We then scan the log for animated spinner frames next
# to "Thinking…" occurring AFTER the Bash tool header.
test_3() {
    echo -e "\n${YELLOW}=== Test 3: Thinking after tool-use, before response (#551) ===${NC}"

    restart_client
    wait_for_client

    enter_chat

    # Start capturing raw pane output to a log file.
    local pipe_log
    pipe_log=$(mktemp /tmp/omnish-test-pipe.XXXXXX.log)
    _tmux pipe-pane -t "$PANE" -o "cat >> $pipe_log"

    # Simple prompt that forces exactly one tool call.
    send_keys "运行 echo hello" 0.3
    send_enter 0.1

    # Wait for the final chat prompt to return (response complete).
    # Use 120s to tolerate one 429 backoff retry (60s) + response.
    if ! wait_for_chat_response 120; then
        _tmux pipe-pane -t "$PANE"
        show_capture "No chat response" "$(capture_pane -40)" 30
        assert_fail "No chat response received after tool-use"
        rm -f "$pipe_log"
        return 1
    fi

    # Stop capturing.
    _tmux pipe-pane -t "$PANE"

    # Strip ANSI escapes to simplify pattern matching, but keep line
    # structure so we can reason about order.
    local stripped_log
    stripped_log=$(mktemp /tmp/omnish-test-stripped.XXXXXX.log)
    sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g' "$pipe_log" > "$stripped_log"

    # Count occurrences of "⣿ Thinking…" patterns (any of the 10 braille
    # spinner frames + " Thinking"). Each redraw_thinking() prints a new
    # frame, so we expect multiple distinct frames across the session.
    local thinking_hits
    thinking_hits=$(grep -oE "[$SPINNER_CHARS] Thinking" "$stripped_log" | wc -l)
    echo -e "  Thinking spinner writes captured: $thinking_hits"

    # Count distinct spinner frames next to Thinking (animation evidence).
    local distinct_frames
    distinct_frames=$(grep -oE "[$SPINNER_CHARS] Thinking" "$stripped_log" \
        | grep -oE "[$SPINNER_CHARS]" | sort -u | wc -l)
    echo -e "  Distinct spinner frames: $distinct_frames"

    # Check that at least one "Thinking" write occurs AFTER a Bash tool
    # header in the log — this is the post-tool window.
    local post_tool_thinking=false
    if awk -v chars="$SPINNER_CHARS" '
        BEGIN { saw_bash = 0; found = 0 }
        /Bash\(/ { saw_bash = 1 }
        saw_bash {
            # match any char from SPINNER_CHARS followed by " Thinking"
            for (i = 1; i <= length(chars); i++) {
                c = substr(chars, i, 1)
                if (index($0, c " Thinking") > 0) { found = 1; exit }
            }
        }
        END { exit !found }
    ' "$stripped_log"; then
        post_tool_thinking=true
    fi

    rm -f "$pipe_log" "$stripped_log"

    if [[ $thinking_hits -lt 2 ]]; then
        assert_fail "Expected ≥2 Thinking spinner writes (initial + post-tool), got $thinking_hits"
        return 1
    fi
    if [[ $distinct_frames -lt 2 ]]; then
        assert_fail "Spinner did not animate: only $distinct_frames distinct frame(s)"
        return 1
    fi
    if [[ "$post_tool_thinking" != "true" ]]; then
        assert_fail "No Thinking spinner observed after Bash tool header"
        return 1
    fi

    assert_pass "Thinking spinner animates after tool-use (hits=$thinking_hits, frames=$distinct_frames)"

    # Final pane must not still show a Thinking line.
    local final
    final=$(capture_pane -40)
    if echo "$final" | grep -q 'Thinking'; then
        show_capture "Thinking still visible" "$final" 30
        assert_fail "Thinking label not erased after final response"
        return 1
    fi
    assert_pass "Thinking erased after tool-use response"

    return 0
}

echo -e "${YELLOW}Spinner animation integration tests (#478, #551)${NC}"
run_tests 3
