# omnish Architecture Overview

## Project Overview

omnish is a terminal assistant with LLM integration that operates on a client-daemon architecture. The system captures terminal sessions, provides intelligent auto-completion, chat capabilities with tool-use functionality, and scheduled analysis tasks. It combines terminal emulation, command tracking, LLM orchestration, and plugin extensibility into a cohesive system.

## Core Architecture

### 1. **Client-Daemon Model**

The system follows a modular client-server architecture:

- **Client** (`omnish-client`): Handles user interaction, PTY management, and UI rendering (inline notices, status line)
- **Daemon** (`omnish-daemon`): Manages sessions, LLM integration, plugin system, and scheduled tasks
- **Transport Layer** (`omnish-transport`): RPC communication over Unix sockets or TCP with TLS encryption
- **Protocol** (`omnish-protocol`): Binary serialization protocol (bincode) with version negotiation

### 2. **Command Tracking & Session Management**

- **Tracker** (`omnish-tracker`): Uses OSC 133 terminal control sequences for precise command boundary detection, combined with regex-based shell prompt detection
- **PTY Management** (`omnish-pty`): Creates pseudo-terminals with RAII-style raw mode management for transparent shell interaction
- **Storage** (`omnish-store`): SQLite-based storage for command records, session metadata, and binary stream data with completion sampling

### 3. **LLM Integration & Context Management**

- **LLM Backend** (`omnish-llm`): Abstract interface supporting multiple providers (Anthropic, OpenAI-compatible APIs) with tool-use capabilities
- **Context Building** (`omnish-context`): Formats terminal history into LLM prompts with KV cache optimization using stable term labels
- **PromptManager**: Composable system prompt fragments with user override support and plugin extensibility
- **Common Utilities** (`omnish-common`): Shared configuration structures and utility functions

## Key Technical Features

### **Agent Loop & Tool Execution System**
- LLM initiates tool calls → Daemon forwards to client → Client executes → Results returned to continue agent loop
- Supports both server-side (bash execution) and client-side (file read/edit) tool execution
- Landlock sandboxing applied to client-side tool execution on Linux systems
- Tool definitions via `tool.json` metadata files in plugin directories

### **Conversation Management**
- Thread-based chat history stored in JSONL format with context preservation
- Multi-turn conversations with automatic context window management
- Agent loops can be paused/resumed while waiting for client-side tool results
- Conversation metadata includes timestamps, tool usage, and token counts

### **Auto-Completion with Quality Sampling**
- Collects LLM suggestion quality metrics: acceptance rate, dwell time, latency
- Levenshtein similarity calculation for analyzing suggestion relevance
- KV cache optimization through stable context formatting prefixes
- Completion suggestions limited to 2 items with intelligent ranking

### **Scheduled Task System**
- Daily/hourly session summaries generated via cron-based scheduling
- Session eviction based on age and inactivity thresholds
- Disk cleanup to manage storage usage and prevent unbounded growth
- Task execution with error handling and retry logic

### **Security Model**
- **Unix sockets**: File permission 0600 with peer UID validation
- **TCP+TLS**: Self-signed certificates stored in `~/.omnish/tls/`
- **Landlock sandboxing**: For client-side tool execution on supported Linux kernels
- **Authentication**: Token-based auth with protocol version negotiation
- **Plugin isolation**: Separate processes with configurable sandboxing

## Module Interactions

```
┌─────────────┐    Protocol    ┌─────────────┐
│   Client    │◄──────────────►│   Daemon    │
│             │  (Unix/TCP)    │             │
├─────────────┤                ├─────────────┤
│ PTY Proxy   │                │ LLM Backend │
│ Tracker     │                │ Plugins     │
│ UI/Notices  │                │ Store       │
│             │                │ Scheduler   │
└─────────────┘                └─────────────┘
```

### **Data Flow**
1. **Session Initialization**: Client creates PTY, sends `SessionStart` to daemon
2. **Command Execution**: User input → PTY → Shell output → Tracker detects boundaries
3. **Context Preparation**: Recent commands formatted with `<system-reminder>` tags for LLM
4. **LLM Processing**: Daemon routes requests based on use case (completion/analysis/chat)
5. **Tool Execution**: LLM tool calls forwarded to client via `ChatToolCall`, results returned via `ChatToolResult`
6. **Storage Persistence**: Command records, conversations, and metadata saved to SQLite/JSONL

## Configuration Architecture

### **Primary Configuration Files**
- `~/.omnish/omnish.toml`: Main configuration with client, daemon, shell, and LLM settings
- `~/.omnish/chat.json`: Default system prompt fragments (installed from binary assets)
- `~/.omnish/chat.override.json`: User-overridden prompt fragments
- `~/.omnish/tool.override.json`: Tool-specific prompt overrides
- `~/.omnish/auth_token`: Authentication token for client-daemon communication

### **LLM Configuration**
- Multi-backend support with routing by use case (completion, analysis, chat)
- API key management via command execution for secure retrieval
- Model-specific context length limits and thinking mode configuration
- Langfuse observability integration for request tracing

## Version Evolution

### **v0.5.0 (2026-03) - Tool-use System**
- Added comprehensive tool-use system with agent loop
- Introduced plugin architecture with `tool.json` metadata
- Implemented client-side tool execution forwarding
- Added conversation thread management

### **v0.6.0 - Observability & Robustness**
- **Langfuse integration**: Request tracing and token usage monitoring
- **PromptManager**: Composable prompt fragments with override support
- **Request logging**: LLM payload storage for debugging and auditing
- **429/529 retry**: Automatic retry with exponential backoff for rate limits
- **Usage statistics**: Token usage parsing from API responses

## Design Principles

### **Performance Optimization**
- KV cache stability through consistent context formatting
- Streaming responses with multi-message protocol support
- Efficient binary serialization with bincode
- Asynchronous I/O with Tokio runtime

### **Reliability & Robustness**
- Automatic reconnection with exponential backoff
- Connection failure detection and cleanup
- Error handling with graceful degradation
- Scheduled maintenance tasks

### **Extensibility**
- Plugin system with metadata-driven tool discovery
- Configurable prompt system with user overrides
- Multi-LLM backend support with use case routing
- Modular architecture with clear separation of concerns

## Module Documentation

For detailed information on each module, refer to:
- [omnish-common](omnish-common.md) - Shared configuration and utilities
- [omnish-protocol](omnish-protocol.md) - Communication protocol definitions
- [omnish-transport](omnish-transport.md) - RPC transport layer
- [omnish-pty](omnish-pty.md) - PTY management and raw mode
- [omnish-store](omnish-store.md) - Data storage and sampling
- [omnish-context](omnish-context.md) - Context building for LLM prompts
- [omnish-llm](omnish-llm.md) - LLM backend abstraction and tool-use
- [omnish-tracker](omnish-tracker.md) - Command tracking and OSC 133 detection
- [omnish-client](omnish-client.md) - Client implementation and UI
- [omnish-daemon](omnish-daemon.md) - Daemon server with plugin management

## Development Notes

### **Integration Testing**
- Tests located in `tools/integration_tests/`
- Use `lib.sh` for common test utilities
- `test_basic.sh` demonstrates basic test patterns

### **Issue Management**
- Use `glab` for GitLab issue management:
  - View issues with comments: `glab api projects/dev%2Fomnish/issues/<id>/notes`
  - Add comment: `glab issue note <id> -m "comment"`
  - Close issue: `glab issue close <id>`

### **Documentation Updates**
- Follow patterns in [how_to_update_docs.md](how_to_update_docs.md)
- Maintain consistency across module documentation
- Update this overview when architectural changes occur