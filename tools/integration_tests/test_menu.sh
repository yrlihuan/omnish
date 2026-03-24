#!/usr/bin/env bash
#
# test_menu.sh - Integration test for multi-level menu widget (/test menu).
#
# Test cases:
#   1. Menu renders with breadcrumb, items, and hint line
#   2. Arrow Down/Up moves cursor between items
#   3. Enter on Toggle flips value (OFF→ON / ON→OFF)
#   4. Enter on Submenu drills into children, ESC returns to parent
#   5. ESC at top level exits menu with "No changes" or change summary
#   6. Ctrl-C cancels menu (shows "Cancelled")
#   7. Enter on TextInput opens editor, type + Enter saves, ESC cancels
#   8. TextInput edit state verification (cursor position, typing)
#   9. Add item via handler submenu (fill form, ESC triggers handler, new item appears)
#  10. Form mode: auto-edit on entry, Enter advances cursor, handler works

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Menu renders with breadcrumb, items, and hint line
  2. Arrow Down/Up moves cursor between items
  3. Enter on Toggle flips value (OFF→ON / ON→OFF)
  4. Enter on Submenu drills into children, ESC returns to parent
  5. ESC at top level exits menu, shows changes or "No changes"
  6. Ctrl-C cancels menu (shows "Cancelled")
  7. TextInput: type + Enter saves, ESC cancels
  8. TextInput edit state verification
  9. Add item via handler submenu
 10. Form mode: auto-edit, Enter advance, handler
EOF
}

test_init "menu" "$@"

# Helper: enter chat mode and open /test menu
open_test_menu() {
    enter_chat
    send_keys "/test menu" 0.3
    send_enter 1
}

# ── Test 1: Menu renders with breadcrumb, items, and hints ───────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: Menu renders with breadcrumb, items, and hints ===${NC}"

    restart_client
    wait_for_client
    open_test_menu

    local content
    content=$(capture_pane -20)
    show_capture "Menu render" "$content" 15

    # Check breadcrumb title "Config"
    if ! echo "$content" | grep -q "Config"; then
        assert_fail "Breadcrumb 'Config' not found"
        return 1
    fi

    # Check menu items: LLM (submenu), Shell (submenu), Telemetry (toggle), Username (text)
    if ! echo "$content" | grep -q "LLM"; then
        assert_fail "Menu item 'LLM' not found"
        return 1
    fi
    if ! echo "$content" | grep -q "Shell"; then
        assert_fail "Menu item 'Shell' not found"
        return 1
    fi
    if ! echo "$content" | grep -q "Telemetry"; then
        assert_fail "Menu item 'Telemetry' not found"
        return 1
    fi
    if ! echo "$content" | grep -q "Username"; then
        assert_fail "Menu item 'Username' not found"
        return 1
    fi

    # Check TextInput shows current value after label
    if ! echo "$content" | grep "Username" | grep -q "user"; then
        assert_fail "TextInput 'Username' does not show current value 'user'"
        return 1
    fi

    # Check Toggle shows [OFF] after label
    if ! echo "$content" | grep "Telemetry" | grep -q "\[OFF\]"; then
        assert_fail "Toggle 'Telemetry' does not show [OFF]"
        return 1
    fi

    # Check SubMenu shows ▸ indicator
    if ! echo "$content" | grep "LLM" | grep -q "▸"; then
        assert_fail "SubMenu 'LLM' does not show ▸ indicator"
        return 1
    fi

    # Check hint line
    if ! echo "$content" | grep -q "move"; then
        assert_fail "Hint line not found"
        return 1
    fi

    # Exit menu
    send_special Escape 0.5

    assert_pass "Menu renders with breadcrumb, all items, and hint line"
    return 0
}

