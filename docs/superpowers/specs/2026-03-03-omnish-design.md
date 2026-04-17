# omnish Design

A transparent shell wrapper that captures I/O from running programs, aggregates sessions across multiple terminals (and future multi-machine), and integrates with remote LLMs for analysis, inline completion, and periodic summaries.

## Architecture Overview

```
┌─────────────────────────────────────────────────┐
│                 omnishd (daemon)                 │
│  ┌──────────┐ ┌──────────┐ ┌─────────────┐     │
│  │ Session  │ │ Storage  │ │ LLM Engine  │     │
│  │ Manager  │ │ (stream) │ │(multi-backend)│    │
│  └────▲─────┘ └──────────┘ └─────────────┘     │
│       │                    ┌─────────────┐      │
│       │                    │ Task Manager│      │
│       │                    │(cron sched) │      │
│       │  Unix Socket       └─────────────┘      │
├───────┼────────────────────────────────────────┤
│  ┌────┴─────┐  ┌──────────┐  ┌──────────┐      │
│  │ omnish   │  │ omnish   │  │ omnish   │      │
│  │ (tty 1)  │  │ (tty 2)  │  │ (tty 3)  │      │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘      │
│       │ PTY          │ PTY         │ PTY        │
│  ┌────┴─────┐  ┌────┴─────┐  ┌────┴─────┐      │
│  │  bash    │  │  bash    │  │ ssh ...  │      │
│  └──────────┘  └──────────┘  └──────────┘      │
└─────────────────────────────────────────────────┘
```

- **omnish (client)** - User's login shell or shell entry point. Uses `forkpty()` to spawn the real shell, acting as a transparent PTY proxy. Provides inline ghost-text completion and chat mode via `::` prefix interception. All daemon communication is async.
- **omnishd (daemon)** - Long-running process that receives I/O streams from all terminals via Unix socket, handles storage, command tracking, context building, LLM dispatch, and scheduled tasks (daily notes, hourly summaries, session eviction).
- **Communication** - Client and daemon communicate via `$XDG_RUNTIME_DIR/omnish.sock` by default. The transport layer is abstracted to support future TCP/HTTP for cross-machine aggregation.

## Client: PTY Proxy Layer

```
omnish client:
  1. Install bash hook (OSC 133 semantic prompts + readline reporter)
  2. forkpty() to spawn target shell (from $SHELL or config)
  3. Main loop (poll-based, stdin + PTY master):
     stdin:
       - Feed bytes to InputInterceptor
         - Buffering  -> render omnish prompt UI + ghost-text completion
         - Forward    -> write to PTY + track shell input + send IoData
         - Chat(msg)  -> dispatch command / daemon query / LLM query
         - Tab        -> accept ghost completion suffix
         - Cancel     -> dismiss omnish UI
       - Tab on shell ghost -> accept completion, inject suffix to PTY
     PTY output:
       - Write to stdout (strip OSC 133 for display)
       - Feed to OSC 133 detector -> state transitions (prompt, command, output)
       - Feed to command tracker -> produce CommandRecord on completion
       - Feed to alt-screen detector -> suppress interceptor in vim/htop/etc.
       - Throttled send to daemon as IoData
     Async channels:
       - Completion responses -> queue, wait for readline report, render ghost
       - Probe timer -> poll hostname, cwd, child process changes
  4. Terminal mode: enter raw mode, restore on exit (explicit Drop before exit)
```

### Shell Hook (bash)

The client generates a bash hook (`~/.local/share/omnish/hooks/bash_hook.sh`) that provides:

- **OSC 133 semantic prompts** - `133;A` (PromptStart), `133;B` (CommandStart with `$BASH_COMMAND`, `cwd`, and original user input from `history 1`), `133;C` (OutputStart), `133;D` (CommandEnd with exit code)
- **Readline reporter** - bound to `\e[13337~`, reports `READLINE_LINE` and `READLINE_POINT` via `133;RL` for accurate input tracking

### Input Interception

The `InputInterceptor` is a byte-level state machine with:

