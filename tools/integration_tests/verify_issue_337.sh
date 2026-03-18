#!/usr/bin/env bash
#
# verify_issue_337.sh - Test picker widget rendering with pre-selected item
#
# Issue #337: When /model picker opens with a pre-selected item that causes
# scroll_offset > 0, pressing Up arrow renders duplicate items because the
# incremental redraw math assumes `vis` items are rendered but fewer actually are.
#
# Tests that:
#   1. Create a thread, switch model via /model + Down*N + Enter
#   2. Re-open /model (picker starts at selected model with possible scroll)
#   3. Press Up arrow — no duplicate items should appear

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Picker rendering with scrolled pre-selection (issue #337)
EOF
}

test_init "issue-337" "$@"

send_down_arrow() {
    local wait="${1:-0.3}"
    echo -e "  Sending: ${YELLOW}Down arrow${NC}"
    _tmux send-keys -t "$PANE" Down
    sleep "$wait"
}

send_up_arrow() {
    local wait="${1:-0.3}"
    echo -e "  Sending: ${YELLOW}Up arrow${NC}"
    _tmux send-keys -t "$PANE" Up
    sleep "$wait"
}

# Extract item lines from picker output.
# Matches lines that look like model items: "  name (model)" or "> name (model)"
_picker_items() {
    echo "$1" | sed 's/\x1b\[[0-9;]*m//g' | grep -E '^\s*[> ] \s*\S+.*\(.*\)\s*$'
}

# Check for duplicate lines in picker items.
# Returns 0 if duplicates found, 1 if all unique.
_has_duplicate_items() {
    local items="$1"
    local total uniq_count
    total=$(echo "$items" | wc -l)
    uniq_count=$(echo "$items" | sort -u | wc -l)
    [[ $total -ne $uniq_count ]]
}

# ── Test 1: Picker rendering with scrolled pre-selection ─────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Picker rendering with scrolled pre-selection (issue #337) ===${NC}"

    restart_client
    wait_for_client

    # Enter chat mode and send a message to create a thread
    echo -e "  ${YELLOW}--- Creating thread ---${NC}"
    send_keys ":" 0.5
    wait_for_prompt

    send_keys "Say OK." 0.3
    send_enter 0.3
    if ! wait_for_chat_response 30; then
        show_capture "After first message" "$(capture_pane -30)" 10
        assert_fail "No chat response for thread creation"
        return 1
    fi
    echo -e "  ${GREEN}Thread created${NC}"

    # ── Step 1: Open /model picker and select a model far down ──
    echo -e "  ${YELLOW}--- Step 1: Select model via /model + Down*8 ---${NC}"
    send_keys "/model" 0.3
    send_enter 1

    local picker1=$(capture_pane -30)
    local stripped1=$(echo "$picker1" | sed 's/\x1b\[[0-9;]*m//g')

    if ! echo "$stripped1" | grep -q "Select model:"; then
        show_capture "After /model" "$picker1" 15
        assert_fail "Model picker not displayed"
        return 1
    fi

    local items1=$(_picker_items "$picker1")
    local model_count=$(echo "$items1" | wc -l)
    echo -e "  Visible models in picker: $model_count"

    if [[ $model_count -lt 6 ]]; then
        echo -e "  ${YELLOW}Need at least 6 models to test scroll behavior, found $model_count${NC}"
        send_special Escape 0.5
        assert_pass "Skipped: not enough models to trigger scroll bug"
        return 0
    fi

    # Press Down 8 times to select a model far down the list
    for i in $(seq 1 8); do
        send_down_arrow 0.2
    done
    sleep 0.3

    # Confirm selection
    send_enter 1

    # ── Step 2: Re-open /model — picker should start at the selected model ──
    echo -e "  ${YELLOW}--- Step 2: Re-open /model (pre-selected at scrolled position) ---${NC}"
    send_keys "/model" 0.3
    send_enter 1

    local picker2=$(capture_pane -30)
    show_capture "Picker2 (pre-selected)" "$picker2" 15

    local stripped2=$(echo "$picker2" | sed 's/\x1b\[[0-9;]*m//g')
    if echo "$stripped2" | grep -q "▲.*more"; then
        echo -e "  ${GREEN}Scroll indicator (▲) detected — scroll scenario active${NC}"
    fi

    # Verify no duplicates in initial render
    local items2=$(_picker_items "$picker2")
    if _has_duplicate_items "$items2"; then
        echo -e "  Items:"
        echo "$items2" | sed 's/^/    /'
        assert_fail "Duplicate items in picker initial render"
        send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}No duplicates in initial render${NC}"

    # ── Step 3: Press Up arrow and check for duplicate rendering ──
    echo -e "  ${YELLOW}--- Step 3: Press Up arrows, check for duplicates ---${NC}"
    send_up_arrow 0.5

    local picker3=$(capture_pane -30)
    show_capture "After 1st Up" "$picker3" 15

    local items3=$(_picker_items "$picker3")
    if _has_duplicate_items "$items3"; then
        echo -e "  Items:"
        echo "$items3" | sed 's/^/    /'
        assert_fail "Duplicate items after 1st Up arrow (issue #337)"
        send_special Escape 0.5
        return 1
    fi

    # Press Up a few more times
    for i in $(seq 1 4); do
        send_up_arrow 0.3

        local picker_up=$(capture_pane -30)
        local items_up=$(_picker_items "$picker_up")
        if _has_duplicate_items "$items_up"; then
            show_capture "After Up #$((i+1))" "$picker_up" 15
            echo -e "  Items:"
            echo "$items_up" | sed 's/^/    /'
            assert_fail "Duplicate items after Up arrow #$((i+1))"
            send_special Escape 0.5
            return 1
        fi
    done

    # Cancel picker
    send_special Escape 0.5

    assert_pass "No duplicate items in picker with scrolled pre-selection"
    return 0
}

run_tests 1
