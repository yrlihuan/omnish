# Omnish Configuration

## Quick Start

1. Copy the example config to your config directory:
```bash
mkdir -p ~/.config/omnish
cp config/example-openai.toml ~/.config/omnish/config.toml
```

2. Edit `~/.config/omnish/config.toml` and configure your LLM backend

3. Set up API key access (choose one method):

### Method 1: Store key in a file
```bash
echo "sk-your-api-key-here" > ~/.openai_api_key
chmod 600 ~/.openai_api_key
```

Then in config:
```toml
api_key_cmd = "cat ~/.openai_api_key"
```

### Method 2: Use environment variable
```bash
export OPENAI_API_KEY="sk-your-api-key-here"
```

Then in config:
```toml
api_key_cmd = "echo $OPENAI_API_KEY"
```

### Method 3: Use password manager
```bash
# Example with pass
api_key_cmd = "pass show openai/api-key"

# Example with 1password CLI
api_key_cmd = "op read op://Private/OpenAI/api-key"
```

## Backend Types

### anthropic
Direct Anthropic API (Claude models)
```toml
[llm.backends.claude]
backend_type = "anthropic"
model = "claude-3-5-sonnet-20241022"
api_key_cmd = "cat ~/.anthropic_api_key"
```

### openai-compat
Any OpenAI-compatible API endpoint
```toml
[llm.backends.openai]
backend_type = "openai-compat"
model = "gpt-4"
api_key_cmd = "cat ~/.openai_api_key"
base_url = "https://api.openai.com/v1"
```

**Supported providers:**
- OpenAI API (`https://api.openai.com/v1`)
- Azure OpenAI (`https://your-resource.openai.azure.com/openai/deployments/model-name`)
- Local LLM servers (LM Studio, Ollama with openai plugin, etc.)
- Any other OpenAI-compatible API

## Configuration Reference

### [llm]
- `default`: Name of the default backend to use (must match a key in `backends`)

### [llm.backends.<name>]
- `backend_type`: Either `"anthropic"` or `"openai-compat"`
- `model`: Model name/ID
- `api_key_cmd`: Shell command to retrieve API key
- `base_url`: (openai-compat only) API endpoint URL without `/chat/completions`

### [llm.auto_trigger]
- `on_nonzero_exit`: Auto-trigger LLM on command failure (boolean)
- `on_stderr_patterns`: Trigger on stderr matching patterns (list of strings)
- `cooldown_seconds`: Minimum seconds between auto-triggers

## Environment Variables

- `OMNISH_CONFIG`: Override config file path
- `OMNISH_SOCKET`: Override daemon socket path
- `SHELL`: Default shell command (if not specified in config)
- `XDG_RUNTIME_DIR`: Used for default socket path

## Usage Examples

### Basic usage
```bash
# Start daemon
omnish-daemon

# Start client (in another terminal)
omnish-client

# Ask about last command
::ask why did that fail

# Ask with all sessions context
::ask -a what have I been doing
```

### Testing without LLM
If no LLM backend is configured, the daemon will still run in passthrough mode,
recording sessions but responding with "(LLM backend not configured)" to ::ask commands.
