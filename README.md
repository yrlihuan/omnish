# omnish

AI-powered shell for software development. omnish wraps your terminal as a transparent PTY proxy, providing an inline coding assistant with full context of your development workflow - commands, outputs, errors, and project state across all terminals.

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

- **omnish** (client) - PTY proxy per terminal. Spawns your shell via `forkpty()`, forwards all I/O transparently, and sends a copy to the daemon.
- **omnishd** (daemon) - Aggregates sessions, stores streams, detects shell prompts to segment commands, runs scheduled tasks, dispatches LLM queries.

For detailed module documentation and implementation details, see the [module documentation](docs/implementation/).

## Features

- **Coding agent** - LLM agent with built-in tools: read/write/edit files, run shell commands, glob/grep search, stream results back inline. Ask it to fix a bug, refactor code, or explore unfamiliar codebases.
- **Ghost completion** - LLM-powered inline command suggestions as you type, context-aware of recent outputs and errors.
- **Chat mode** - Type `:` to ask about build failures, debug errors, or get explanations - with full context of what just happened across all terminals.
- **Zero interference** - All programs (vim, ssh, htop, etc.) behave identically. omnish is invisible until you need it.
- **Per-thread model selection** - Switch LLM backend mid-conversation with `/model`.
- **Multi-backend LLM** - Anthropic, OpenAI, DeepSeek, Moonshot, OpenRouter, or any OpenAI-compatible API.
- **Scheduled tasks** - Hourly/daily work summaries, thread summaries, session cleanup.
- **Auto-update** - Daemon periodically checks for new releases and distributes to client machines.
- **Cross-platform** - Linux and macOS.

## Installation

### From GitHub (downloads latest release)

```bash
curl -fsSL https://raw.githubusercontent.com/yrlihuan/omnish/master/install.sh | bash
```

### From a downloaded release

```bash
tar -xzf omnish-0.8.0-linux-x86_64.tar.gz
cd omnish-0.8.0-linux-x86_64
bash install.sh
```

### Build from source

Requires Rust toolchain and `clang` (for rustls/ring):

```bash
cargo build --release
```

## Configuration

All settings are managed through `~/.omnish/daemon.toml` (or `$OMNISH_DAEMON_CONFIG`). Client settings (hotkeys, completion, etc.) are stored in the `[client]` section and pushed to clients at connect time. Use `:config` inside omnish to edit settings interactively.

**Client-only config** - `~/.omnish/client.toml` (or `$OMNISH_CLIENT_CONFIG`):

Per-host settings (daemon address, sandbox preferences). Usually only needed for remote TCP connections:

```toml
daemon_addr = "~/.omnish/omnish.sock"  # or "server-ip:9800" for TCP
```

**Daemon config** - `~/.omnish/daemon.toml`:

```toml
listen_addr = "~/.omnish/omnish.sock"  # or "0.0.0.0:9800" for TCP

[client]
command_prefix = ":"        # prefix for chat mode (default: ":")
resume_prefix = "::"       # prefix to resume last thread (default: "::")
completion_enabled = true
ghost_timeout_ms = 10000    # ghost-text suggestion timeout

[llm]
default = "claude"

[llm.backends.claude]
backend_type = "anthropic"
model = "claude-opus-4-6"
api_key_cmd = "pass show anthropic/api-key"

[llm.backends.openai]
backend_type = "openai"
model = "gpt-4o"
api_key_cmd = "cat ~/.openai_api_key"
base_url = "https://api.openai.com/v1"

[llm.use_cases]
chat = "claude"
analysis = "claude"
completion = "claude"

[tasks.disk_cleanup]
schedule = "0 0 */6 * * *"

[tasks.auto_update]
enabled = true
# schedule = "0 0 4 * * *"  # default: 4 AM daily

[context.completion]
max_commands = 50
max_chars = 8000
```

### Other backend examples

```toml
# DeepSeek
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
> why did the build fail            # analyze compiler errors with full context
> fix the type error in main.rs     # agent edits the file directly
> what changed since yesterday      # cross-session development activity
> refactor this function to async   # agent reads, edits, and verifies
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
/config                   # interactive configuration menu
/integrate tmux           # add omnish as default shell in ~/.tmux.conf
/integrate screen         # add omnish as shell in ~/.screenrc
/integrate ssh            # show SSH config snippet for RemoteCommand
```

Chat-mode commands (available only inside a chat session):

```bash
/resume       # resume or start a conversation thread (interactive picker)
/model        # switch LLM backend for current thread (interactive picker)
/context      # show current chat context (system-reminder)
```

Results from auto-triggers appear above the shell prompt without disrupting your workflow.


## Storage

Session data is stored under `~/.omnish/`:

```
~/.omnish/
├── install.sh               # installer (also used for --upgrade)
├── deploy.sh                # client deployment script
├── client.toml              # per-host client config (daemon_addr, sandbox)
├── daemon.toml              # all configuration (daemon, client, LLM, tasks)
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
├── threads/                 # chat conversation threads
│   ├── <uuid>.jsonl         # raw LLM messages (JSONL)
│   └── <uuid>.meta.json     # thread metadata (model, summary)
├── sessions/
│   └── 2026-02-13T10-30-00_abc12345/
│       ├── meta.json        # session metadata
│       ├── stream.bin       # raw I/O stream (binary, timestamped)
│       ├── commands.jsonl   # segmented command records
│       └── completions.jsonl # completion interaction summaries
├── logs/
│   ├── messages/            # LLM request payloads (JSON, rolling 30)
│   ├── samples/             # completion quality samples (JSONL)
│   └── daemon.log.*         # daemon logs (daily rotation)
└── notes/
    ├── hourly/              # hourly activity summaries (YYYY-MM-DD/HH.md)
    └── 2026-03-03.md        # daily notes
```

## Workspace

| Crate | Purpose |
|-------|---------|
| `omnish-client` | PTY proxy binary, input interception, ghost completion, chat session, display |
| `omnish-daemon` | Session manager, agent loop, tool formatters, scheduled tasks, server |
| `omnish-transport` | Transport layer (Unix socket, TCP+TLS), RPC client/server, token auth |
| `omnish-protocol` | Binary framed message format (length + bincode) |
| `omnish-pty` | `forkpty()` wrapper, raw mode guard |
| `omnish-store` | Session metadata (JSON), stream storage (binary), command records |
| `omnish-llm` | LLM backend trait + Anthropic/OpenAI-compatible implementations |
| `omnish-common` | Shared config types, auth token utilities |
| `omnish-tracker` | Command tracker for shell command monitoring and analysis |
| `omnish-context` | Context builder for LLM prompt construction |
| `omnish-plugin` | Built-in coding tools (file I/O, shell, search) + external plugin host |

For detailed module documentation, see [`docs/implementation/`](docs/implementation/).

## Tests

```bash
cargo test --workspace
```

## License

MIT