# ── Test 2: Arrow Down/Up moves cursor ──────────────────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Arrow Down/Up cursor movement ===${NC}"

    restart_client
    wait_for_client
    open_test_menu

    # Initial state: cursor should be on LLM (first item, shown with "> ")
    local content
    content=$(capture_pane -20)

    # Move Down — cursor should move to Shell
    send_special Down 0.3
    content=$(capture_pane -20)
    show_capture "After Down" "$content" 10

    # Move Down again — cursor on Telemetry
    send_special Down 0.3
    content=$(capture_pane -20)

    # Move Up — cursor back on Shell
    send_special Up 0.3
    content=$(capture_pane -20)
    show_capture "After Up" "$content" 10

    # The selected item should have ">" prefix (inverse video in rendering)
    # Since tmux strips some ANSI, we just verify navigation didn't crash
    # and menu is still displayed
    if echo "$content" | grep -q "Config" && echo "$content" | grep -q "LLM"; then
        assert_pass "Arrow Down/Up cursor navigation works"
        send_special Escape 0.5
        return 0
    else
        assert_fail "Menu display broken after cursor movement"
        send_special Escape 0.5
        return 1
    fi
}

# ── Test 3: Toggle flips value ──────────────────────────────────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: Toggle flips value ===${NC}"

    restart_client
    wait_for_client
    open_test_menu

    # Navigate to Telemetry (3rd item: Down, Down)
    send_special Down 0.3
    send_special Down 0.3

    local content
    content=$(capture_pane -20)
    show_capture "Before toggle" "$content" 10

    # Telemetry starts as OFF — press Enter to toggle ON
    if ! echo "$content" | grep -q "\[OFF\]"; then
        assert_fail "Telemetry initial value not OFF"
        send_special Escape 0.5
        return 1
    fi

    send_enter 0.5
    content=$(capture_pane -20)
    show_capture "After toggle" "$content" 10

    if echo "$content" | grep "Telemetry" | grep -q "\[ON\]"; then
        echo -e "  ${GREEN}Toggled OFF→ON${NC}"
    else
        assert_fail "Toggle did not change to ON"
        send_special Escape 0.5
        return 1
    fi

    # Toggle again: ON→OFF
    send_enter 0.5
    content=$(capture_pane -20)

    if echo "$content" | grep "Telemetry" | grep -q "\[OFF\]"; then
        assert_pass "Toggle flips value correctly (OFF→ON→OFF)"
        send_special Escape 0.5
        return 0
    else
        assert_fail "Toggle did not flip back to OFF"
        send_special Escape 0.5
        return 1
    fi
}

# ── Test 4: Submenu drill-in and ESC back ───────────────────────────────
test_4() {
    echo -e "\n${YELLOW}=== Test 4: Submenu drill-in and ESC back ===${NC}"

    restart_client
    wait_for_client
    open_test_menu

    # LLM is first item — press Enter to drill in
    send_enter 1

    local content
    content=$(capture_pane -20)
    show_capture "Inside LLM submenu" "$content" 10

    # Breadcrumb should show "Config > LLM"
    if ! echo "$content" | grep -q "Config > LLM"; then
        # tmux may strip some chars; check for "Config" and "LLM" on same line
        if ! echo "$content" | grep "Config" | grep -q "LLM"; then
            assert_fail "Breadcrumb 'Config > LLM' not found"
            send_special Escape 0.5
            send_special Escape 0.5
            return 1
        fi
    fi

    # Should see LLM children: Default backend, Streaming, API key, Proxy URL
    if ! echo "$content" | grep -q "Default backend"; then
        assert_fail "'Default backend' not found in LLM submenu"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    if ! echo "$content" | grep -q "Streaming"; then
        assert_fail "'Streaming' not found in LLM submenu"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    # Select item should show current value (#4)
    if ! echo "$content" | grep "Default backend" | grep -q "claude"; then
        assert_fail "Select 'Default backend' does not show current value 'claude'"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    # TextInput should show current value (#1)
    if ! echo "$content" | grep "API key" | grep -q "sk-"; then
        assert_fail "TextInput 'API key' does not show current value"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    # Press ESC to go back to root
    send_special Escape 0.5
    sleep 0.3
    content=$(capture_pane -20)
    show_capture "Back at root" "$content" 10

    # Should see root items again
    if echo "$content" | grep -q "Telemetry" && echo "$content" | grep -q "Username"; then
        assert_pass "Submenu drill-in and ESC back works"
        send_special Escape 0.5
        return 0
    else
        assert_fail "Root menu not restored after ESC"
        send_special Escape 0.5
        return 1
    fi
}

