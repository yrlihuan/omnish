# Claude Code Langfuse Hooks 脚本分析

## 概述

这是一套 Claude Code + Langfuse 可观测性集成的 hook 脚本系统，由 `langfuse-cli` 安装，用于将 Claude Code 的会话交互自动追踪上报到 Langfuse（LLM 可观测性平台）。

---

## 脚本职责

### 1. `langfuse_utils.py` — 公共工具库

所有 hook 脚本的共享基础模块，提供：

- **配置管理**：状态目录、日志文件、锁文件等路径常量，从环境变量读取凭据（`LANGFUSE_PUBLIC_KEY`/`SECRET_KEY`）
- **日志**：写入 `~/.claude/state/langfuse_hook.log`，支持 debug/info/warn/error 级别
- **环境检测**：`tracing_enabled()` 检查 `TRACE_TO_LANGFUSE=true`
- **Hook payload 解析**：从 stdin 读取 JSON，提取 `session_id`、`transcript_path`
- **Git 辅助**：获取 repo root、remote URL、commit SHA，构建 GitHub commit URL
- **用户身份识别**：从 `~/.claude.json` 或 `claude auth status` 获取用户邮箱
- **文件 I/O**：原子写入 JSON、trace 状态文件的读写
- **Trace manifest**：在仓库的 `.langfuse/traces/` 下写入 trace 元数据清单

### 2. `langfuse_session_init_hook.py` — PreToolUse hook（会话初始化）

- **触发时机**：每次工具调用前（PreToolUse）
- **功能**：在会话的首次工具调用时，生成确定性的 `trace_id`（基于 `session_id` 通过 Langfuse SDK 或 SHA-256 派生），写入 `~/.claude/state/langfuse_last_trace.json`。后续同一 session 的调用检测到已有记录后直接跳过
- **目的**：让 `prepare-commit-msg` hook 能立即引用到 trace ID，而不必等到 Stop hook 运行之后

### 3. `langfuse_hook.py` — Stop hook（核心追踪引擎）

- **触发时机**：Claude Code 会话结束时（Stop hook）
- **功能**：
  1. 增量读取会话 transcript 文件（JSONL 格式），通过文件偏移量实现断点续读
  2. 解析消息为 Turn（一个 user 提问 + assistant 回复 + tool 调用/结果）
  3. 构建 ChatML 格式的输入/输出，将每个 Turn 上报为 Langfuse 的 span + generation + tool observations
  4. 记录工具调用详情（名称、输入、输出、耗时、是否出错），Bash 工具还会提取命令前缀
  5. 附加 git 元数据（commit SHA、commit URL、分支）和用户身份
  6. 使用文件锁（`fcntl`）防止并发写入冲突
  7. 持久化处理进度到 `~/.claude/state/langfuse_state.json`

### 4. `langfuse_git_commit_hook.py` — PostToolUse hook（Git 提交检测）

- **触发时机**：每次 Bash 工具调用后（PostToolUse）
- **功能**：
  1. 检查 Bash 命令是否是 `git commit`（通过正则匹配，支持链式命令如 `cd repo && git commit`）
  2. 确认命令执行成功且 HEAD 确实发生了变化
  3. 将 commit 元数据（SHA、message、branch、remote URL）写入 `.langfuse/traces/` 的 trace manifest
  4. 生成 Agent Trace Record（记录被修改文件、关联的 trace URL），写入 `.langfuse/traces/agent-trace-{sha}.json`
- **目的**：将 git commit 与 Langfuse trace 关联，实现"哪个 AI 会话产生了哪个 commit"的溯源

### 5. `langfuse_prepare_commit_msg.py` — Git prepare-commit-msg hook

- **触发时机**：`git commit` 时，Git 自动调用
- **功能**：自动在 commit message 末尾追加 `Langfuse-Session: <url>` trailer，链接到对应的 Langfuse session 页面。只在 trace 文件存在且不超过 4 小时时生效，跳过 merge/squash commit

---

## 整体数据流

```
Claude Code 会话开始
  → [PreToolUse] session_init_hook: 生成 trace_id，写入状态文件
  → 用户与 Claude 交互...
  → [PostToolUse] git_commit_hook: 检测 git commit，记录 commit↔trace 映射
  → [git hook] prepare_commit_msg: 给 commit message 加 Langfuse 链接
  → [Stop] langfuse_hook: 增量解析 transcript，批量上报所有 Turn 到 Langfuse
```

---

## Transcript 文件结构分析

### 文件格式

JSONL（每行一个 JSON 对象），路径通过 hook payload 的 `transcriptPath` 字段传入。

### 每条消息的顶层字段

| 字段 | 来源代码 | 说明 |
|------|----------|------|
| `type` | `msg.get("type")` — 值为 `"user"` 或 `"assistant"` | 消息类型/角色 |
| `timestamp` | `parse_timestamp()` — ISO 格式字符串 | 消息时间戳 |
| `version` | `get_version()` | Claude Code 版本号 |
| `message` | 嵌套对象，包含实际消息内容 | 消息体 |

