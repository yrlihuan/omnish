# Changelog

## v0.5.1 (2026-03-09)

### Features
- **widgets**: `LineEditor` — full-featured chat input with cursor movement (←/→/↑/↓, Home/End, Ctrl-A/E), word jump (Ctrl-←/→), word delete, and multi-line editing (issue #180)
- **client**: Multi-line paste via bracketed paste mode; fast-paste detection as fallback; large pastes collapsed to `[pasted text N chars]` marker
- **client**: Shift+Enter / Ctrl+J inserts newline in chat input
- **widgets**: `LineStatus` — temporary single-line status display that erases itself completely on `clear()`, fixing residual `(thinking...)` and tool-status lines (issue #183)

### Bug Fixes
- **client**: Paste block backspace display and cursor positioning
- **client**: Track terminal cursor row for correct multi-line redraw
- **daemon**: Use char boundary for `/thread list` question truncation

### Refactoring
- **widgets**: Move picker into `widgets/` module alongside LineEditor and LineStatus
- **client**: Integrate paste blocks into LineEditor as placeholder characters

### Removals
- **tools**: Remove `omnish-commands` diagnostic binary

---

## v0.5.0 (2026-03-08)

### Features
- **daemon**: Agent loop with tool execution — LLM can call `command_query` tool to inspect command output, up to 5 iterations (issue #161)
- **daemon**: Rewrite ConversationManager to raw JSON storage format for KV cache-optimized conversation replay (issue #166)
- **protocol**: ChatToolStatus message type for streaming tool-use status to client
- **transport**: Streaming multi-message RPC responses with end-of-stream sentinel
- **client**: Stream ChatToolStatus messages during agent tool execution
- **client**: `/thread del` uses multi-select picker widget when no index given (issue #168)
- **template**: Add Tools section to CHAT_SYSTEM_PROMPT documenting command_query usage
- **template**: Move `/template chat` to daemon request, show actual tool definitions (issue #164)
- **context**: Wrap workingDirectory in `<system-reminder>` tags for auto-complete (issue #167)
- **daemon**: Include recent command list directly in chat context (issue #165)

### Bug Fixes
- **llm**: Use config `base_url` for Anthropic backend, upgrade API version to 2024-04-04
- **transport**: Fix stream memory leak — add Ack sentinel for multi-message RPC cleanup
- **daemon**: Don't persist `<system-reminder>` in thread JSONL files (issue #169)
- **daemon**: Replace newlines in `/thread list` question preview
- **client**: Sort picker indices numerically, not lexicographically

### Breaking Changes
- **storage**: Chat thread JSONL format changed from `{role, content, ts}` to raw Anthropic API JSON. Old thread files must be deleted (`rm ~/.local/share/omnish/threads/*.jsonl`).

---

## v0.4.1 (2026-03-06)

### Bug Fixes
- **client**: `/context` with pipe (e.g. `| tail -n 5`) now correctly uses thread-aware routing in chat mode after `/resume` (issue #144, #145)
- **client**: Backspace only exits chat mode before first message is sent; suppressed visual artifacts on empty buffer backspace (issue #127)
- **client**: Default to 10 lines when `| head` or `| tail` has no number argument
- **client**: Parse redirect before limit to support both together

### Features
- **client**: Support `| head`/`| tail` with `-n N` and `-nN` syntax for command output
- **client**: Add shell cwd to `/debug client` output (issue #146)

### Testing & Tooling
- **tools**: Add shared integration test library (`lib.sh`) with tmux helpers, assertions, and test runner
- **tools**: Add integration tests for issue #127 and #144
- **tools**: Fix test tmux config to use bash as default shell instead of installed omnish

---

## v0.4.0 (2026-03-04)

### Features
- **client**: Multi-turn chat mode with `/chat`, `/resume`, `/conversations`, `/threads` (issue #110, #111, #121, #128, #129)
- **client**: Deferred thread creation — thread only created on first message (issue #130)
- **client**: `/context` in chat mode shows current thread's conversation (issue #136)
- **client**: Ctrl-C interrupts pending chat LLM request (issue #123)
- **client**: Ctrl-D and backspace-on-empty exit chat mode (issue #120, #124)
- **client**: `/resume [N]` to select and resume conversations by index (issue #111, #133)
- **llm**: System prompt for chat mode with command awareness (issue #140)
- **llm**: Multi-turn conversation support in Anthropic and OpenAI backends (issue #110)
- **protocol**: ChatStart/ChatReady/ChatMessage/ChatResponse message types (issue #110)
- **daemon**: ConversationManager for thread storage and retrieval (issue #110)
- **daemon**: Load conversations into memory at startup (issue #131)
- **daemon**: Relative time display in `/conversations` (issue #139)
- **daemon**: JSON command responses with display field (issue #134)
- **client**: Ghost completion restored in chat mode (issue #119)
- **client**: `/debug client` restored in chat mode via closure (issue #115)
- **completion**: Disable thinking mode for auto-completion requests (issue #118)

### Bug Fixes
- **client**: Handle multi-byte UTF-8 in backspace (issue #141)
- **client**: Enter chat mode immediately on prefix match (issue #116)
- **client**: Process initial message from interceptor in chat loop (issue #114)
- **client**: Render /commands inline in chat mode without PTY cleanup (issue #114)
- **client**: Clear full readline on chat exit to prevent stale command execution (issue #125)
- **client**: Track isearch mode with dedicated flag, not timeout (issue #88)
- **client**: Skip readline trigger when user typed since completion request (issue #88)
- **client**: Discard completion suggestions that diverge from input (issue #113)
- **client**: Discard completion responses during emacs-isearch mode (issue #88)
- **client**: Remove debug state header/footer from output (issue #135)
- **client**: `/resume` shows last exchange of resumed conversation (issue #137)
- **client**: Auto-fetch conversations when `/resume N` cache is empty (issue #133)
- **daemon**: Resolve interrupted chat exchanges on load (issue #126)
- **daemon**: Suppress noisy rustls debug logs (issue #132)
- **llm**: Pass `enable_thinking=false` to vLLM (issue #118)

---

## v0.3.0 (2026-03-01)

### Features
- **context**: `/context <scenario>` command for viewing different context types (completion, chat, daily-notes, hourly-notes)
- **context**: `/template <name>` command for viewing prompt templates
- **client**: `/version` command
- **transport**: TLS support for TCP connections
- **transport**: Socket permissions (0600) and peer UID verification
- **transport**: Token authentication for RPC server
- **protocol**: Auth and AuthFailed message variants
- **common**: Auth token generation and loading utilities
- **context**: Reuse existing context building functions for hourly/daily notes
- **completion**: Proactive KV cache warmup on context prefix change
- **completion**: XML tags with hostname:cwd prompt format
- **completion**: Concurrent completion requests with intelligent filtering
- **completion**: Prefer full command for 2nd suggestion (issue #93, #95)
- **context**: Per-command output char limit and reduced max_line_width
- **daemon**: Daily notes include hourly notes as LLM context (issue #63)
- **daemon**: Completion sampling infrastructure and logic (issue #101)
- **store**: JSONL sample writer thread (issue #101)
- **client**: Completion enabled toggle in client config
- **debug**: Show version in `/debug client` output
- **version**: Auto-embed git version via build.rs
- **template**: Hourly-notes template

### Bug Fixes
- **completion**: Reset debounce on all input activity (issue #100)
- **completion**: Subtract input prefix length in issue #95 check (issue #99)
- **completion**: Suppress ghost text when cursor not at end of input
- **completion**: Freeze history section between elastic resets for KV cache
- **context**: Trim leading whitespace from command output
- **daemon**: Release sessions read lock before disk I/O in cleanup (issue #61)
- **daemon**: Eliminate lock contention causing all clients to freeze (issue #61)
- **daemon**: Use local timezone for cron scheduler
- **hook**: Bind readline trigger in emacs-isearch keymap (issue #88)
- **llm**: Unify completion prompt template for KV cache stability
- **llm**: Handle `<think>` tag at start of response without leading newline
- **client**: Discard completion that is a subset of current input
- **client**: Clear stale readline content on prompt return
- **client**: Clear pending_rl_report on prompt (issue #34)
- **client**: Auto-clear pending_rl_report after 1s timeout (issue #57)
- **context**: Replace home directory with ~ in paths
- **daily-notes**: Use local timezone, include hourly notes in LLM context only
- **shell-hook**: Suppress error when emacs-isearch keymap not available
- **completion**: Avoid suggesting && chained commands unless in history
- **daemon**: Truncate completion suggestions at && when input has none (issue #107)
- **chat**: Only include recent commands with output in chat context (issue #109)
- **macOS**: Full macOS support for shell probes

---

## v0.2.0 (2026-02-24)

### Features
- **version**: Auto-embed git version via build.rs and `--version` flag
- **daemon**: Highlight current session in `/sessions` output

---

## v0.1.1 (2026-02-22)

Initial tagged release with core functionality:

### Features
- **core**: PTY proxy with forkpty, raw mode, and window resize
- **core**: Client-daemon architecture with Unix socket transport
- **core**: Binary framed protocol with bincode serialization
- **core**: Session manager with metadata and binary stream storage
- **llm**: LlmBackend trait with Anthropic and OpenAI-compatible backends
- **context**: Two-tier context (history + detailed commands) with elastic window
- **context**: Strategy/formatter pattern for context building
- **tracker**: Command boundary detection via OSC 133 + regex fallback
- **client**: Ghost text completion with Tab accept
- **client**: Chat mode with `:` prefix and inline prompt UI
- **client**: `/debug`, `/sessions`, `/context` commands
- **client**: Input interceptor with alternate screen suppression
- **daemon**: Daily notes with command log and LLM summary
- **daemon**: Session eviction for inactive sessions
- **transport**: RPC client/server with reconnection and exponential backoff
- **store**: Command recording and persistence