- **Configurable prefix** - `::` by default (from `config.shell.command_prefix`)
- **Time-gap guard** - only intercept if sufficient idle time elapsed (avoids catching mid-command `:` in vim, etc.)
- **Suppression** - disabled when `at_prompt=false` (child process running) or alt-screen active (TUI apps)
- **ESC sequence filter** - handles arrow keys, bracketed paste, function keys without breaking state
- **Chat mode** - once prefix matched, accumulates input until Enter, supports backspace and Tab completion

### Ghost-Text Completion

Two layers of ghost-text (dim inline suggestions):

1. **Shell completion** (`ShellCompleter`) - daemon-backed LLM completions for shell commands. Flow: input change → debounced request → daemon completes via LLM → response queued → readline report triggered → ghost rendered. Tab accepts by injecting suffix to PTY.
2. **Chat completion** (`GhostCompleter`) - local builtin provider for `/` commands in chat mode (e.g., type `/tem` → ghost shows `plate`). Tab accepts by injecting into interceptor buffer.

### Shell Input Tracking

`ShellInputTracker` maintains real-time state of the shell input line:

- `input` / `cursor_pos` - from readline reports (`133;RL`)
- `at_prompt` - true after PromptStart/CommandEnd, false after Enter key
- `sequence_id` - monotonic counter, used to match completion requests to input state
- `pending_rl_report` - prevents duplicate readline triggers

### Probes

`ProbeSet` polls shell environment on progressive intervals (1, 2, 4, 8, 15, 30, 60s), reset to 1s on command start:

- Hostname changes
- Shell working directory (via `/proc/{pid}/cwd`)
- Child process detection (for tmux window title + interceptor suppression)

### Event Log

Ring buffer (200 capacity) records key client events with elapsed-time prefixes for `/debug events` command:

- OSC 133 state transitions (PromptStart, CommandStart, CommandEnd, OutputStart)
- Readline request/response pairs (async trigger → OSC response)
- Completion request/response pairs
- Tab accepts, chat mode entry, command completions

### Key Design Points

- **Zero-interference principle** - PTY proxy is fully transparent. `cat`, `vim`, `htop`, `ssh`, and any program must behave identically. All daemon communication is async, never blocking the main I/O path.
- **Graceful degradation** - When daemon is unreachable, omnish still works as a normal shell. I/O messages are buffered (up to 10,000) and flushed on reconnect.
- **Alt-screen awareness** - Detects alternate screen transitions (vim, less, htop) to suppress I/O reporting and interceptor during TUI sessions.

## Command Tracker (omnish-tracker)

Detects command boundaries and builds `CommandRecord` structs from the I/O stream:

```
CommandTracker state machine:
  PromptStart → pending command created (seq incremented)
  CommandStart → entered=true, record $BASH_COMMAND + original input + cwd
  OutputStart → output collection begins
  CommandEnd → finalize: produce CommandRecord with exit code
```

Two detection modes:
- **OSC 133 mode** (primary) - uses semantic prompt markers from bash hook
- **Regex fallback** - prompt pattern detection for shells without OSC 133 support

### Command Line Resolution

Priority chain for `command_line` field: `original` (from `history 1`, preserves aliases) → `$BASH_COMMAND` (alias-expanded) → `extract_command_line()` (replay raw input bytes handling backspace, Ctrl-U, ESC sequences).

### Output Summary

Head/tail format (first 5 + last 5 lines) with ANSI stripped. Shell echo before Enter is excluded.

## Communication Layer

```rust
trait Transport: Send + Sync {
    async fn connect(&self, addr: &str) -> Result<Connection>;
    async fn listen(&self, addr: &str) -> Result<Listener>;
}

// Implementations
struct UnixSocketTransport;   // Local, default
struct TcpTransport;          // Cross-machine, future
struct HttpTransport;         // Cross-network/firewall, future
```

### Message Protocol

Binary framed format, decoupled from transport:

```
┌────────┬────────┬──────────┬─────────┐
│ magic  │ length │ msg_type │ payload │
│ 2B     │ 4B     │ 1B       │ ...     │
└────────┴────────┴──────────┴─────────┘
```

