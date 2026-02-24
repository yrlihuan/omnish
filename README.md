# omnish

Transparent shell wrapper that captures terminal I/O across multiple sessions and integrates with LLMs for context-aware analysis.

omnish sits between you and your shell as a PTY proxy. It records everything, segments commands, and lets you query an LLM about what just happened — across any number of terminals.

## Architecture

```
┌──────────────────────────────────────────────┐
│              omnishd (daemon)                │
│  ┌──────────┐  ┌─────────┐  ┌────────────┐  │
│  │ Session  │  │ Storage  │  │ LLM Engine │  │
│  │ Manager  │  │ (stream) │  │ (backends) │  │
│  └────▲─────┘  └─────────┘  └────────────┘  │
│       │  Unix Socket                         │
├───────┼──────────────────────────────────────┤
│  ┌────┴────┐   ┌─────────┐   ┌─────────┐    │
│  │ omnish  │   │ omnish  │   │ omnish  │    │
│  │ (tty 1) │   │ (tty 2) │   │ (tty 3) │    │
│  └────┬────┘   └────┬────┘   └────┬────┘    │
│       │ PTY         │ PTY         │ PTY      │
│  ┌────┴────┐   ┌────┴────┐   ┌────┴────┐    │
│  │  bash   │   │  zsh    │   │ ssh ... │    │
│  └─────────┘   └─────────┘   └─────────┘    │
└──────────────────────────────────────────────┘
```

- **omnish** (client) — PTY proxy per terminal. Spawns your shell via `forkpty()`, forwards all I/O transparently, and sends a copy to the daemon.
- **omnishd** (daemon) — Aggregates sessions, stores streams, detects shell prompts to segment commands, dispatches LLM queries.

For detailed module documentation and implementation details, see the [module documentation](docs/implementation/).

## Features

- **Zero interference** — All programs (vim, ssh, htop, etc.) behave identically. The proxy is fully transparent.
- **Graceful degradation** — Works as a normal shell when the daemon is unavailable.
- **Command recording** — Detects shell prompts to segment continuous I/O into individual commands with metadata and output summaries.
- **Multi-session aggregation** — Query context from multiple terminals at once.
- **Multi-backend LLM** — Anthropic (Claude), OpenAI, Azure, local models (Ollama/LM Studio) via OpenAI-compatible API.
- **Auto-trigger** — Optionally analyze on non-zero exit codes or stderr patterns.

## Build

Requires Rust toolchain and `clang` (for rustls/ring):

```bash
cargo build --release
```

Produces two binaries:
- `target/release/omnish-client` — the shell wrapper
- `target/release/omnish-daemon` — the daemon

A diagnostic tool is also built:
- `target/release/omnish-commands` — list recorded commands from stored sessions

## Configuration

Create `~/.config/omnish/config.toml` (or set `$OMNISH_CONFIG`):

```toml
[shell]
command_prefix = ":"    # prefix for omnish commands (default: ":")

[daemon]
# socket_path = "/run/user/1000/omnish.sock"  # default: $XDG_RUNTIME_DIR/omnish.sock

[llm]
default = "claude"

[llm.backends.claude]
backend_type = "anthropic"
model = "claude-sonnet-4-5-20250929"
api_key_cmd = "pass show anthropic/api-key"

[llm.auto_trigger]
on_nonzero_exit = true
on_stderr_patterns = ["error", "panic", "traceback", "fatal"]
cooldown_seconds = 5
```

### Other backend examples

```toml
# OpenAI
[llm.backends.openai]
backend_type = "openai-compat"
model = "gpt-4"
api_key_cmd = "cat ~/.openai_api_key"
base_url = "https://api.openai.com/v1"

# Local (Ollama, LM Studio)
[llm.backends.local]
backend_type = "openai-compat"
model = "llama3"
api_key_cmd = "echo dummy"
base_url = "http://localhost:1234/v1"
```

## Usage

Start the daemon, then use `omnish` as your shell:

```bash
omnishd &
omnish
```

Inside any omnish session, type `:` to enter chat mode, then you can directly interact with the LLM:

```bash
:why did make fail just now        # analyze recent output
:what are all my terminals doing   # cross-session query
```

Debug commands available in chat mode:
```bash
/debug context    # show current session context
/debug template   # show LLM prompt template
```

Results from auto-triggers appear above the shell prompt without disrupting your workflow.

### omnish-commands

Inspect recorded commands across sessions:

```bash
omnish-commands              # last 20 commands
omnish-commands -n 50        # last 50
omnish-commands -s abc123    # filter by session ID prefix
```

## Storage

Session data is stored under `~/.local/share/omnish/sessions/`:

```
~/.local/share/omnish/sessions/
└── 2026-02-13T10-30-00_abc12345/
    ├── meta.json        # session metadata
    ├── stream.bin       # raw I/O stream (binary, timestamped)
    ├── commands.json    # segmented command records
    └── events.jsonl     # detected events
```

## Workspace

omnish 项目由以下 11 个 crate 组成：

| Crate | Purpose |
|-------|---------|
| `omnish-client` | PTY proxy binary, input interception, display |
| `omnish-daemon` | Session manager, prompt detection, command tracking, server |
| `omnish-transport` | Abstracted transport layer (Unix socket now, TCP/HTTP later) |
| `omnish-protocol` | Binary framed message format (magic + length + bincode) |
| `omnish-pty` | `forkpty()` wrapper, raw mode guard |
| `omnish-store` | Session metadata (JSON), stream storage (binary), command records |
| `omnish-llm` | LLM backend trait + Anthropic/OpenAI-compatible implementations |
| `omnish-common` | Shared config types |
| `omnish-tracker` | Command tracker for shell command monitoring and analysis |
| `omnish-context` | Context builder for LLM prompt construction |
| `shell-prompt-state-tracking` | Technical explanation of shell prompt state tracking |

**详细模块文档**：每个模块的详细说明文档可在 [`docs/implementation/`](docs/implementation/) 目录中找到，包含模块概述、重要数据结构、关键函数说明、使用示例和依赖关系。

## Tests

```bash
cargo test --workspace
```

## License

MIT