### `message` 嵌套对象的字段

```jsonc
{
  "type": "assistant",
  "timestamp": "2026-03-08T10:00:00Z",
  "version": "1.2.3",
  "message": {
    "id": "msg_01XFDUDYJgAACzvnptvVoYEL",   // 消息唯一 ID
    "role": "assistant",                      // 或 "user"
    "model": "claude-sonnet-4-20250514",      // 使用的模型
    "content": [...]                          // 消息内容
  }
}
```

### `content` 内容类型

#### 文本块（Text）

```jsonc
{ "type": "text", "text": "这是 Claude 的回复文本..." }
```

#### 工具调用块（Tool Use）— 仅 assistant 消息

```jsonc
{
  "type": "tool_use",
  "id": "toolu_01abc123",
  "name": "Bash",                  // Bash, Read, Edit, Write, Grep, Glob, Agent 等
  "input": {
    "command": "git status"
  }
}
```

#### 工具结果块（Tool Result）— 仅 user 消息

```jsonc
{
  "type": "tool_result",
  "tool_use_id": "toolu_01abc123",
  "content": "...",
  "is_error": false
}
```

### 一个完整 Turn 的消息序列

```
┌─ User 消息 (type=user)
│    content: [{ type: "text", text: "帮我写个函数" }]
│
├─ Assistant 消息 (type=assistant)  ← 可能有多条（流式多段回复，按 message.id 去重取最新）
│    content: [
│      { type: "text", text: "好的，让我来..." },
│      { type: "tool_use", id: "toolu_1", name: "Write", input: {...} }
│    ]
│
├─ User 消息 (type=user, 实际是工具结果)
│    content: [
│      { type: "tool_result", tool_use_id: "toolu_1", content: "文件已写入", is_error: false }
│    ]
│
├─ Assistant 消息 (type=assistant)
│    content: [
│      { type: "text", text: "文件已创建，现在运行测试..." },
│      { type: "tool_use", id: "toolu_2", name: "Bash", input: { command: "npm test" } }
│    ]
│
├─ User 消息 (工具结果)
│    content: [
│      { type: "tool_result", tool_use_id: "toolu_2", content: "Tests passed", is_error: false }
│    ]
│
├─ Assistant 消息 (最终回复)
│    content: [{ type: "text", text: "所有测试通过！" }]
│
└─ 下一个 User 消息 → 触发 flush，结束当前 Turn
```

### 关键细节

1. **工具结果伪装为 user 消息**：工具执行结果以 `role=user` 的消息发送，但内容是 `tool_result` 类型。不会触发新 Turn，而是归入当前 Turn 的 `tool_results_by_id` 字典
2. **Assistant 消息去重**：同一个 `message.id` 可能出现多次（流式传输中的增量更新），只保留最新版本
3. **被拒绝/失败的工具调用**：`is_error=true` 或找不到对应 `tool_result` 时，标记为 `level="ERROR"`
4. **Bash 工具特殊处理**：提取命令第一个词，在 Langfuse 中显示为 `Tool: Bash (git)` 等

---

## Transcript 的性质（澄清）

Transcript **不是** Claude API 原始请求/响应流的直接记录，而是 **Claude Code 客户端的本地会话日志**。主要区别：

1. **有额外包装层**：外层有 `type`、`timestamp`、`version` 等 Claude Code 添加的字段，API 消息体嵌套在 `message` 字段内
2. **缺失 API 级别信息**：
   - `usage`（input_tokens / output_tokens）— 无 token 用量
   - `stop_reason`（end_turn / tool_use / max_tokens）— 无停止原因
   - system prompt — 无系统提示词
   - `temperature`、`max_tokens` 等请求参数 — 无采样配置
3. **工具结果是本地记录**：时间戳反映本地执行完成时间，而非 API 调用时间
4. **流式更新导致重复**：同一 `message.id` 可能出现多次（流式响应的多次快照）

---

## Compact（上下文压缩）在 Transcript 中的表现

解析代码中没有任何 compact/summary/compress 相关的处理逻辑。存在两种可能：

- **可能性 A**：Compact 结果不记录在 transcript 中。Compact 是客户端内部的上下文管理操作，只影响内存中的消息列表，不写入 transcript。此时 transcript 保留 compact 之前的原始完整对话
- **可能性 B**：Compact 结果以某种特殊 type 记录，但被 hook 忽略。`get_role()` 只识别 `"user"` 和 `"assistant"`，其他 type 的消息会被 `build_turns()` 静默跳过

Transcript 文件是 append-only 的（增量读取逻辑基于 `seek` 偏移量），compact 不会修改已写入的历史消息。

> 如需验证，可在触发 compact 后检查实际 transcript 文件中是否出现特殊 type 的消息记录。
