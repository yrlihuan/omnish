#!/usr/bin/env bash
#
# test_thread_command.sh - Integration tests for /thread stats display (#608).
#
# Test cases:
#   1. /thread stats with an active thread in chat
#        → output starts with "Thread Stats:" and contains "[active]"
#   2. /thread stats without an active thread (fresh chat, no message sent)
#        → output starts with "Thread Stats:" and lists threads as "[N]"
#   3. /thread stats 3 (regression for #608)
#        → output is the stats display, NOT /thread list output
#          (must not contain "Conversations:" header or
#          "(N total, showing M ...)" summary)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. /thread stats in active chat shows current thread stats with [active]
  2. /thread stats in fresh chat (no active thread) lists all threads
  3. /thread stats 3 returns stats output, not /thread list output (#608)
EOF
}

test_init "thread-command" "$@"

# Helper: from a fresh client, enter chat and send one message so that a
# thread exists on disk. Leaves the user inside chat mode with the thread
# claimed by the current session.
seed_active_thread() {
    restart_client
    wait_for_client

    enter_chat
    send_keys "Reply with just the word: ok" 0.3
    send_enter 0.3
    if ! wait_for_chat_response; then
        show_capture "After seed message" "$(capture_pane -20)" 10
        return 1
    fi
    return 0
}

# Helper: ensure at least one thread exists on disk, then start a fresh
# client with NO active claim and leave the user inside chat mode.
enter_chat_without_active_thread() {
    # Ensure a thread exists. prepare_threads.sh creates some in CI; locally
    # the previous test may have already created one. Either way, seeding
    # here is cheap and idempotent for our needs.
    if ! seed_active_thread; then
        return 1
    fi
    # Drop out of chat and tear the session down so the thread claim is
    # released. The new client below will not have any thread active.
    send_special Escape 0.3

    restart_client
    wait_for_client
    enter_chat
    return 0
}

# ── Test 1: /thread stats with active thread ──────────────────────────────
test_1() {
    echo -e "\n${YELLOW}=== Test 1: /thread stats with active thread ===${NC}"

    if ! seed_active_thread; then
        assert_fail "Failed to seed an active thread"
        return 1
    fi

    send_keys "/thread stats" 0.3
    send_enter 1

    local content
    content=$(capture_pane -30)
    show_capture "/thread stats (active thread)" "$content" 15

    if ! echo "$content" | grep -q "Thread Stats:"; then
        assert_fail "Expected 'Thread Stats:' header in output"
        return 1
    fi
    if ! echo "$content" | grep -q "\[active\]"; then
        assert_fail "Expected '[active]' marker for current thread"
        return 1
    fi
    if echo "$content" | grep -q "Conversations:"; then
        assert_fail "Unexpected 'Conversations:' header in stats output"
        return 1
    fi
    assert_pass "/thread stats shows current-thread stats with [active]"
    return 0
}

# ── Test 2: /thread stats without active thread ──────────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: /thread stats without active thread ===${NC}"

    if ! enter_chat_without_active_thread; then
        assert_fail "Failed to set up fresh chat without active thread"
        return 1
    fi

    send_keys "/thread stats" 0.3
    send_enter 1

    # Capture a generous scrollback: when many threads exist, the
    # "Thread Stats:" header may have scrolled off well before this capture.
    # Don't rely on the header - assert stats-specific per-thread markers.
    local content
    content=$(capture_pane -500)
    show_capture "/thread stats (no active thread)" "$content" 20

    if echo "$content" | grep -q "Conversations:"; then
        assert_fail "Unexpected 'Conversations:' header in stats output"
        return 1
    fi
    # /thread list footer mentions "Use /thread list"; stats footer mentions
    # "Use /thread stats". Reject the list-specific phrasing only.
    if echo "$content" | grep -q "Use /thread list"; then
        assert_fail "Unexpected /thread list footer in stats output"
        return 1
    fi
    # Note: do NOT assert absence of [active] here. The multi-thread stats
    # listing marks ANY thread that is locked by any session as [active],
    # so concurrent clients on this machine can legitimately produce them.
    # Stats output uniquely includes "model: ... | context: ..." lines
    # under each thread; list output puts everything on a single line.
    if ! echo "$content" | grep -qE "model:.*context:"; then
        assert_fail "Expected stats marker 'model: ... | context: ...' in output"
        return 1
    fi
    # /thread stats defaults to N=10. Count entries by the per-thread
    # "model: ... | context: ..." line (one per thread, unique to stats
    # output, immune to false matches from the client's connection banner
    # "[0] [omnish] Connected ...").
    local count
    count=$(echo "$content" | grep -cE 'model:.*context:') || true
    if (( count < 1 )); then
        assert_fail "Expected at least one thread entry, found $count"
        return 1
    fi
    if (( count > 10 )); then
        assert_fail "Expected at most 10 entries (default limit), found $count"
        return 1
    fi
    assert_pass "/thread stats lists threads with default limit 10 ($count entries)"
    return 0
}

# ── Test 3: /thread stats 3 returns stats, not list (#608) ───────────────
test_3() {
    echo -e "\n${YELLOW}=== Test 3: /thread stats 3 returns stats, not list (#608) ===${NC}"

    if ! enter_chat_without_active_thread; then
        assert_fail "Failed to set up fresh chat for /thread stats 3"
        return 1
    fi

    send_keys "/thread stats 3" 0.3
    send_enter 1

    local content
    content=$(capture_pane -500)
    show_capture "/thread stats 3" "$content" 20

    # The bug: dispatcher misroutes "conversations stats 3" to /thread list 3,
    # giving the conversations listing with its "Conversations:" header and
    # the "(N total, showing M ...)" summary line.
    if echo "$content" | grep -q "Conversations:"; then
        assert_fail "Bug #608: /thread stats 3 returns /thread list output ('Conversations:' header)"
        return 1
    fi
    # /thread list footer mentions "Use /thread list"; stats footer mentions
    # "Use /thread stats". Reject the list-specific phrasing only.
    if echo "$content" | grep -q "Use /thread list"; then
        assert_fail "Bug #608: /thread stats 3 returns /thread list footer"
        return 1
    fi
    # N=3 should cap the listing at exactly 3 thread entries. Count entries
    # via the per-thread "model: ... | context: ..." line (one per entry,
    # unique to stats output).
    local count
    count=$(echo "$content" | grep -cE 'model:.*context:') || true
    if (( count != 3 )); then
        assert_fail "Expected exactly 3 thread entries from N=3, found $count"
        return 1
    fi
    assert_pass "/thread stats 3 returns stats output capped at N=3"
    return 0
}

echo -e "${YELLOW}Thread command integration test: /thread stats display variants${NC}"
run_tests 3
