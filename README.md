# omnish

Transparent shell wrapper that captures terminal I/O across multiple sessions and integrates with LLMs for context-aware analysis.

omnish sits between you and your shell as a PTY proxy. It records everything, segments commands, and lets you query an LLM about what just happened — across any number of terminals.

## Architecture

```
┌──────────────────────────────────────────────┐
│              omnishd (daemon)                │
│  ┌──────────┐  ┌─────────┐  ┌────────────┐   │
│  │ Session  │  │ Storage │  │ LLM Engine │   │
│  │ Manager  │  │ (stream)│  │ (backends) │   │
│  └────▲─────┘  └─────────┘  └────────────┘   │
│       │  Unix Socket / TCP+TLS               │
├───────┼──────────────────────────────────────┤
│  ┌────┴────┐   ┌─────────┐   ┌─────────┐     │
│  │ omnish  │   │ omnish  │   │ omnish  │     │
│  │ (tty 1) │   │ (tty 2) │   │ (tty 3) │     │
│  └────┬────┘   └────┬────┘   └────┬────┘     │
│       │ PTY         │ PTY         │ PTY      │
│  ┌────┴────┐   ┌────┴────┐   ┌────┴────┐     │
│  │  bash   │   │  zsh    │   │ ssh ... │     │
│  └─────────┘   └─────────┘   └─────────┘     │
└──────────────────────────────────────────────┘
```

- **omnish** (client) — PTY proxy per terminal. Spawns your shell via `forkpty()`, forwards all I/O transparently, and sends a copy to the daemon.
- **omnishd** (daemon) — Aggregates sessions, stores streams, detects shell prompts to segment commands, runs scheduled tasks, dispatches LLM queries.

For detailed module documentation and implementation details, see the [module documentation](docs/implementation/).

## Features

- **Zero interference** — All programs (vim, ssh, htop, etc.) behave identically.
- **Ghost completion** — LLM-powered inline command suggestions as you type.
- **Chat mode** — Type `:` to ask the LLM about recent terminal activity across all sessions.
- **Agent with tools** — LLM can run shell commands, query plugins, and stream results back to the terminal.
- **Multi-backend LLM** — Anthropic, OpenAI, DeepSeek, Moonshot, OpenRouter, or any OpenAI-compatible API.
- **Scheduled tasks** — Hourly/daily work summaries, session cleanup.
- **Auto-update** — Daemon periodically checks for new releases and distributes to client machines.
- **Cross-platform** — Linux and macOS.

## Installation

### From GitHub (downloads latest release)

```bash
curl -fsSL https://raw.githubusercontent.com/yrlihuan/omnish/master/install.sh | bash
```

### From a downloaded release

```bash
tar -xzf omnish-0.6.6-linux-x86_64.tar.gz
cd omnish-0.6.6-linux-x86_64
bash install.sh
```

### Build from source

Requires Rust toolchain and `clang` (for rustls/ring):

```bash
cargo build --release
```

## Configuration

Configuration uses two files under `~/.omnish/`:

**Client config** — `~/.omnish/client.toml` (or `$OMNISH_CLIENT_CONFIG`):

```toml
daemon_addr = "~/.omnish/omnish.sock"  # or "server-ip:9800" for TCP
completion_enabled = true

[shell]
command = "/bin/bash"
command_prefix = ":"        # prefix for chat mode (default: ":")
resume_prefix = "::"       # prefix to resume last thread (default: "::")
intercept_gap_ms = 1000     # min ms between inputs to trigger interception
ghost_timeout_ms = 10000    # ghost-text suggestion timeout
```

**Daemon config** — `~/.omnish/daemon.toml` (or `$OMNISH_DAEMON_CONFIG`):

```toml
listen_addr = "~/.omnish/omnish.sock"  # or "0.0.0.0:9800" for TCP

[llm]
default = "claude"

[llm.backends.claude]
backend_type = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_cmd = "pass show anthropic/api-key"

[llm.backends.openrouter]
backend_type = "openai"
model = "Qwen/Qwen2.5-Coder-32B-Instruct"
api_key_cmd = "cat ~/.openrouter_api_key"
base_url = "https://openrouter.ai/api/v1"

[llm.use_cases]
chat = "claude"
analysis = "claude"
completion = "openrouter"

[llm.auto_trigger]
on_nonzero_exit = true
on_stderr_patterns = ["error", "panic", "traceback", "fatal"]
cooldown_seconds = 5

[tasks.eviction]
session_evict_hours = 48

[tasks.daily_notes]
schedule_hour = 18

[tasks.disk_cleanup]
schedule = "0 0 */6 * * *"

[tasks.auto_update]
enabled = true
# schedule = "0 0 4 * * *"  # default: 4 AM daily
# clients = ["user@host1", "user@host2"]

[context.completion]
max_commands = 50
max_chars = 8000

[plugins]
enabled = []   # list plugin names; each must have ~/.omnish/plugins/{name}/{name} binary
```

