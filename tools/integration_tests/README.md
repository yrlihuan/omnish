# Integration Tests

This directory contains integration tests for omnish that require external tools or complex setups.

## Current Tests

### `verify_issue_127.sh`

**Purpose:** Tests the fix for issue #127: "backspace退出chat模式，仅当用户没有发出首轮对话的时候有效"

**What it tests:**
1. **Phase 1 (mode selection):** Backspace exits chat mode when no message has been sent
2. **Phase 2 (chat loop):** Backspace is ignored after the first message has been sent

**How it works:**
- Uses tmux to create an isolated terminal session
- Automates interaction with `omnish-client`
- Verifies chat prompt visibility to determine if still in chat mode

**Requirements:**
- `tmux` installed
- `omnish-client` built (`cargo build`)

**Usage:**
```bash
# From project root
tools/integration_tests/verify_issue_127.sh [-w] [-t TEST_CASE]

# Examples:
tools/integration_tests/verify_issue_127.sh          # Run all tests (default)
tools/integration_tests/verify_issue_127.sh -t 1     # Run only test 1 (phase 1)
tools/integration_tests/verify_issue_127.sh -t 2     # Run only test 2 (phase 2)
tools/integration_tests/verify_issue_127.sh -t all   # Run all tests
tools/integration_tests/verify_issue_127.sh -w -t 1  # Wait for confirmation, then run test 1
```

**Options:**
- `-w`: Wait for user confirmation after showing the monitor command. Useful for manual inspection of the tmux session before tests run.
- `-t TEST_CASE`: Run specific test case(s). Can be: `1` (phase 1), `2` (phase 2), or `all` (default: all).
- `-h, --help`: Show help message.

**Expected output:**
- Test 1: ✓ PASS - Chat prompt disappears after backspace (exited chat mode)
- Test 2: ✓ PASS - Chat prompt still present after backspace (backspace ignored)

## Adding New Integration Tests

When adding new integration tests:

1. Place `.sh` scripts in this directory
2. Use the tmux socket pattern for isolation: `/tmp/claude-tmux-sockets/`
3. Include cleanup functions to remove tmux sessions
4. Add documentation to this README
5. Ensure scripts work from the project root directory

## Notes

- Tests in this directory may be slower than unit tests
- They require external dependencies (tmux, terminal access)
- Use for testing complex user interactions, not for CI/CD
- Consider adding timeouts to prevent hanging tests