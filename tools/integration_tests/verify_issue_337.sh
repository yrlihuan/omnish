#!/usr/bin/env bash
#
# verify_issue_337.sh - Test picker widget rendering with pre-selected item
#
# Issue #337: When /model picker opens with a pre-selected item that causes
# scroll_offset > 0, pressing Up arrow renders duplicate items because the
# incremental redraw math assumes `vis` items are rendered but fewer actually are.
#
# Uses /test picker [N] (20 deterministic items, no LLM config dependency).
#
# Tests that:
#   1. Open /test picker 8 (pre-selected at scrolled position)
#   2. Press Up arrow - no duplicate items should appear

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
# Matches lines that look like picker items: "  name (model)" or "> name (model)"
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

    # Enter chat mode
    echo -e "  ${YELLOW}--- Entering chat mode ---${NC}"
    send_keys ":" 0.5
    wait_for_prompt

    # ── Step 1: Open /test picker with pre-selected index 8 (triggers scroll) ──
    echo -e "  ${YELLOW}--- Step 1: Open picker pre-selected at index 8 ---${NC}"
    send_keys "/test picker 8" 0.3
    send_enter 1

    local picker1=$(capture_pane -30)
    local stripped1=$(echo "$picker1" | sed 's/\x1b\[[0-9;]*m//g')

    if ! echo "$stripped1" | grep -q "Select model:"; then
        show_capture "After /test picker" "$picker1" 15
        assert_fail "Test picker not displayed"
        return 1
    fi
    show_capture "Picker (pre-selected at 8)" "$picker1" 15

    if echo "$stripped1" | grep -q "▲.*more"; then
        echo -e "  ${GREEN}Scroll indicator (▲) detected - scroll scenario active${NC}"
    fi

    # Verify no duplicates in initial render
    local items1=$(_picker_items "$picker1")
    if _has_duplicate_items "$items1"; then
        echo -e "  Items:"
        echo "$items1" | sed 's/^/    /'
        assert_fail "Duplicate items in picker initial render"
        send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}No duplicates in initial render${NC}"

    # ── Step 2: Press Up arrow and check for duplicate rendering ──
    echo -e "  ${YELLOW}--- Step 2: Press Up arrows, check for duplicates ---${NC}"
    send_up_arrow 0.5

    local picker2=$(capture_pane -30)
    show_capture "After 1st Up" "$picker2" 15

    local items2=$(_picker_items "$picker2")
    if _has_duplicate_items "$items2"; then
        echo -e "  Items:"
        echo "$items2" | sed 's/^/    /'
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