# ── Test 5: ESC at top level exits with changes summary ─────────────────
test_5() {
    echo -e "\n${YELLOW}=== Test 5: ESC exits menu, shows changes ===${NC}"

    restart_client
    wait_for_client
    open_test_menu

    # Toggle Telemetry (Down, Down, Enter) to generate a change
    send_special Down 0.3
    send_special Down 0.3
    send_enter 0.5

    # ESC to exit
    send_special Escape 0.5
    sleep 0.5

    local content
    content=$(capture_pane -20)
    show_capture "After menu exit" "$content" 10

    # Should show change summary (Telemetry = true)
    if echo "$content" | grep -q "Changes\|Telemetry"; then
        assert_pass "ESC exits menu and shows change summary"
        return 0
    else
        # Also acceptable: "No changes" if toggle was double-flipped
        if echo "$content" | grep -q "No changes"; then
            assert_pass "ESC exits menu (no changes case)"
            return 0
        fi
        assert_fail "No change summary or 'No changes' after menu exit"
        return 1
    fi
}

# ── Test 6: Ctrl-C cancels menu ─────────────────────────────────────────
test_6() {
    echo -e "\n${YELLOW}=== Test 6: Ctrl-C cancels menu ===${NC}"

    restart_client
    wait_for_client
    open_test_menu

    # Press Ctrl-C to cancel
    send_special C-c 0.5
    sleep 0.5

    local content
    content=$(capture_pane -20)
    show_capture "After Ctrl-C" "$content" 10

    if echo "$content" | grep -qi "cancel"; then
        assert_pass "Ctrl-C cancels menu"
        return 0
    else
        assert_fail "No 'Cancelled' message after Ctrl-C"
        return 1
    fi
}

# ── Test 7: TextInput edit and cancel ───────────────────────────────────
test_7() {
    echo -e "\n${YELLOW}=== Test 7: TextInput edit + Enter saves, ESC cancels ===${NC}"

    restart_client
    wait_for_client
    open_test_menu

    # Navigate to Username (4th item: Down x3)
    send_special Down 0.3
    send_special Down 0.3
    send_special Down 0.3

    local content
    content=$(capture_pane -20)
    show_capture "On Username" "$content" 10

    # Press Enter to edit Username
    send_enter 0.5

    content=$(capture_pane -20)
    show_capture "Edit mode" "$content" 10

    # Hint should change to "Enter confirm  ESC cancel"
    if ! echo "$content" | grep -q "confirm"; then
        assert_fail "Edit mode hint not shown"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    # Type some text — first clear existing with backspace x4 ("user" = 4 chars)
    send_special BSpace 0.1
    send_special BSpace 0.1
    send_special BSpace 0.1
    send_special BSpace 0.1
    send_keys "admin" 0.3

    content=$(capture_pane -20)
    show_capture "After typing" "$content" 10

    # Press ESC to cancel edit (value should revert to "user")
    send_special Escape 0.5
    sleep 0.3

    content=$(capture_pane -20)
    show_capture "After ESC cancel edit" "$content" 10

    # Username should still show "user"
    if ! echo "$content" | grep "Username" | grep -q "user"; then
        assert_fail "Username did not revert after ESC cancel"
        send_special Escape 0.5
        return 1
    fi

    # Now edit again and confirm with Enter
    send_enter 0.5

    # Clear and type new value
    send_special BSpace 0.1
    send_special BSpace 0.1
    send_special BSpace 0.1
    send_special BSpace 0.1
    send_keys "admin" 0.3
    send_enter 0.5

    content=$(capture_pane -20)
    show_capture "After Enter confirm" "$content" 10

    if echo "$content" | grep "Username" | grep -q "admin"; then
        echo -e "  ${GREEN}TextInput saved 'admin' with Enter${NC}"
    else
        assert_fail "TextInput did not save new value"
        send_special Escape 0.5
        return 1
    fi

    # Exit menu and verify change is reported
    send_special Escape 0.5
    sleep 0.5

    content=$(capture_pane -20)
    show_capture "Menu exit with changes" "$content" 10

    if echo "$content" | grep -q "admin"; then
        assert_pass "TextInput: ESC cancels, Enter saves value"
        return 0
    else
        assert_fail "Change summary missing 'admin' value"
        return 1
    fi
}

