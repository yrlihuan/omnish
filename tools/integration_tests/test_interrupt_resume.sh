#!/usr/bin/env bash
#
# test_interrupt_resume.sh - Interrupt/resume contract for thinking-mode chats.
#
# Verifies the regression fixed in commits 05f38ae / 12ab6c8 / 21c2bfe:
# interrupting the LLM (both during a tool call and during the Thinking phase
# right after a tool completes) and then sending another user message must
# not violate the API thinking-mode contract or produce duplicate
# tool_results from a stale agent loop. Symptom of regression: the next
# request fails with an "AI service returned an error" line in the pane.
#
# Test cases:
#   1. Mid-tool-call interrupt + resume + post-tool Thinking interrupt + resume
#      runs to completion without any API error appearing in the pane.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  N. One pass of "two consecutive interrupts (mid-tool, then mid-Thinking
     after tool) followed by /continue must not surface an API error",
     repeated for each backend name in the comma-separated env var
     TEST_CHAT_MODELS.  When unset or empty the test runs once on the
     daemon's default chat model.

Example:
  TEST_CHAT_MODELS=anthropic_default,deepseek_pro \
      bash $(basename "$0")
EOF
}

test_init "interrupt-resume" "$@"

THREADS_DIR="${OMNISH_HOME:-$HOME/.omnish}/threads"

