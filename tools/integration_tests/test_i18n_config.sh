#!/usr/bin/env bash
#
# test_i18n_config.sh - Integration tests for i18n config menu display.
#
# Tests:
#   1. English (default): config menu labels are in English, Language shows "English"
#   2. Simplified Chinese (OMNISH_LANG=zh): labels in simplified Chinese
#   3. Traditional Chinese (OMNISH_LANG=zh-tw): labels in traditional Chinese

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. English config menu labels (default language)
  2. Simplified Chinese config menu labels (OMNISH_LANG=zh)
  3. Traditional Chinese config menu labels (OMNISH_LANG=zh-tw)
EOF
}

test_init "i18n-config" "$@"

# Start client with a specific OMNISH_LANG env var.
start_client_lang() {
    local lang="$1"
    _tmux kill-session -t "$SESSION" 2>/dev/null || true
    _tmux new -d -s "$SESSION" -n test "OMNISH_LANG=$lang $CLIENT"
}

# Helper: enter chat and open /config
open_config() {
    enter_chat
    send_keys "/config" 0.3
    send_enter 1
}

# ── Test 1: English config menu labels ────────────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: English config menu labels ===${NC}"

    start_client_lang "en"
    wait_for_client

    open_config

    local content
    content=$(capture_pane -30)
    show_capture "Config top level (en)" "$content" 15

    local ok=true

    # Check top-level submenu labels
    if echo "$content" | grep -q "General"; then
        assert_pass "Top-level 'General' label present"
    else
        assert_fail "Top-level 'General' label missing"
        ok=false
    fi

    if echo "$content" | grep -q "LLM"; then
        assert_pass "Top-level 'LLM' label present"
    else
        assert_fail "Top-level 'LLM' label missing"
        ok=false
    fi

    if echo "$content" | grep -q "Sandbox"; then
        assert_pass "Top-level 'Sandbox' label present"
    else
        assert_fail "Top-level 'Sandbox' label missing"
        ok=false
    fi

    # Enter General submenu
    send_enter 0.5
    content=$(capture_pane -30)
    show_capture "General submenu (en)" "$content" 15

    if echo "$content" | grep -q "Hotkeys"; then
        assert_pass "General > 'Hotkeys' label present"
    else
        assert_fail "General > 'Hotkeys' label missing"
        ok=false
    fi

    if echo "$content" | grep -q "Language"; then
        assert_pass "General > 'Language' label present"
    else
        assert_fail "General > 'Language' label missing"
        ok=false
    fi

    if echo "$content" | grep -q "Completion"; then
        assert_pass "General > 'Completion' label present"
    else
        assert_fail "General > 'Completion' label missing"
        ok=false
    fi

    if echo "$content" | grep -q "Auto Update"; then
        assert_pass "General > 'Auto Update' label present"
    else
        assert_fail "General > 'Auto Update' label missing"
        ok=false
    fi

    # Language value should show a display name, not raw code (en/zh/zh-tw)
    local lang_line
    lang_line=$(echo "$content" | grep "Language" | head -1)
    if echo "$lang_line" | grep -qE 'English|简体中文|繁體中文'; then
        assert_pass "Language shows display name (not raw code)"
    else
        assert_fail "Language should show display name, not raw code"
        ok=false
    fi

    # Verify raw codes are NOT shown
    if echo "$lang_line" | grep -qE '\[en\]|\[zh\]|\[zh-tw\]'; then
        assert_fail "Language shows raw code"
        ok=false
    else
        assert_pass "Language does not show raw code"
    fi

    # Exit config: ESC back to General, ESC back to top, ESC exit
    send_special Escape 0.3
    send_special Escape 0.3
    send_special Escape 0.3

    $ok
}