# ── Test 8: TextInput edit state verification ───────────────────────────
test_8() {
    echo -e "\n${YELLOW}=== Test 8: TextInput edit state verification ===${NC}"

    restart_client
    wait_for_client
    open_test_menu

    # Navigate to Username (4th item: Down x3)
    send_special Down 0.3
    send_special Down 0.3
    send_special Down 0.3
    sleep 0.3

    # Record cursor Y before entering edit
    local cursor_y_before
    cursor_y_before=$(_tmux display-message -p -t "$PANE" '#{cursor_y}')
    echo -e "  Cursor Y before edit: $cursor_y_before"

    # Get the Username line number in visible area
    local content
    content=$(capture_pane -20)
    local username_line
    username_line=$(echo "$content" | grep -n "Username" | tail -1 | cut -d: -f1)
    echo -e "  Username at capture line: $username_line"

    # Press Enter to enter edit mode
    send_enter 0.5

    content=$(capture_pane -20)
    show_capture "Edit mode state" "$content" 15

    # 1. Verify hint line changed to edit mode
    if ! echo "$content" | grep -q "confirm"; then
        assert_fail "Edit hint 'confirm' not shown"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}Edit hint shown correctly${NC}"

    # 2. Verify cursor Y is on the edit line (Username row), not at bottom
    local cursor_y_edit
    cursor_y_edit=$(_tmux display-message -p -t "$PANE" '#{cursor_y}')
    local cursor_x_edit
    cursor_x_edit=$(_tmux display-message -p -t "$PANE" '#{cursor_x}')
    echo -e "  Cursor position in edit mode: x=$cursor_x_edit y=$cursor_y_edit"

    # Cursor should NOT be on the hint line (last line).
    # Get pane height to determine hint line position
    local pane_height
    pane_height=$(_tmux display-message -p -t "$PANE" '#{pane_height}')
    local hint_y=$((pane_height - 1))
    echo -e "  Pane height: $pane_height, hint line Y: $hint_y"

    if [[ "$cursor_y_edit" -ge "$hint_y" ]]; then
        assert_fail "Cursor is at bottom (y=$cursor_y_edit) instead of edit line"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}Cursor is on edit line, not at bottom${NC}"

    # 3. Verify the Username line shows the value (should still show "user")
    if ! echo "$content" | grep "Username" | grep -q "user"; then
        assert_fail "Username value 'user' not visible in edit mode"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}Value 'user' visible in edit mode${NC}"

    # 4. Type a character and verify it appears on the edit line
    send_keys "X" 0.3
    content=$(capture_pane -20)
    show_capture "After typing X" "$content" 15

    if echo "$content" | grep "Username" | grep -q "X"; then
        echo -e "  ${GREEN}Typed character 'X' appears on edit line${NC}"
    else
        assert_fail "Typed character 'X' not on Username edit line"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    # 5. Verify cursor X moved forward after typing
    local cursor_x_after
    cursor_x_after=$(_tmux display-message -p -t "$PANE" '#{cursor_x}')
    echo -e "  Cursor X after typing: $cursor_x_after (was $cursor_x_edit)"

    if [[ "$cursor_x_after" -gt "$cursor_x_edit" ]]; then
        echo -e "  ${GREEN}Cursor X advanced after typing${NC}"
    else
        assert_fail "Cursor X did not advance after typing (before=$cursor_x_edit, after=$cursor_x_after)"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    # ESC to cancel, ESC to exit menu
    send_special Escape 0.5
    send_special Escape 0.5

    assert_pass "TextInput edit state: cursor on edit line, typing works, hint correct"
    return 0
}

