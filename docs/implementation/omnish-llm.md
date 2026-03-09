# omnish-llm 模块

**功能:** LLM后端抽象和实现

## 模块概述

omnish-llm 提供LLM后端抽象，支持多种LLM提供商，包括Anthropic、OpenAI兼容API。模块通过统一的接口封装不同LLM提供商的API调用，提供一致的LLM交互体验。从v0.5.0开始，支持工具调用（tool-use）功能，使LLM能够主动调用外部工具完成任务。

## 重要数据结构

### 工具调用相关类型（Tool-use）

#### `Tool` trait
定义工具接口（来自`tool.rs`）：
- `definition()`: 返回工具定义（`ToolDef`），包含名称、描述和JSON schema
- `execute()`: 执行工具并返回结果（`ToolResult`）

#### `ToolDef`
工具定义结构体，描述一个可供LLM调用的工具：
- `name`: 工具名称
- `description`: 工具描述
- `input_schema`: JSON schema定义（`serde_json::Value`），描述工具输入参数

#### `ToolCall`
LLM请求的工具调用：
- `id`: 工具调用ID（用于关联结果）
- `name`: 工具名称
- `input`: 工具输入参数（`serde_json::Value`）

#### `ToolResult`
工具执行结果：
- `tool_use_id`: 对应的工具调用ID
- `content`: 执行结果内容（字符串）
- `is_error`: 是否为错误结果

### LLM后端接口

#### `LlmBackend` trait
LLM后端接口，定义所有LLM后端必须实现的方法：
- `complete()`: 发送补全请求并获取响应
- `name()`: 返回后端名称标识符
- `max_content_chars()`: 返回该后端模型的上下文最大字符数（可选，默认None）
- `max_content_chars_for_use_case()`: 根据用途返回上下文字符数限制（可选，默认返回max_content_chars()）

#### `LlmRequest`
LLM请求结构体，包含发送给LLM的完整请求信息：
- `context`: 终端会话上下文
- `query`: 用户查询（可选）
- `trigger`: 触发类型（手动、自动错误检测、自动模式检测）
- `session_ids`: 相关会话ID列表
- `use_case`: 请求用途（用于选择合适的模型）
- `max_content_chars`: 模型上下文最大字符数限制（可选，用于限制上下文大小）
- `conversation`: 多轮对话历史（`Vec<ChatTurn>`，用于chat模式）
- `system_prompt`: 系统提示词（可选，chat模式使用`CHAT_SYSTEM_PROMPT`）
- `enable_thinking`: 思考模式开关（可选，`Some(false)`禁用自动补全的思考模式）
- `tools`: 工具定义列表（`Vec<ToolDef>`），提供给LLM的可用工具
- `extra_messages`: 额外消息（`Vec<serde_json::Value>`），用于agent循环中的tool_use和tool_result交换

#### `LlmResponse`
LLM响应结构体，包含LLM返回的结果：
- `content`: 响应内容块列表（`Vec<ContentBlock>`）
- `stop_reason`: 停止原因（`StopReason`枚举）
- `model`: 使用的模型名称
- `thinking`: 思考内容（可选，来自支持思考模式的模型）

辅助方法：
- `text()`: 提取所有文本块并用换行符连接，方便不使用tool-use的调用者
- `tool_calls()`: 提取所有工具调用（`Vec<&ToolCall>`）

#### `ContentBlock` 枚举
响应内容块类型：
- `Text(String)`: 文本内容
- `ToolUse(ToolCall)`: 工具调用请求

#### `StopReason` 枚举
LLM停止生成的原因：
- `EndTurn`: 正常结束
- `ToolUse`: 需要调用工具
- `MaxTokens`: 达到最大token限制

#### `UseCase` 枚举
请求用途，用于选择合适的模型后端：
- `Completion`: 自动命令补全
- `Analysis`: 分析任务（每日/每小时总结等）
- `Chat`: 多轮对话

#### `TriggerType` 枚举
触发类型枚举，表示LLM请求的触发方式：
- `Manual`: 手动触发（用户显式请求）
- `AutoError`: 自动错误检测触发
- `AutoPattern`: 自动模式检测触发

### LLM后端实现

