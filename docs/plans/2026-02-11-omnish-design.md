# omnish Design

A transparent shell wrapper that captures I/O from running programs, aggregates sessions across multiple terminals (and future multi-machine), and integrates with remote LLMs for analysis.

## Architecture Overview

```
┌─────────────────────────────────────────────┐
│                 omnishd (daemon)             │
│  ┌──────────┐ ┌──────────┐ ┌─────────────┐  │
│  │ Session  │ │ Storage  │ │ LLM Engine  │  │
│  │ Manager  │ │ (stream) │ │(multi-backend)│ │
│  └────▲─────┘ └──────────┘ └─────────────┘  │
│       │  Unix Socket (abstracted)            │
├───────┼─────────────────────────────────────┤
│  ┌────┴─────┐  ┌──────────┐  ┌──────────┐   │
│  │ omnish   │  │ omnish   │  │ omnish   │   │
│  │ (tty 1)  │  │ (tty 2)  │  │ (tty 3)  │   │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘   │
│       │ PTY          │ PTY         │ PTY     │
│  ┌────┴─────┐  ┌────┴─────┐  ┌────┴─────┐   │
│  │  bash    │  │  zsh     │  │ ssh ...  │   │
│  └──────────┘  └──────────┘  └──────────┘   │
└─────────────────────────────────────────────┘
```

- **omnish (client)** — User's login shell or shell entry point. Uses `forkpty()` to spawn the real shell (bash/zsh etc), acting as a transparent PTY proxy. All I/O bytes are forwarded to the daemon asynchronously.
- **omnishd (daemon)** — Long-running process that receives I/O streams from all terminals via Unix socket, handles storage, event detection, and LLM dispatch.
- **Communication** — Client and daemon communicate via `$XDG_RUNTIME_DIR/omnish.sock` by default. The transport layer is abstracted to support future TCP/HTTP for cross-machine aggregation.

## Client: PTY Proxy Layer

```
omnish client:
  1. forkpty() to spawn target shell (from $SHELL or config)
  2. Main loop:
     - User input -> check if omnish special command (e.g. ::analyze)
       - Yes -> intercept and handle
       - No  -> write to PTY master as-is
     - PTY master output -> write to stdout as-is + async send to daemon
     - SIGWINCH -> sync PTY slave window size
     - SIGCHLD -> child shell exited, cleanup and exit
  3. Terminal mode: enter raw mode, restore on exit
```

### Key Design Points

- **Zero-interference principle** — PTY proxy is fully transparent. `cat`, `vim`, `htop`, `ssh`, and any program must behave identically. All daemon communication is async, never blocking the main I/O path.
- **Special command prefix** — `::` as escape prefix (configurable). E.g. `::ask why did that fail`. Only intercepted at line start in shell prompt state, to avoid interfering with normal input.
- **Graceful degradation** — When daemon is unreachable, omnish still works as a normal shell. Only analysis capabilities are lost.

## Communication Layer Abstraction

```rust
trait Transport: Send + Sync {
    async fn connect(&self, addr: &str) -> Result<Connection>;
    async fn listen(&self, addr: &str) -> Result<Listener>;
}

trait Connection: Send + Sync {
    async fn send(&self, msg: &Message) -> Result<()>;
    async fn recv(&self) -> Result<Message>;
}

// Implementations
struct UnixSocketTransport;   // Local, default
struct TcpTransport;          // Cross-machine, future
struct HttpTransport;         // Cross-network/firewall, future
```

### Message Protocol

Binary format, decoupled from transport:

```
┌────────┬────────┬──────────┬─────────┐
│ magic  │ length │ msg_type │ payload │
│ 2B     │ 4B     │ 1B       │ ...     │
└────────┴────────┴──────────┴─────────┘
```

Core message types:
- `SessionStart` — client registers new session (shell type, PID, tty info)
- `IoData` — I/O byte stream report (direction in/out, timestamp, raw bytes)
- `Event` — structured events (command completed, non-zero exit code, etc.)
- `Request` — client requests to daemon (e.g. LLM query)
- `Response` — daemon replies to client

## Daemon: omnishd

```
omnishd
├── Session Manager    — manage lifecycle of all active sessions
├── Stream Store       — persist raw I/O streams
├── Event Detector     — detect trigger events from streams
├── LLM Dispatcher     — dispatch LLM requests, manage backends
└── Query Handler      — handle active query requests from clients
```

### Session Manager