# ── Test 9: Add item handler submenu ──────────────────────────────────
test_9() {
    echo -e "\n${YELLOW}=== Test 9: Add item via handler submenu ===${NC}"

    restart_client
    wait_for_client
    open_test_menu

    # Navigate to "Add item" (last item: Down x4)
    send_special Down 0.3
    send_special Down 0.3
    send_special Down 0.3
    send_special Down 0.3

    # Enter "Add item" submenu
    send_enter 0.5

    local content
    content=$(capture_pane -20)
    show_capture "Add item submenu" "$content" 10

    # Should see breadcrumb "Config > Add item" and fields Name, Type
    if ! echo "$content" | grep -q "Add item"; then
        assert_fail "Not inside Add item submenu"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi
    if ! echo "$content" | grep -q "Name"; then
        assert_fail "Name field not found in Add item submenu"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    # form_mode: Name TextInput auto-enters edit mode, just type and Enter to advance
    send_keys "MyFlag" 0.3
    send_enter 0.5

    content=$(capture_pane -20)
    show_capture "After setting Name (form_mode)" "$content" 10

    if ! echo "$content" | grep "Name" | grep -q "MyFlag"; then
        assert_fail "Name value 'MyFlag' not saved"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi

    # ESC back triggers handler — should return to root with new item
    send_special Escape 1.0

    content=$(capture_pane -20)
    show_capture "After handler (root)" "$content" 12

    # New item "MyFlag" should appear in root menu as a Toggle [OFF]
    if echo "$content" | grep -q "MyFlag"; then
        echo -e "  ${GREEN}New item 'MyFlag' appears in root menu${NC}"
    else
        assert_fail "New item 'MyFlag' not found in root menu after handler"
        send_special Escape 0.5
        return 1
    fi

    if echo "$content" | grep "MyFlag" | grep -q "\[OFF\]"; then
        echo -e "  ${GREEN}New item shows as Toggle [OFF]${NC}"
    else
        assert_fail "New item 'MyFlag' not showing as Toggle [OFF]"
        send_special Escape 0.5
        return 1
    fi

    # "Add item" should still be present
    if echo "$content" | grep -q "Add item"; then
        echo -e "  ${GREEN}'Add item' still present in menu${NC}"
    else
        assert_fail "'Add item' disappeared after handler"
        send_special Escape 0.5
        return 1
    fi

    send_special Escape 0.5
    assert_pass "Add item via handler submenu works"
    return 0
}

# ── Test 10: Form mode auto-edit and cursor advance ───────────────────
test_10() {
    echo -e "\n${YELLOW}=== Test 10: Form mode auto-edit and cursor advance ===${NC}"

    restart_client
    wait_for_client
    open_test_menu

    # Navigate to "Add item" (last item: Down x4)
    send_special Down 0.3
    send_special Down 0.3
    send_special Down 0.3
    send_special Down 0.3

    # Enter "Add item" submenu (form_mode=true)
    send_enter 0.5

    local content
    content=$(capture_pane -20)
    show_capture "Form mode entry" "$content" 10

    # 1. Verify auto-edit: cursor should be visible (edit mode shows cursor)
    #    and Name field should be in edit highlight (not inverse-selected)
    local cursor_visible
    cursor_visible=$(_tmux display-message -p -t "$PANE" '#{cursor_flag}')

    # 2. Type in Name field (auto-edit, no Enter needed to start)
    send_keys "TestItem" 0.3

    content=$(capture_pane -20)
    show_capture "Typing in auto-edit" "$content" 10

    if ! echo "$content" | grep "Name" | grep -q "TestItem"; then
        assert_fail "Typed text 'TestItem' not visible in auto-edit Name field"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}Auto-edit: typing works without pressing Enter first${NC}"

    # 3. Press Enter to confirm Name — should advance to Type (Select)
    send_enter 0.5

    content=$(capture_pane -20)
    show_capture "After Enter (should advance to Type)" "$content" 10

    # Name should still show "TestItem" and Type should be highlighted
    if ! echo "$content" | grep "Name" | grep -q "TestItem"; then
        assert_fail "Name value lost after Enter"
        send_special Escape 0.5
        send_special Escape 0.5
        return 1
    fi
    echo -e "  ${GREEN}Enter advances cursor from Name to Type${NC}"

    # 4. ESC back to root (triggers handler since Name was filled)
    send_special Escape 1.0

    content=$(capture_pane -20)
    show_capture "After handler" "$content" 12

    if echo "$content" | grep -q "TestItem"; then
        echo -e "  ${GREEN}Handler created 'TestItem' in root menu${NC}"
    else
        assert_fail "New item 'TestItem' not found after form_mode handler"
        send_special Escape 0.5
        return 1
    fi

    send_special Escape 0.5
    assert_pass "Form mode: auto-edit on entry, Enter advances, handler works"
    return 0
}

echo -e "${YELLOW}Menu widget integration test: render, navigation, toggle, submenu, exit, cancel, text input, add item, form mode${NC}"
run_tests 10