#### `AnthropicBackend`
Anthropic API后端实现：
- 支持Claude模型系列
- 实现`LlmBackend` trait
- 使用Anthropic Messages API (v1/messages，API版本2024-04-04）
- 支持`base_url`配置（默认api.anthropic.com），可用于代理或自托管
- 多轮对话支持：conversation历史映射为messages数组，上下文注入第一条user消息
- 系统提示词支持：通过Anthropic `system` 顶层字段
- 思考模式：`enable_thinking == Some(false)` 时发送禁用思考参数
- 工具调用支持：通过`tools`字段提供工具定义，解析响应中的`tool_use` content blocks
- `strip_thinking()` 辅助函数解析content blocks（thinking vs text vs tool_use）

#### `OpenAiCompatBackend`
OpenAI兼容API后端实现：
- 支持OpenAI、Azure OpenAI、本地兼容API（如vLLM）
- 实现`LlmBackend` trait
- 使用OpenAI兼容的Chat Completions API
- 多轮对话支持：conversation历史映射为messages数组
- 系统提示词支持：作为 `role: "system"` 消息前置
- 思考模式：通过 `chat_template_kwargs` 传递 `enable_thinking: false`（适配vLLM/Qwen3）
- 工具调用支持：通过`tools`字段提供工具定义，解析响应中的`tool_calls`
- `extract_thinking()` 辅助函数解析响应中的 `<think>` 标签

#### `MultiBackend`
多后端路由实现：
- 根据 `UseCase` 将请求路由到不同的后端实例
- 支持为 Completion、Analysis、Chat 分别配置不同的模型

### 配置结构

#### `LlmBackendConfig`
LLM后端配置结构体（来自omnish-common）：
- `backend_type`: 后端类型（"anthropic" 或 "openai-compat"）
- `model`: 模型名称
- `api_key_cmd`: 获取API密钥的命令
- `base_url`: API基础URL（anthropic支持自定义，openai-compat必需）
- `max_content_chars`: 该模型的上下文最大字符数（可选，用于限制上下文大小）

## 关键函数说明

### `LlmBackend::complete()`
发送LLM补全请求并获取响应。

**参数:** `req: &LlmRequest` - LLM请求结构体
**返回:** `Result<LlmResponse>` - LLM响应或错误
**用途:** 主要的LLM交互接口，处理API调用、错误处理和响应解析

### `create_backend()`
根据配置创建LLM后端实例。

**参数:**
- `_name: &str` - 后端名称（当前未使用）
- `config: &LlmBackendConfig` - 后端配置

**返回:** `Result<Arc<dyn LlmBackend>>` - 装箱的LLM后端实例
**用途:** 工厂函数，根据配置类型创建对应的后端实现

### `create_default_backend()`
从完整LLM配置创建默认后端。

**参数:** `llm_config: &LlmConfig` - 完整的LLM配置
**返回:** `Result<Arc<dyn LlmBackend>>` - 默认后端实例
**用途:** 简化后端创建，自动使用配置中的默认后端

### `resolve_api_key()`
通过命令解析API密钥。

**参数:** `api_key_cmd: &Option<String>` - 获取API密钥的命令
**返回:** `Result<String>` - 解析出的API密钥
**用途:** 安全地获取API密钥，支持通过命令动态获取

### `build_user_content()`
构建发送给LLM的用户内容。

**参数:**
- `context: &str` - 终端会话上下文
- `query: Option<&str>` - 用户查询（可选）

**返回:** `String` - 格式化后的用户内容
**用途:** 根据是否有查询构建不同的提示模板

### `build_simple_completion_content()`
构建shell命令补全的统一提示内容（用于KV cache前缀稳定性）。

**参数:**
- `context: &str` - 终端会话上下文（XML格式，包含`<recent>`等标签，工作目录包裹在`<system-reminder>`中）
- `input: &str` - 当前输入的命令
- `cursor_pos: usize` - 光标位置

**返回:** `String` - 格式化后的补全提示
**用途:** 统一空输入和非空输入的模板，指令+上下文形成稳定前缀，仅末尾的 `Current input:` 行变化。返回JSON数组格式 `["cmd1", "cmd2"]`，最多2个建议。第二个建议优先使用完整命令（issue #93）。禁止建议 `&&` 链式命令除非用户输入中已包含（issue #95）。此设计使LLM服务器可在连续请求间复用KV cache。

**上下文格式说明:**
- 历史命令和最近命令使用原有的`<history>`和`<recent>`标签
- 当前工作目录单独包裹在`<system-reminder>`标签中（commit 458db9f），格式为：`<system-reminder>\n# workingDirectory\n{path}\n</system-reminder>`
- Claude等模型对`<system-reminder>`标签有特殊训练，可提升理解效果

### `prompt_template()`
获取提示模板。

**参数:** `has_query: bool` - 是否有用户查询
**返回:** `&'static str` - 提示模板字符串
**用途:** 返回包含占位符的提示模板

### 常量

- `DAILY_NOTES_PROMPT` — 每日工作总结的LLM提示（中文），使用XML标签 `<commands>` 包裹上下文，输出bullet列表格式
- `HOURLY_NOTES_PROMPT` — 每小时工作总结的LLM提示（中文），使用XML标签 `<commands>`、`<hourly_summaries>` 包裹上下文（issue #96）
- `CHAT_SYSTEM_PROMPT` — 聊天模式系统提示词，包含以下内容：
  - omnish chat模式介绍
  - 可用命令列表（/help, /resume, /thread list, /thread del, /context, /sessions等）
  - 工具使用说明（command_query工具可查询命令输出）
  - 更新记录：移除了/new、/chat、/ask命令（issue #152），更改/threads为/thread list和/thread del（issue #163），移除/conversations别名（commit b2f5a6f）
- `TEMPLATE_NAMES` — 已知模板名列表：`["chat", "chat-system", "auto-complete", "daily-notes", "hourly-notes"]`

### `template_by_name()`
根据名称返回模板内容（用于 `/template <name>` 命令）。

**参数:** `name: &str` - 模板名称
**返回:** `Option<String>` - 模板内容或None
**用途:**
- `auto-complete`: 使用 `build_simple_completion_content()` 渲染两种变体（空输入/有输入）
- `chat-system`: 返回 `CHAT_SYSTEM_PROMPT`
- `chat`: 由daemon处理，返回提示信息让用户使用 `/template chat` daemon请求（可显示实际工具定义）

## 使用示例

### 基本使用（无工具调用）
```rust
use omnish_llm::{create_default_backend, LlmBackend};
use omnish_common::config::LlmConfig;

// 从配置创建默认后端
let backend = create_default_backend(&llm_config).await?;

// 构建LLM请求
let request = LlmRequest {
    context: "终端会话上下文...".to_string(),
    query: Some("用户查询".to_string()),
    trigger: TriggerType::Manual,
    session_ids: vec![],
    use_case: UseCase::Chat,
    max_content_chars: None,
    conversation: vec![],     // 多轮对话历史
    system_prompt: None,      // 系统提示词
    enable_thinking: None,    // 思考模式
    tools: vec![],            // 工具定义
    extra_messages: vec![],   // agent循环消息
};

// 发送请求并获取响应
let response = backend.complete(&request).await?;
println!("LLM响应: {}", response.text());
```

### 工具调用示例
```rust
use omnish_llm::tool::{Tool, ToolDef, ToolResult};
use serde_json::json;

// 1. 实现Tool trait
struct MyTool;

impl Tool for MyTool {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: "my_tool".to_string(),
            description: "执行某个操作".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "param": {"type": "string"}
                },
                "required": ["param"]
            }),
        }
    }

    fn execute(&self, input: &serde_json::Value) -> ToolResult {
        ToolResult {
            tool_use_id: String::new(),
            content: format!("结果: {:?}", input),
            is_error: false,
        }
    }
}

// 2. 构建带工具的LLM请求
let tool = MyTool;
let request = LlmRequest {
    context: "上下文".to_string(),
    query: Some("请使用工具".to_string()),
    trigger: TriggerType::Manual,
    session_ids: vec![],
    use_case: UseCase::Chat,
    max_content_chars: None,
    conversation: vec![],
    system_prompt: None,
    enable_thinking: None,
    tools: vec![tool.definition()],  // 提供工具定义
    extra_messages: vec![],
};

// 3. 发送请求并处理响应
let response = backend.complete(&request).await?;

// 检查是否有工具调用
if response.stop_reason == StopReason::ToolUse {
    for tool_call in response.tool_calls() {
        println!("LLM请求调用工具: {}", tool_call.name);
        let result = tool.execute(&tool_call.input);
        println!("工具执行结果: {}", result.content);
        // 通常需要将结果返回给LLM继续agent循环
    }
} else {
    println!("LLM文本响应: {}", response.text());
}
```

### 创建特定后端
```rust
use omnish_llm::{create_backend, LlmBackend};
use omnish_common::config::LlmBackendConfig;

// 配置Anthropic后端
let config = LlmBackendConfig {
    backend_type: "anthropic".to_string(),
    model: "claude-3-5-sonnet-20241022".to_string(),
    api_key_cmd: Some("echo $ANTHROPIC_API_KEY".to_string()),
    base_url: Some("https://api.anthropic.com".to_string()), // 可选，支持自定义
    max_content_chars: Some(200000),
};

// 创建后端
let backend = create_backend("anthropic", &config)?;

// 使用后端...
```

### 构建提示内容
```rust
use omnish_llm::template;

// 构建带查询的提示
let user_content = template::build_user_content(
    "$ ls\nfile1.txt\nfile2.txt",
    Some("有哪些文件？")
);

// 构建命令补全提示
let completion_content = template::build_simple_completion_content(
    "<recent>...</recent>",
    "git comm",
    8
);
```

## 依赖关系
- `omnish-common`: 配置类型定义
- `omnish-protocol`: ChatTurn类型（用于多轮对话）
- `reqwest`: HTTP客户端，用于API调用
- `serde`/`serde_json`: JSON序列化和反序列化
- `anyhow`: 错误处理
- `async-trait`: 异步trait支持
- `tracing`: 日志记录
- `std::process::Command`: 执行命令获取API密钥
- `std::sync::{Arc, RwLock}`: 线程安全的引用计数和读写锁

## 模块文件结构
- `lib.rs`: 模块入口，导出所有公共接口
- `backend.rs`: LlmBackend trait、LlmRequest/LlmResponse、UseCase/TriggerType等核心类型
- `tool.rs`: Tool trait和工具相关类型（ToolDef、ToolCall、ToolResult）
- `anthropic.rs`: Anthropic API后端实现
- `openai_compat.rs`: OpenAI兼容API后端实现
- `factory.rs`: 后端工厂函数（create_backend、create_default_backend等）
- `template.rs`: 提示模板（CHAT_SYSTEM_PROMPT、build_simple_completion_content等）

## 配置示例
```toml
# omnish.toml 中的LLM配置
[llm]
default = "anthropic"

[llm.backends.anthropic]
backend_type = "anthropic"
model = "claude-3-5-sonnet-20241022"
api_key_cmd = "echo $ANTHROPIC_API_KEY"
# base_url = "https://api.anthropic.com"  # 可选，默认为api.anthropic.com
max_content_chars = 200000

[llm.backends.openai]
backend_type = "openai-compat"
model = "gpt-4"
api_key_cmd = "echo $OPENAI_API_KEY"
base_url = "https://api.openai.com/v1"  # openai-compat必需
max_content_chars = 128000

[llm.use_cases]
completion = "local-vllm"
analysis = "anthropic"
chat = "anthropic"
```

## 错误处理
模块使用`anyhow::Result`进行错误处理，包括：
- API调用失败（HTTP错误、网络问题）
- 响应格式错误（缺少必需字段）
- 配置错误（缺少必需参数）
- 命令执行失败（获取API密钥失败）

## 测试
模块包含完整的单元测试，覆盖：
- API密钥解析
- 后端创建
- 提示模板构建（前缀稳定性验证）
- chat系统提示词与用户可见命令同步验证
- thinking标签的提取和剥离
- 错误处理场景

## 更新历史

### v0.5.0 - 工具调用支持（2026-03）

**主要新功能:**
1. **工具调用（Tool-use）系统** (commit 467041e, 9e84c65):
   - 新增`Tool` trait定义工具接口
   - 新增`ToolDef`、`ToolCall`、`ToolResult`类型
   - `LlmRequest`新增`tools`和`extra_messages`字段
   - `LlmResponse`改用`ContentBlock`枚举支持文本和工具调用
   - 新增`StopReason`枚举区分停止原因
   - 两个后端（Anthropic和OpenAI-compat）均支持工具调用

2. **模板和上下文改进**:
   - `CHAT_SYSTEM_PROMPT`新增工具使用说明（commit 8d0ec9f）
   - `/template chat`移至daemon处理，显示实际工具定义（commit 5a0f0f9）
   - 自动补全上下文中工作目录包裹在`<system-reminder>`标签（commit 458db9f）

3. **后端改进**:
   - Anthropic后端支持`base_url`配置（commit 01ad6e8）
   - Anthropic API版本升级至2024-04-04（支持工具调用）
   - `LlmResponse::text()`用换行符连接多文本块（commit 135d372）

4. **命令变更**:
   - 移除`/new`、`/chat`、`/ask`命令（commit 48beea5）
   - `/threads`改为`/thread list`和`/thread del`（commit 096b094）
   - 移除`/conversations`别名（commit b2f5a6f）

**相关issue:** #161 (agent tool-use), #152 (remove /new /chat /ask), #163 (/thread subcommands), #167 (system-reminder tags)