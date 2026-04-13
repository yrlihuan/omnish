#!/usr/bin/env bash
#
# test_config_backend.sh - Integration tests for LLM backend config management.
#
# Tests:
#   1. Add + Delete LLM backend: add via /config Add backend form, verify in
#      daemon.toml and menu, then open edit form, press Delete, verify removed.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Add + Delete LLM backend via /config
EOF
}

test_init "config-backend" "$@"

DAEMON_TOML="${OMNISH_HOME:-$HOME/.omnish}/daemon.toml"

# Helper: enter chat and open /config
open_config() {
    enter_chat
    send_keys "/config" 0.3
    send_enter 1
}

# Helper: navigate to LLM > Backends from Config top level.
# Config top: General, LLM, Tasks, Plugins, Sandbox → Down×1 + Enter for LLM
# LLM submenu: Use Cases, Backends → Down×1 + Enter for Backends
enter_backends() {
    send_special Down 0.3   # General → LLM
    send_enter 0.5
    send_special Down 0.3   # Use Cases → Backends
    send_enter 0.5
    sleep 0.3
}

# Helper: from inside the Backends submenu, navigate to the item matching
# <target> by scrolling to bottom, parsing visible items, and navigating Up.
# Returns 0 with cursor on the target item, 1 if not found.
navigate_to_menu_item() {
    local target="$1"

    # Scroll to bottom so target is likely visible
    for _ in $(seq 1 30); do
        send_special Down 0.1
    done
    sleep 0.3

    local content
    content=$(capture_pane -30)

    # Count items after target within the ─── separator-bracketed section
    local -a lines
    mapfile -t lines <<< "$content"

    local found_target=false
    local items_after=0
    local in_items=false

    for ((i = 0; i < ${#lines[@]}; i++)); do
        local line="${lines[$i]}"
        if [[ "$line" == *─────* ]]; then
            if $in_items; then break; fi
            in_items=true
            continue
        fi
        if ! $in_items; then continue; fi

        local trimmed
        trimmed=$(echo "$line" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
        [[ -z "$trimmed" ]] && continue

        if $found_target; then
            items_after=$((items_after + 1))
        fi

        if echo "$trimmed" | grep -q "$target"; then
            found_target=true
        fi
    done

    if ! $found_target; then
        echo -e "  ${RED}navigate_to_menu_item: '$target' not found in visible items${NC}"
        show_capture "Menu pane" "$content" 20
        return 1
    fi

    # Cursor is at last item after scrolling. Go Up to reach target.
    echo -e "  Navigating Up ${items_after} to reach '${target}'"
    for _ in $(seq 1 $items_after); do
        send_special Up 0.15
    done
    sleep 0.3
    return 0
}

# Cleanup: remove test backend section from daemon.toml if present
cleanup_test_backend() {
    if grep -q '\[llm\.backends\.test-del\]' "$DAEMON_TOML" 2>/dev/null; then
        echo -e "  ${YELLOW}Cleaning up test-del backend from daemon.toml${NC}"
        sed -i '/^\[llm\.backends\.test-del\]/,/^\[/{/^\[llm\.backends\.test-del\]/d;/^\[/!d;}' "$DAEMON_TOML"
    fi
}

# ── Test 1: Add + Delete LLM backend via /config ───────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Add + Delete LLM backend via /config ===${NC}"

    # Ensure clean state
    cleanup_test_backend

    restart_client
    wait_for_client

    # ── Step 1: Add test backend via /config Add backend form ──
    echo -e "  ${YELLOW}--- Step 1: Add test backend via /config ---${NC}"

    open_config
    enter_backends

    # "Add backend" is first item (alphabetically first). Enter to open form.
    send_enter 0.8
    sleep 0.5

    local content
    content=$(capture_pane -25)
    show_capture "Add backend form" "$content" 15

    if ! echo "$content" | grep -q "Add backend"; then
        assert_fail "Step 1: not inside Add backend form"
        send_special Escape 0.5; send_special Escape 0.5; send_special Escape 0.5
        return 1
    fi

    # No auto-edit in this form (Provider has presets → has_prefilled=true).
    # Form: Provider(0) Name(1) Backend type(2) Model(3) API key(4)
    #        Base URL(5) Use proxy(6) Context window(7) Done(8)

    # Provider (Select, cursor here): open picker, pick first preset (non-custom).
    # Preset fills Name/Backend type/Model/etc. so we only need to change Name.
    send_enter 0.5          # open picker
    send_special Down 0.3   # move past "custom" to first real preset
    send_enter 0.5          # confirm → prefills applied

    # Navigate to Name (Down 1 from Provider)
    send_special Down 0.3

    # Edit Name: clear prefilled value, type "test-del"
    send_enter 0.5
    for _ in $(seq 1 30); do send_special BSpace 0.05; done
    send_keys "test-del" 0.3
    send_enter 0.5

    # Navigate all the way down to Done button (Down 20 overshoots but stops at last item)
    for i in $(seq 1 20); do
        send_special Down 0.1
    done
    sleep 0.3

    content=$(capture_pane -25)
    show_capture "Add form (filled)" "$content" 15

    # Cursor on Done button → Enter to submit
    send_enter 1.5

    sleep 1
    if grep -q '\[llm\.backends\.test-del\]' "$DAEMON_TOML"; then
        assert_pass "Step 1: test backend added to daemon.toml"
    else
        show_capture "daemon.toml" "$(cat "$DAEMON_TOML")" 30
        assert_fail "Step 1: test backend not found in daemon.toml"
        return 1
    fi

    # ── Step 2: Navigate to test-del edit form and press Delete ──
    echo -e "  ${YELLOW}--- Step 2: Navigate to test-del edit form and delete ---${NC}"

    # After handler fires, menu returns to Config root. Re-navigate to Backends.
    sleep 0.5
    enter_backends

    if ! navigate_to_menu_item "test-del"; then
        assert_fail "Step 2: could not find test-del in Backends submenu"
        send_special Escape 0.5; send_special Escape 0.5; send_special Escape 0.5; send_special Escape 0.5
        cleanup_test_backend
        return 1
    fi

    # Enter the edit form
    send_enter 0.8

    content=$(capture_pane -25)
    show_capture "Backend edit form" "$content" 15

    # Verify we entered the correct backend form
    if ! echo "$content" | grep -q "test-del"; then
        assert_fail "Step 2: entered wrong backend (expected test-del)"
        send_special Escape 0.5; send_special Escape 0.5; send_special Escape 0.5; send_special Escape 0.5
        cleanup_test_backend
        return 1
    fi

    # Edit form fields (cursor starts at Backend type):
    #   Backend type → Model → API key command → Base URL → Use proxy → Context window → Delete
    # Navigate Down × 6 to reach Delete button
    for i in $(seq 1 6); do
        send_special Down 0.3
    done

    content=$(capture_pane -25)
    show_capture "At Delete button" "$content" 15

    # Press Enter to trigger Delete
    send_enter 1.0
    sleep 1

    # ── Step 3: Verify backend removed from daemon.toml ──
    echo -e "  ${YELLOW}--- Step 3: Verify backend removed from daemon.toml ---${NC}"

    if grep -q '\[llm\.backends\.test-del\]' "$DAEMON_TOML"; then
        assert_fail "Step 3: test backend still present in daemon.toml after delete"
        show_capture "daemon.toml" "$(cat "$DAEMON_TOML")" 30
        cleanup_test_backend
        return 1
    fi

    assert_pass "Step 3: backend removed from daemon.toml"

    # ── Step 4: Verify backend no longer appears in menu ──
    echo -e "  ${YELLOW}--- Step 4: Verify backend gone from menu ---${NC}"

    # After handler fires, menu rebuilds. Scroll through to check.
    sleep 0.5
    for i in $(seq 1 20); do
        send_special Down 0.15
    done
    sleep 0.3

    content=$(capture_pane -30)
    show_capture "Menu after delete" "$content" 15

    if echo "$content" | grep -q "test-del"; then
        assert_fail "Step 4: test backend still visible in menu after delete"
        return 1
    fi

    assert_pass "Step 4: backend no longer in menu"
    return 0
}

echo -e "${YELLOW}LLM backend config management integration tests${NC}"
run_tests 1
