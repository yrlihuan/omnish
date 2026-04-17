# Chat Context Redesign

## Goal

Maximize KV cache hit rate on resume by ensuring the messages sent to the LLM API are byte-for-byte identical to the original conversation.

## Design Decisions

1. **Storage format**: Raw API message JSON (serde_json::Value), one per line in JSONL. Content can be String or Vec<ContentBlock>, matching Anthropic API format exactly.
2. **No backward compat**: Old format `.jsonl` files are ignored. Clean start.
3. **Single source of truth**: All display functions (get_last_exchange, /conversations, etc.) extract text from the raw JSON.
4. **Context injection**: Recent command list appended to the user's query as `<system-reminder>` - only affects the last user message, preserving KV cache prefix for all prior messages.
5. **Conversation replay**: All messages (including tool_use/tool_result) go through `LlmRequest.extra_messages`. `LlmRequest.conversation` is not used for chat.
6. **Backend unchanged**: `anthropic.rs` needs no changes.

## Storage Format

Each line in `{thread_id}.jsonl`:

```jsonl
{"role":"user","content":"问题\n\n<system-reminder>Recent commands:\n[seq=1] git status (exit 0, 2m ago)\n...</system-reminder>"}
{"role":"assistant","content":[{"type":"text","text":"让我查看..."},{"type":"tool_use","id":"toolu_1","name":"command_query","input":{"action":"get_output","seq":1}}]}
{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"...output..."}]}
{"role":"assistant","content":[{"type":"text","text":"最终回复"}]}
```

Message types are distinguishable by structure:
- **User input**: `role: "user"`, `content` is String
- **Tool result**: `role: "user"`, `content` is Array with `type: "tool_result"` elements
- **Assistant text**: `role: "assistant"`, `content` is String or Array with only `type: "text"`
- **Assistant tool_use**: `role: "assistant"`, `content` is Array containing `type: "tool_use"`

## ConversationManager API

Old:
- `append_exchange(thread_id, query, response)` - store Q&A text pair
- `load_messages(thread_id) -> Vec<ChatTurn>` - return text pairs
- `get_last_exchange(thread_id) -> (Option<(String, String)>, u32)`

New:
- `append_messages(thread_id, &[serde_json::Value])` - append raw API messages
- `load_raw_messages(thread_id) -> Vec<serde_json::Value>` - return messages for API replay
- `get_last_exchange(thread_id) -> (Option<(String, String)>, u32)` - same signature, extracts text from raw JSON internally

## handle_chat_message Flow

1. `load_raw_messages(thread_id)` → `Vec<Value>` → use as initial `extra_messages`
2. Build user message: `query + "\n\n<system-reminder>" + command_list + "</system-reminder>"` → append to `extra_messages`
3. Send to LLM (query=None since user message is already in extra_messages, conversation=empty)
4. Agent loop: append assistant response and tool_result messages to `extra_messages`
5. On completion: `append_messages(thread_id, &new_messages)` - store only the messages added in this turn

## System Prompt Update

Add to CHAT_SYSTEM_PROMPT Guidelines:

```
## Tools

You have access to the command_query tool to inspect command output:
- Use get_output(seq) to retrieve the full output of a specific command
- The recent command list is provided at the end of the user's message in <system-reminder>
- You do NOT need to call list_history - the command list is already provided
```

## Files Changed

| File | Change |
|------|--------|
| `conversation_mgr.rs` | Storage format → raw JSON, new API methods, extract text from JSON for display |
| `server.rs` (handle_chat_message) | Use `load_raw_messages`, append `<system-reminder>` to query, store with `append_messages` |
| `template.rs` | Update `CHAT_SYSTEM_PROMPT` with Tools section |
| `anthropic.rs` | No change |

## Not in scope

- Conversation compaction/summarization (future issue)
- Old format backward compatibility
- Backend layer changes