Payload serialized with bincode. Message types:

| Message | Direction | Purpose |
|---------|-----------|---------|
| `SessionStart` | client→daemon | Register session (attrs: shell, PID, tty) |
| `SessionEnd` | client→daemon | Session closed (exit code) |
| `SessionUpdate` | client→daemon | Metadata change (host, cwd, child_process) |
| `IoData` | client→daemon | I/O bytes (direction, timestamp, data) |
| `Event` | client→daemon | Structured event (NonZeroExit, PatternMatch, CommandBoundary) |
| `CommandComplete` | client→daemon | Finalized CommandRecord |
| `CompletionRequest` | client→daemon | Shell input for LLM completion |
| `CompletionResponse` | daemon→client | Completion suggestions (text + confidence) |
| `CompletionSummary` | client→daemon | Analytics: prompt, completion, accepted, latency |
| `Request` | client→daemon | LLM query (chat or command) |
| `Response` | daemon→client | LLM response (streaming support) |
| `Auth` / `Ack` / `AuthFailed` | bidirectional | Authentication handshake |

## Daemon: omnishd

```
omnishd
├── Session Manager    - manage lifecycle of all active sessions
├── Stream Store       - persist raw I/O streams + command records
├── Context Builder    - build LLM context from stored data
├── LLM Dispatcher     - dispatch LLM requests, manage backends
├── Task Manager       - cron-scheduled periodic tasks
│   ├── Daily Notes    - generate daily work summaries
│   ├── Hourly Summary - generate hourly activity digests
│   ├── Eviction       - clean up inactive sessions
│   └── Disk Cleanup   - remove old session data
└── Command Handler    - handle /debug, /context, /sessions, etc.
```

### Session Manager

Each client connection maps to a session with metadata (PID, tty, shell type, hostname, cwd, start time). Tracks:
- Active/ended state
- Stream writer for raw I/O persistence
- Per-session command records (via CommandTracker)
- Parent session relationships

### Stream Store

```
~/.local/share/omnish/sessions/
├── 2026-02-11T16:30:00_a1b2c3/
│   ├── meta.json           # session metadata + attributes
│   ├── stream.bin          # raw byte stream + timestamps
│   ├── commands.jsonl      # finalized CommandRecord entries
│   └── completions.jsonl   # completion interaction summaries
```

### Context Builder (omnish-context)

Orchestrates building LLM context from stored commands:

1. **Strategy selection** - `RecentCommands` selects N most recent commands across sessions
2. **Elastic window** - splits budget between "history" (command-line only, many commands) and "detailed" (full output, fewer commands), with minimum guarantees for current session
3. **Stream reading** - reads actual output bytes from `stream.bin` at recorded offsets
4. **Formatting** - strips ANSI, adds hostname/cwd labels, truncates long lines, formats as XML-tagged blocks (`<recent>` with `<cmd>` entries)

### Task Manager

Cron-scheduled tasks via tokio-cron-scheduler:

- **Daily notes** - generates work diary at configurable hour (default 18:00)
- **Hourly summary** - generates activity digest every hour
- **Eviction** - removes sessions inactive beyond configurable hours
- **Disk cleanup** - removes old session data on configurable cron schedule

## LLM Engine: Remote Multi-Backend

```rust
trait LlmBackend: Send + Sync {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse>;
    fn name(&self) -> &str;
    fn supports_streaming(&self) -> bool;
}

struct LlmRequest {
    messages: Vec<LlmMessage>,  // system + user messages
    model: String,
    max_tokens: Option<u32>,
}
```

### Backends

- `AnthropicBackend` - Claude API (native messages format)
- `OpenAiCompatBackend` - OpenAI-compatible API (covers OpenAI, DeepSeek, and other compatible services)

API keys resolved via `api_key_cmd` (e.g., `pass show anthropic/api-key`) - no plaintext keys in config.

### Use Cases

| UseCase | Purpose | Typical Backend |
|---------|---------|-----------------|
| `Completion` | Fast shell command suggestions | Smaller/faster model |
| `Analysis` | Daily notes, hourly summaries | Full model |
| `Chat` | Interactive chat mode queries | Full model |