### Other backend examples

```toml
# OpenAI
[llm.backends.openai]
backend_type = "openai"
model = "gpt-4o"
api_key_cmd = "cat ~/.openai_api_key"
base_url = "https://api.openai.com/v1"

# DeepSeek (Anthropic-compatible)
[llm.backends.deepseek]
backend_type = "anthropic"
model = "deepseek-chat"
api_key_cmd = "cat ~/.deepseek_api_key"
base_url = "https://api.deepseek.com/anthropic"

# Local (Ollama, LM Studio)
[llm.backends.local]
backend_type = "openai"
model = "llama3"
api_key_cmd = "echo dummy"
base_url = "http://localhost:1234/v1"
```


## Usage

The daemon is managed via systemd (set up by `install.sh`):

```bash
systemctl --user start omnish-daemon    # start
systemctl --user status omnish-daemon   # check status
journalctl --user -u omnish-daemon -f   # follow logs
```

Then use `omnish` as your shell:

```bash
omnish
```

Inside any omnish session, type `:` to enter chat mode, then you can directly interact with the LLM:

```bash
:why did make fail just now        # analyze recent output
:what are all my terminals doing   # cross-session query
```

Built-in commands:

```bash
/help                     # show available commands
/context                  # show completion context (default)
/context chat             # show chat/analysis context
/context daily-notes      # show daily summary context (past 24 hours)
/context hourly-notes     # show hourly summary context (past hour)
/template <name>          # show prompt template (chat, auto-complete, daily-notes, hourly-notes)
/debug events [n]         # show recent client events (default: 20)
/debug client             # show client debug state
/debug session            # show session info and attributes
/sessions                 # list active sessions
/thread list              # list conversation threads
/thread del <n>[,<n>...]  # delete thread(s) by number (interactive multi-select if omitted)
/tasks                    # list scheduled tasks and their status
/tasks disable <name>     # disable a scheduled task
/integrate tmux           # add omnish as default shell in ~/.tmux.conf
/integrate screen         # add omnish as shell in ~/.screenrc
/integrate ssh            # show SSH config snippet for RemoteCommand
```

Chat-mode commands (available only inside a chat session):

```bash
/resume       # resume or start a conversation thread (interactive picker)
```

Results from auto-triggers appear above the shell prompt without disrupting your workflow.


## Storage

Session data is stored under `~/.omnish/`:

```
~/.omnish/
├── install.sh               # installer (also used for --upgrade)
├── deploy.sh                # client deployment script
├── client.toml              # client configuration
├── daemon.toml              # daemon configuration
├── omnish.sock              # Unix domain socket
├── auth_token               # shared auth token (0600)
├── tls/                     # TLS cert and key for TCP mode
│   ├── cert.pem
│   └── key.pem
├── bin/                     # binaries
│   ├── omnish
│   ├── omnish-daemon
│   └── omnish-plugin
├── plugins/
│   └── builtin/
│       ├── tool.json        # built-in tool definitions
│       └── tool.override.json.example
├── prompts/
│   ├── chat.json            # chat prompt template
│   └── chat.override.json.example
├── sessions/
│   └── 2026-02-13T10-30-00_abc12345/
│       ├── meta.json        # session metadata
│       ├── stream.bin       # raw I/O stream (binary, timestamped)
│       ├── commands.jsonl   # segmented command records
│       └── completions.jsonl # completion interaction summaries
└── notes/
    ├── hourly/              # hourly activity summaries (YYYY-MM-DD/HH.md)
    └── 2026-03-03.md        # daily notes
```

## Workspace

| Crate | Purpose |
|-------|---------|
| `omnish-client` | PTY proxy binary, input interception, ghost completion, display |
| `omnish-daemon` | Session manager, scheduled tasks, prompt detection, command tracking, server |
| `omnish-transport` | Transport layer (Unix socket, TCP+TLS), RPC client/server, token auth |
| `omnish-protocol` | Binary framed message format (length + bincode) |
| `omnish-pty` | `forkpty()` wrapper, raw mode guard |
| `omnish-store` | Session metadata (JSON), stream storage (binary), command records |
| `omnish-llm` | LLM backend trait + Anthropic/OpenAI-compatible implementations |
| `omnish-common` | Shared config types, auth token utilities |
| `omnish-tracker` | Command tracker for shell command monitoring and analysis |
| `omnish-context` | Context builder for LLM prompt construction |
| `omnish-plugin` | Plugin host for external JSON-RPC tool subprocess |

For detailed module documentation, see [`docs/implementation/`](docs/implementation/).

## Tests

```bash
cargo test --workspace
```

## License

MIT