Each client connection maps to a session with metadata (PID, tty, shell type, start time). Session is marked ended when client disconnects, history preserved.

### Stream Store

I/O streams written to filesystem:

```
~/.local/share/omnish/sessions/
├── 2026-02-11T16:30:00_a1b2c3/
│   ├── meta.json        # session metadata
│   ├── stream.bin        # raw byte stream + timestamps (scriptreplay-like format)
│   └── events.jsonl      # detected events
```

### Event Detector

Lightweight parsing of I/O streams to identify:
- Non-zero exit codes (detect `$?` in shell prompt)
- stderr key patterns (`error`, `panic`, `traceback`, `fatal`, etc. — configurable)
- Command boundaries (via prompt detection)
- User-defined custom rules

### LLM Dispatcher

On event trigger or user request:
- Extract relevant context from stream store (last N lines, full output of current command, etc.)
- Send to configured LLM backend
- Push results back to corresponding client for display, or store in session directory

## LLM Engine: Remote Multi-Backend

```rust
trait LlmBackend: Send + Sync {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse>;
    fn name(&self) -> &str;
    fn supports_streaming(&self) -> bool;
}

struct LlmRequest {
    context: Vec<IoSegment>,   // relevant I/O segments
    query: Option<String>,     // user question (for manual ::ask)
    trigger: TriggerType,      // Manual / AutoError / AutoPattern
    session_ids: Vec<String>,  // involved sessions (cross-terminal aggregation)
}
```

### Backends

- `AnthropicBackend` — Claude API
- `OpenAiCompatBackend` — OpenAI-compatible API (covers OpenAI, DeepSeek, and other compatible services)

### Configuration

```toml
# ~/.config/omnish/config.toml

[llm]
default = "claude"

[llm.claude]
backend = "anthropic"
model = "claude-sonnet-4-5-20250929"
api_key_cmd = "pass show anthropic/api-key"   # no plaintext keys

[llm.openai]
backend = "openai_compat"
model = "gpt-4o"
api_key_cmd = "pass show openai/api-key"
base_url = "https://api.openai.com/v1"        # override for compatible services

[llm.auto_trigger]
on_nonzero_exit = true
on_stderr_patterns = ["error", "panic", "traceback", "fatal"]
cooldown_seconds = 5   # prevent rapid repeated triggers
```

### Context Construction

Before sending to LLM: extract relevant content from raw stream, strip meaningless escape sequence noise, retain sufficient context while controlling token usage. Cross-terminal queries via `::ask -a` can reference data from multiple sessions.

## User Interaction: Special Commands

Omnish intercepts `::` prefixed commands at shell prompt state (prefix configurable):

```bash
# LLM queries
::ask why did make fail just now              # analyze current terminal recent output
::ask -a what are these terminals doing       # -a aggregate all active sessions
::ask -s 3 compare these two deployments      # -s specify session lookback count

# Session management
::sessions                                    # list all active sessions
::replay a1b2c3                               # replay specified session

# Control
::status                                      # daemon status, connections, LLM backend
::config llm.default openai                   # runtime switch LLM
::pause                                       # pause I/O reporting for current session
::resume                                      # resume reporting
```

### Display Strategy

Auto-triggered LLM results must not interrupt user's current operation:
- User at prompt waiting for input -> insert analysis above prompt
- User in interactive program (vim etc.) -> buffer result, show after returning to prompt, or send desktop notification
- Manual `::ask` -> block and stream result to terminal

## Project Structure

```
omnish/
├── Cargo.toml                 # workspace
├── crates/
│   ├── omnish-client/         # client binary: PTY proxy + :: command interception
│   ├── omnish-daemon/         # daemon binary: omnishd
│   ├── omnish-transport/      # Transport trait + Unix/TCP/HTTP implementations
│   ├── omnish-protocol/       # message definitions, serialization/deserialization
│   ├── omnish-pty/            # PTY operations (forkpty, signals, raw mode)
│   ├── omnish-store/          # stream storage, session file management
│   ├── omnish-llm/            # LlmBackend trait + Anthropic/OpenAI implementations
│   └── omnish-common/         # shared types, config parsing, logging
├── config/
│   └── default.toml           # default config template
└── docs/
    └── plans/
```

### Key Dependencies

- `nix` / `rustix` — PTY and signal handling
- `tokio` — async runtime (daemon and client async I/O)
- `serde` + `bincode` — protocol serialization
- `reqwest` — HTTP LLM API calls
- `toml` — config parsing
