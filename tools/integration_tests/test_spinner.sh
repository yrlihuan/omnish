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
# The earlier pipe-pane-based version only checked whether "Thinking…" was
# ever written to the byte stream. That missed the real failure mode where
# `show_thinking` writes the line but a subsequent handler erases it within
# a few ms — the user never actually sees it.
#
# This version polls the pane with `capture-pane` at 50ms intervals and
# requires AT LEAST ONE snapshot taken AFTER the Bash tool header to still
# contain an active "Thinking…" line. The prompt forces a tool with a
# large output (`cat /etc/services && seq 1 2000`) so the post-tool
# first-token latency is several seconds, giving capture-pane many chances
# to observe the Thinking line if it is truly visible.
test_3() {
    echo -e "\n${YELLOW}=== Test 3: Thinking after tool-use, before response (#551) ===${NC}"

    restart_client
    wait_for_client

    enter_chat

    local snaps_dir
    snaps_dir=$(mktemp -d /tmp/omnish-test-thinking-snaps.XXXXXX)

    # Background high-frequency capture-pane poller, one file per snapshot.
    (
        local i=0
        while :; do
            _tmux capture-pane -p -J -t "$PANE" -S -30 \
                > "$snaps_dir/$(printf '%05d' "$i").txt" 2>/dev/null || true
            i=$((i+1))
            sleep 0.05
        done
    ) &
    local poll_pid=$!

    # Prompt forces one Bash tool whose output is large enough that the
    # LLM's first-token latency after the tool result is several seconds —
    # well over the 50ms capture interval.
    local prompt="请运行 bash 命令：cat /etc/services && seq 1 2000 。然后简短总结输出中出现的端口/数字规律，再运行 echo done。"
    send_keys "$prompt" 0.3
    send_enter 0.1

    # Use 300s: big tool output + long LLM processing + possible 429 retry.
    if ! wait_for_chat_response 300; then
        kill $poll_pid 2>/dev/null || true
        wait $poll_pid 2>/dev/null || true
        show_capture "No chat response" "$(capture_pane -40)" 30
        assert_fail "No chat response received after tool-use"
        rm -rf "$snaps_dir"
        return 1
    fi
    kill $poll_pid 2>/dev/null || true
    wait $poll_pid 2>/dev/null || true

    # Walk snapshots in order. Anchor "post-tool" as the range AFTER the
    # first snapshot that contains a Bash tool header. Count how many of
    # those still contain a "<spinner> Thinking" line, and how many
    # distinct spinner codepoints show up there (animation evidence).
    local first_bash=-1
    local pre_tool_thinking=0
    local post_tool_thinking=0
    local distinct_frames=""
    local idx=0
    for f in "$snaps_dir"/*.txt; do
        local stripped
        stripped=$(sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g' "$f")
        if [[ $first_bash -eq -1 ]] && echo "$stripped" | grep -qE 'Bash\('; then
            first_bash=$idx
        fi
        if echo "$stripped" | grep -qE "[$SPINNER_CHARS] Thinking"; then
            if [[ $first_bash -eq -1 ]]; then
                pre_tool_thinking=$((pre_tool_thinking + 1))
            else
                post_tool_thinking=$((post_tool_thinking + 1))
                local ch
                ch=$(echo "$stripped" | grep -oE "[$SPINNER_CHARS] Thinking" \
                    | grep -oE "[$SPINNER_CHARS]" | head -1)
                if [[ -n "$ch" && "$distinct_frames" != *"$ch"* ]]; then
                    distinct_frames+="$ch"
                fi
            fi
        fi
        idx=$((idx + 1))
    done

    echo -e "  Total snapshots:          $idx"
    echo -e "  First Bash-header snap:   $first_bash"
    echo -e "  Pre-tool  Thinking snaps: $pre_tool_thinking"
    echo -e "  Post-tool Thinking snaps: $post_tool_thinking"
    echo -e "  Distinct post-tool spinner codepoints: ${#distinct_frames} ('$distinct_frames')"

    local failed=0

    if [[ $first_bash -lt 0 ]]; then
        assert_fail "No Bash tool header ever appeared"
        failed=1
    elif [[ $post_tool_thinking -lt 1 ]]; then
        assert_fail "No post-tool capture-pane snapshot contained Thinking (indicator not visible to the user between tool completion and LLM response)"
        failed=1
    else
        assert_pass "Thinking visible after Bash tool header in $post_tool_thinking snapshot(s)"
    fi

    # Final pane must not still show a Thinking line.
    local final
    final=$(capture_pane -40)
    if echo "$final" | grep -q 'Thinking'; then
        show_capture "Thinking still visible" "$final" 30
        assert_fail "Thinking label not erased after final response"
        failed=1
    else
        assert_pass "Thinking erased after tool-use response"
    fi

    if [[ $failed -eq 0 ]]; then
        rm -rf "$snaps_dir"
    else
        echo -e "  ${YELLOW}Snapshot dir preserved for debugging: $snaps_dir${NC}"
    fi

    return $failed
}

echo -e "${YELLOW}Spinner animation integration tests (#478, #551)${NC}"
run_tests 3