# ── Test 2: Chinese config menu labels ────────────────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: Chinese config menu labels (OMNISH_LANG=zh) ===${NC}"

    start_client_lang "zh"
    wait_for_client

    open_config

    local content
    content=$(capture_pane -30)
    show_capture "Config top level (zh)" "$content" 15

    local ok=true

    # Check top-level submenu labels in Chinese
    if echo "$content" | grep -q "通用"; then
        assert_pass "Top-level '通用' (General) label present"
    else
        assert_fail "Top-level '通用' (General) label missing"
        ok=false
    fi

    if echo "$content" | grep -q "大语言模型"; then
        assert_pass "Top-level '大语言模型' (LLM) label present"
    else
        assert_fail "Top-level '大语言模型' (LLM) label missing"
        ok=false
    fi

    if echo "$content" | grep -q "沙箱"; then
        assert_pass "Top-level '沙箱' (Sandbox) label present"
    else
        assert_fail "Top-level '沙箱' (Sandbox) label missing"
        ok=false
    fi

    # Enter 通用 (General) submenu
    send_enter 0.5
    content=$(capture_pane -30)
    show_capture "General submenu (zh)" "$content" 15

    if echo "$content" | grep -q "快捷键"; then
        assert_pass "General > '快捷键' (Hotkeys) label present"
    else
        assert_fail "General > '快捷键' (Hotkeys) label missing"
        ok=false
    fi

    if echo "$content" | grep -q "语言"; then
        assert_pass "General > '语言' (Language) label present"
    else
        assert_fail "General > '语言' (Language) label missing"
        ok=false
    fi

    if echo "$content" | grep -q "补全"; then
        assert_pass "General > '补全' (Completion) label present"
    else
        assert_fail "General > '补全' (Completion) label missing"
        ok=false
    fi

    if echo "$content" | grep -q "自动更新"; then
        assert_pass "General > '自动更新' (Auto Update) label present"
    else
        assert_fail "General > '自动更新' (Auto Update) label missing"
        ok=false
    fi

    if echo "$content" | grep -q "代理"; then
        assert_pass "General > '代理' (Proxy) label present"
    else
        assert_fail "General > '代理' (Proxy) label missing"
        ok=false
    fi

    # Language value should show a display name, not raw code
    local lang_line_zh
    lang_line_zh=$(echo "$content" | grep "语言" | head -1)
    if echo "$lang_line_zh" | grep -qE 'English|简体中文|繁體中文'; then
        assert_pass "Language shows display name (not raw code)"
    else
        assert_fail "Language should show display name, not raw code"
        ok=false
    fi

    # Navigate to 大语言模型 submenu: ESC back, Down, Enter
    send_special Escape 0.5
    send_special Down 0.3
    send_enter 0.5
    content=$(capture_pane -30)
    show_capture "LLM submenu (zh)" "$content" 15

    if echo "$content" | grep -q "用途"; then
        assert_pass "LLM > '用途' (Use Cases) label present"
    else
        assert_fail "LLM > '用途' (Use Cases) label missing"
        ok=false
    fi

    if echo "$content" | grep -q "后端列表"; then
        assert_pass "LLM > '后端列表' (Backends) label present"
    else
        assert_fail "LLM > '后端列表' (Backends) label missing"
        ok=false
    fi

    # Enter Backends submenu (second item under LLM)
    send_special Down 0.3
    send_enter 0.5
    content=$(capture_pane -30)
    show_capture "Backends submenu (zh)" "$content" 15

    if echo "$content" | grep -q "添加后端"; then
        assert_pass "Backends > '添加后端' (Add backend) label present"
    else
        assert_fail "Backends > '添加后端' (Add backend) label missing"
        ok=false
    fi

    # Exit config
    send_special Escape 0.3
    send_special Escape 0.3
    send_special Escape 0.3
    send_special Escape 0.3

    $ok
}

# ── Test 3: Traditional Chinese config menu labels ────────────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: Traditional Chinese config menu labels (OMNISH_LANG=zh-tw) ===${NC}"

    start_client_lang "zh-tw"
    wait_for_client

    open_config

    local content
    content=$(capture_pane -30)
    show_capture "Config top level (zh-tw)" "$content" 15

    local ok=true

    # Check top-level submenu labels in Traditional Chinese
    if echo "$content" | grep -q "一般"; then
        assert_pass "Top-level '一般' (General) label present"
    else
        assert_fail "Top-level '一般' (General) label missing"
        ok=false
    fi

    if echo "$content" | grep -q "大語言模型"; then
        assert_pass "Top-level '大語言模型' (LLM) label present"
    else
        assert_fail "Top-level '大語言模型' (LLM) label missing"
        ok=false
    fi

    if echo "$content" | grep -q "沙箱"; then
        assert_pass "Top-level '沙箱' (Sandbox) label present"
    else
        assert_fail "Top-level '沙箱' (Sandbox) label missing"
        ok=false
    fi

    # Enter 一般 (General) submenu
    send_enter 0.5
    content=$(capture_pane -30)
    show_capture "General submenu (zh-tw)" "$content" 15

    if echo "$content" | grep -q "快速鍵"; then
        assert_pass "General > '快速鍵' (Hotkeys) label present"
    else
        assert_fail "General > '快速鍵' (Hotkeys) label missing"
        ok=false
    fi

    if echo "$content" | grep -q "語言"; then
        assert_pass "General > '語言' (Language) label present"
    else
        assert_fail "General > '語言' (Language) label missing"
        ok=false
    fi

    if echo "$content" | grep -q "補全"; then
        assert_pass "General > '補全' (Completion) label present"
    else
        assert_fail "General > '補全' (Completion) label missing"
        ok=false
    fi

    if echo "$content" | grep -q "自動更新"; then
        assert_pass "General > '自動更新' (Auto Update) label present"
    else
        assert_fail "General > '自動更新' (Auto Update) label missing"
        ok=false
    fi

    if echo "$content" | grep -q "代理"; then
        assert_pass "General > '代理' (Proxy) label present"
    else
        assert_fail "General > '代理' (Proxy) label missing"
        ok=false
    fi

    # Language value should show a display name, not raw code
    local lang_line
    lang_line=$(echo "$content" | grep "語言" | head -1)
    if echo "$lang_line" | grep -qE 'English|简体中文|繁體中文'; then
        assert_pass "Language shows display name (not raw code)"
    else
        assert_fail "Language should show display name, not raw code"
        ok=false
    fi

    # Exit config
    send_special Escape 0.3
    send_special Escape 0.3
    send_special Escape 0.3

    $ok
}

run_tests 3