### Prompt Templates

| Template | Purpose |
|----------|---------|
| `auto-complete` | Shell command completion (unified for empty and non-empty input, prefix-stable for KV cache warmup) |
| `chat` | Interactive chat with context (query variant + auto-analyze variant) |
| `daily-notes` | Daily work diary generation |
| `hourly-notes` | Hourly activity summary |

### Configuration

```toml
# ~/.config/omnish/config.toml

[client]
completion_enabled = true

[client.shell]
command = "/bin/bash"
command_prefix = "::"
intercept_gap_ms = 500
ghost_timeout_ms = 3000

[daemon]
listen_addr = "/run/user/1000/omnish.sock"

[daemon.llm]
default = "claude"

[daemon.llm.backends.claude]
backend_type = "anthropic"
model = "claude-sonnet-4-5-20250929"
api_key_cmd = "pass show anthropic/api-key"

[daemon.llm.backends.openai]
backend_type = "openai_compat"
model = "gpt-4o"
api_key_cmd = "pass show openai/api-key"
base_url = "https://api.openai.com/v1"

[daemon.llm.use_cases]
completion = "claude"
analysis = "claude"
chat = "claude"

[daemon.tasks.eviction]
inactive_hours = 24

[daemon.tasks.daily_notes]
schedule_hour = 18

[daemon.tasks.disk_cleanup]
cron = "0 0 3 * * *"
```

## User Interaction: Chat Mode

Omnish intercepts `::` prefixed input at shell prompt state (prefix configurable):

```bash
# LLM queries (chat mode)
::why did make fail just now              # analyze current terminal context
::explain this error                      # free-form question with context

# Slash commands (in chat mode)
::/context                                # show current LLM context
::/context chat                           # show context for chat template
::/template chat                          # show chat prompt template
::/template auto-complete                 # show completion template
::/version                                # show omnish version
::/sessions                               # list all active sessions
::/tasks                                  # list/manage scheduled tasks
::/debug events [N]                       # show last N client events (default 20)
::/debug client                           # show client debug state
::/debug session                          # show current session info
```

### Display Strategy

- Chat mode response → rendered inline with separator bar, then clears readline and restores pre-chat input
- Ghost-text completion → dim text after cursor, Tab to accept
- Alt-screen apps (vim, etc.) → interceptor suppressed, all input forwarded

## Project Structure

```
omnish/
├── Cargo.toml                 # workspace
├── crates/
│   ├── omnish-client/         # client binary: PTY proxy, interceptor, completion, probes
│   ├── omnish-daemon/         # daemon binary: server, session mgr, task scheduler
│   ├── omnish-tracker/        # command boundary detection (OSC 133 + regex fallback)
│   ├── omnish-context/        # LLM context building (strategy, formatting, stream reading)
│   ├── omnish-transport/      # Transport trait + Unix socket implementation
│   ├── omnish-protocol/       # message definitions, bincode serialization
│   ├── omnish-pty/            # PTY operations (forkpty, signals, raw mode)
│   ├── omnish-store/          # stream storage, session files, command/completion records
│   ├── omnish-llm/            # LlmBackend trait + Anthropic/OpenAI + prompt templates
│   └── omnish-common/         # shared types, config parsing, version
├── config/
│   └── default.toml           # default config template
└── docs/
    └── plans/
```

### Key Dependencies

- `nix` - PTY and signal handling (`"term"` feature for PTY support)
- `tokio` - async runtime (daemon server loop, client async I/O, channels)
- `serde` + `bincode` - protocol serialization
- `reqwest` - HTTP LLM API calls (`rustls-tls` feature to avoid OpenSSL dependency)
- `toml` - config parsing
- `tokio-cron-scheduler` - daemon task scheduling
- `uuid` - session ID generation

### Build Notes

- Requires `clang` for ring/rustls compilation
- `nix` 0.29: PTY support uses `"term"` feature (not `"pty"`)
- `std::process::exit()` skips Drop destructors - always explicitly drop RAII guards (like RawModeGuard) before calling it