# Verify the most recently modified thread .jsonl has strictly alternating
# user/assistant roles. Two consecutive same-role messages indicate the
# interrupt/resume merge path (chat_session::merge_user_query_into_tail or
# the daemon's persist_unsaved sanitizer) failed to fold the resume into
# the prior tail - the exact regression the test is gating against.
# Echoes a diagnostic line listing the offending pair on failure.
assert_thread_alternates() {
    local latest
    latest=$(ls -t "$THREADS_DIR"/*.jsonl 2>/dev/null | head -1)
    if [[ -z "$latest" ]]; then
        assert_fail "No .jsonl thread file found in $THREADS_DIR"
        return 1
    fi
    echo -e "  Checking thread file: ${YELLOW}${latest}${NC}"
    local result
    result=$(python3 - "$latest" <<'PY'
import json, sys
path = sys.argv[1]
prev_role = None
prev_idx = None
with open(path) as f:
    for idx, line in enumerate(f):
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            print(f"BAD_JSON at line {idx}: {line[:80]}")
            sys.exit(1)
        role = msg.get("role")
        if role not in ("user", "assistant"):
            print(f"BAD_ROLE at line {idx}: {role!r}")
            sys.exit(1)
        if role == prev_role:
            print(f"DUPLICATE {role} at lines {prev_idx} and {idx}")
            sys.exit(1)
        prev_role, prev_idx = role, idx
print("OK")
PY
    )
    if [[ "$result" != "OK" ]]; then
        assert_fail "Thread roles not alternating: $result"
        return 1
    fi
    assert_pass "Thread roles strictly alternate user/assistant"
    return 0
}

# Patterns the daemon writes when an LLM call fails downstream (see
# server.rs handle_chat_message error path). Any of these means the
# resume request triggered the bug we're guarding against.
API_ERROR_RE='AI service returned an error|Connection to the AI service was lost|<event>api error</event>'

# Spinner-line markers.  "Thinking…" uses U+2026 (horizontal ellipsis), the
# exact byte sequence emitted by chat_session::thinking_line().  TOOL_RUNNING
# matches the Braille spinner glyphs on the tool header line - status_icon_str
# only renders one of these while the tool is still in flight; once it
# completes the icon flips to the static "●".  Matching the static "●" would
# fire after the tool already finished, which is why the first Ctrl-C
# previously arrived too late.
THINKING_RE='Thinking…'
TOOL_RUNNING_RE='[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏][[:space:]]+Bash\(sleep'
TOOL_ANY_RE='[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏●][[:space:]]+Bash\(sleep'

# wait_for_pattern <regex> <timeout_seconds> [poll_interval=0.1] [history_lines=-50]
# Returns 0 once `capture_pane` matches; 1 on timeout.  Default interval is
# 100ms so we never miss a >=1s window (e.g. the running spinner before a
# 5s sleep completes).
wait_for_pattern() {
    local pattern="$1"
    local timeout="$2"
    local interval="${3:-0.1}"
    local hist="${4:--50}"
    local deadline_ns
    deadline_ns=$(($(date +%s%N) + timeout * 1000000000))
    while (( $(date +%s%N) < deadline_ns )); do
        if capture_pane "$hist" | grep -qE "$pattern"; then
            return 0
        fi
        sleep "$interval"
    done
    return 1
}

# wait_for_thinking_fast <timeout_seconds>
# High-frequency (50ms) poll for the Thinking… line. Used right after a
# bash tool result is returned, where the LLM may emit Thinking briefly
# before streaming the next token. Slow polling races past it.
wait_for_thinking_fast() {
    local timeout="$1"
    local deadline_ns
    deadline_ns=$(($(date +%s%N) + timeout * 1000000000))
    while (( $(date +%s%N) < deadline_ns )); do
        if capture_pane -10 | grep -qE "$THINKING_RE"; then
            return 0
        fi
        sleep 0.05
    done
    return 1
}

# run_interrupt_cycle [model_name]
# Body of one interrupt+resume run.  When `model_name` is non-empty, the
# test issues `/model <name>` immediately after entering chat so the new
# thread is bound to that backend before the first user query.  Empty =
# leave the daemon's default chat model in place.
run_interrupt_cycle() {
    local model="${1:-}"

    restart_client
    wait_for_client

    enter_chat

    if [[ -n "$model" ]]; then
        echo -e "  Selecting model: ${YELLOW}${model}${NC}"
        send_keys "/model $model" 0.3
        send_enter 0.5
        local pane
        pane=$(capture_pane -20)
        if echo "$pane" | grep -qE "Switched to ${model}\b"; then
            echo -e "  Model selected"
        elif echo "$pane" | grep -q "Unknown model:"; then
            show_capture "/model output" "$pane" 15
            assert_fail "Backend '$model' unknown to daemon"
            return 1
        else
            show_capture "/model output" "$pane" 15
            assert_fail "Did not see 'Switched to $model' confirmation"
            return 1
        fi
    fi

    # Explicit prompt: ask for two serial sleep-5 invocations so we have
    # one tool we can interrupt mid-execution and another we can let
    # finish so the post-tool Thinking phase is exercised.
    send_keys "请使用bash工具串行执行2次命令, 每次都是 sleep 5" 0.3
    send_enter 0.3

    # ── First interrupt: during the first tool's execution ──────────────
    # 90s tolerates one upstream 60s 429 backoff (cf. test_chat_interrupt).
    # Match the Braille-spinner header so we only fire while the tool is
    # actually mid-flight; matching "●" would trip after the sleep already
    # completed.
    if ! wait_for_pattern "$TOOL_RUNNING_RE" 90; then
        show_capture "Running tool never observed" "$(capture_pane -50)" 30
        assert_fail "Bash(sleep ...) running spinner never observed within 90s"
        return 1
    fi
    echo -e "  First tool running, sending Ctrl-C..."
    send_special C-c 2

    if ! is_chat_prompt "$(capture_pane -10)"; then
        show_capture "After first Ctrl-C" "$(capture_pane -20)" 15
        assert_fail "Did not return to chat prompt after first interrupt"
        return 1
    fi
    echo -e "  Back at chat prompt"

    # ── Resume #1 ───────────────────────────────────────────────────────
    send_keys "继续" 0.3
    send_enter 0.3

    # Wait for the resumed request to dispatch a Bash tool again.  Match
    # any header (running or completed) - we don't need to catch it
    # mid-flight here since the next step waits for completion anyway.
    if ! wait_for_pattern "$TOOL_ANY_RE" 90; then
        show_capture "Resumed run never dispatched tool" "$(capture_pane -80)" 40
        assert_fail "Bash tool did not re-dispatch after first /continue"
        return 1
    fi
    echo -e "  Tool re-dispatched after resume; waiting for completion..."

    # sleep 5 + small buffer so we know the bash exits before we poll.
    # We must not Ctrl-C while the tool is still mid-flight or this just
    # repeats test 1 instead of exercising the post-tool Thinking path.
    sleep 6

    # ── Second interrupt: during the post-tool Thinking phase ────────────
    # The Thinking line is brief (LLM streams the next iteration quickly),
    # hence the 50ms poll. 30s window covers cases where the LLM stays in
    # Thinking longer (e.g. 429 retry).
    if ! wait_for_thinking_fast 30; then
        show_capture "Thinking line never observed" "$(capture_pane -80)" 40
        assert_fail "Did not observe post-tool Thinking line within 30s"
        return 1
    fi
    echo -e "  Thinking observed, sending Ctrl-C..."
    send_special C-c 2

    if ! is_chat_prompt "$(capture_pane -10)"; then
        show_capture "After second Ctrl-C" "$(capture_pane -20)" 15
        assert_fail "Did not return to chat prompt after second interrupt"
        return 1
    fi
    echo -e "  Back at chat prompt"

    # ── Resume #2 ───────────────────────────────────────────────────────
    send_keys "继续" 0.3
    send_enter 0.3

    # Wait for the resumed turn to come back. Allow extra time because
    # the resume includes the merged interrupt marker plus any pending
    # tool output.
    if ! wait_for_chat_response 240 2; then
        show_capture "No final response" "$(capture_pane -120)" 60
        assert_fail "No chat response within 240s after second /continue"
        return 1
    fi

    # ── Final assertion: no API error ever surfaced ─────────────────────
    # Use a wide capture window: the first error (if any) might have been
    # pushed up by subsequent tool output and resume text.
    local content
    content=$(capture_pane -400)
    if echo "$content" | grep -qE "$API_ERROR_RE"; then
        show_capture "API error detected" "$content" 200
        assert_fail "API error appeared during interrupt/resume cycle"
        return 1
    fi

    if ! assert_thread_alternates; then
        return 1
    fi

    assert_pass "Two interrupt + resume cycles completed without API error"
    return 0
}

# ── Model matrix ────────────────────────────────────────────────────────
# Split TEST_CHAT_MODELS on commas; trim surrounding whitespace and drop
# empty fields so "a, ,b" yields ["a","b"].  When the var is unset or
# empty we fall back to a single run on the daemon's default chat model
# (passed as the empty string, which run_interrupt_cycle treats as
# "skip /model").
MODELS=()
if [[ -n "${TEST_CHAT_MODELS:-}" ]]; then
    IFS=',' read -ra _raw <<< "$TEST_CHAT_MODELS"
    for entry in "${_raw[@]}"; do
        entry="${entry#"${entry%%[![:space:]]*}"}"   # ltrim
        entry="${entry%"${entry##*[![:space:]]}"}"   # rtrim
        [[ -n "$entry" ]] && MODELS+=("$entry")
    done
fi
if [[ ${#MODELS[@]} -eq 0 ]]; then
    MODELS=("")
    echo -e "${YELLOW}TEST_CHAT_MODELS not set; running once on the default model${NC}"
else
    echo -e "${YELLOW}Running interrupt/resume cycle for ${#MODELS[@]} model(s): ${MODELS[*]}${NC}"
fi

# Generate one test_$i per model so run_tests' -t selector and pass/fail
# accounting work per-model without changes to lib.sh.
for ((i = 0; i < ${#MODELS[@]}; i++)); do
    model="${MODELS[$i]}"
    label="${model:-default}"
    eval "test_$((i + 1))() {
        echo -e \"\n\${YELLOW}=== Test $((i + 1)): interrupt+resume on '${label}' ===\${NC}\"
        run_interrupt_cycle '$model'
    }"
done

run_tests "${#MODELS[@]}"
