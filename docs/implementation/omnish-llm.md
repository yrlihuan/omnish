# omnish-llm 模块

**功能:** LLM后端抽象和实现

## 模块概述

omnish-llm 提供LLM后端抽象，支持多种LLM提供商，包括Anthropic、OpenAI兼容API。模块通过统一的接口封装不同LLM提供商的API调用，提供一致的LLM交互体验。从v0.5.0开始，支持工具调用（tool-use）功能，使LLM能够主动调用外部工具完成任务。v0.6.0新增了Langfuse可观测性集成、可组合的系统提示词管理（PromptManager）、请求日志记录、429/529重试机制等功能。

## 重要数据结构

### 工具调用相关类型（Tool-use）

#### `ToolDef`
工具定义结构体，描述一个可供LLM调用的工具：
- `name`: 工具名称
- `description`: 工具描述
- `input_schema`: JSON schema定义（`serde_json::Value`），描述工具输入参数

> 注：v0.5.0中的`Tool` trait已在v0.6.0中移除，工具简化为固有方法（inherent methods）实现。

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
- `system_prompt`: 系统提示词（可选，chat模式使用`PromptManager`构建）
- `enable_thinking`: 思考模式开关（可选，`Some(true)`启用思考模式，`Some(false)`禁用，`None`使用后端默认）
- `tools`: 工具定义列表（`Vec<ToolDef>`），提供给LLM的可用工具
- `extra_messages`: 额外消息（`Vec<serde_json::Value>`），用于agent循环中的tool_use和tool_result交换

#### `LlmResponse`
LLM响应结构体，包含LLM返回的结果：
- `content`: 响应内容块列表（`Vec<ContentBlock>`）
- `stop_reason`: 停止原因（`StopReason`枚举）
- `model`: 使用的模型名称
- `thinking`: 思考内容（可选，来自支持思考模式的模型）
- `usage`: token使用统计（可选，`Usage`结构体）

辅助方法：
- `text()`: 提取所有文本块并用换行符连接，方便不使用tool-use的调用者
- `tool_calls()`: 提取所有工具调用（`Vec<&ToolCall>`）

#### `Usage`
token使用统计结构体，从API响应中解析：
- `input_tokens`: 输入token数
- `output_tokens`: 输出token数
- `cache_read_input_tokens`: KV cache读取的token数（Anthropic: `cache_read_input_tokens`，OpenAI: `cached_tokens`）
- `cache_creation_input_tokens`: KV cache写入的token数（Anthropic特有）

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

### PromptManager（系统提示词管理）

可组合的系统提示词片段管理器，将系统提示词拆分为具名片段（fragment），支持插入顺序拼接和覆盖合并。

**核心方法：**
- `new()`: 创建空的PromptManager
- `add(name, content)`: 添加具名片段，按插入顺序排列
- `build()`: 将所有片段用 `\n\n` 连接，生成最终系统提示词
- `from_json(json)`: 从JSON数组加载片段（`[{name, content}]`，content支持字符串或字符串数组）
- `merge(overrides)`: 合并覆盖片段——同名片段替换，新片段追加
- `default_chat()`: 从编译内嵌的`chat.json`创建默认chat提示词管理器

**提示词片段格式：**
```json
[
  {"name": "identity", "content": ["行1", "行2"]},
  {"name": "tone", "content": "单行内容"}
]
```

**chat提示词覆盖机制：**
- 基础提示词：`assets/chat.json`编译内嵌到二进制文件中，启动时安装到`~/.omnish/chat.json`
- 用户覆盖：`~/.omnish/chat.override.json`，同名片段替换基础片段，新名片段追加
- 工具覆盖：`~/.omnish/tool.override.json`（原`prompt.json`重命名）

**插件提示词片段：**
插件可通过系统提供自己的提示词片段，这些片段会被合并到PromptManager中。

### LLM后端实现

#### `AnthropicBackend`
Anthropic API后端实现：
- 支持Claude模型系列
- 实现`LlmBackend` trait
- 使用Anthropic Messages API (v1/messages，API版本2024-04-04）
- 支持`base_url`配置（默认api.anthropic.com），可用于代理或自托管
- `max_tokens`固定为8192
- 多轮对话支持：conversation历史映射为messages数组，上下文注入第一条user消息
- 系统提示词支持：通过Anthropic `system` 顶层字段
- 思考模式：`enable_thinking == Some(true)` 时启用（budget_tokens: 4096），`Some(false)` 时发送禁用参数
- 工具调用支持：通过`tools`字段提供工具定义，解析响应中的`tool_use` content blocks
- `strip_thinking()` 辅助函数解析content blocks（thinking vs text vs tool_use）
- 429/529自动重试：最多3次重试，指数退避（默认5s起步，最大60s），支持解析`retry-after`响应头
- Usage解析：从API响应中提取`input_tokens`、`output_tokens`、`cache_read_input_tokens`、`cache_creation_input_tokens`
- 请求日志：Chat请求的完整payload记录到`~/.omnish/logs/messages/`

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
- 429自动重试：最多3次重试，指数退避，支持解析`retry-after`响应头
- Usage解析：从API响应中提取`prompt_tokens`→`input_tokens`、`completion_tokens`→`output_tokens`、`cached_tokens`→`cache_read_input_tokens`
- 请求日志：Chat请求的完整payload记录到`~/.omnish/logs/messages/`

#### `MultiBackend`
多后端路由实现：
- 根据 `UseCase` 将请求路由到不同的后端实例
- 支持为 Completion、Analysis、Chat 分别配置不同的模型
- 创建时自动解析Langfuse配置并包装各后端

#### `LangfuseBackend`（可观测性）
Langfuse可观测性包装器，透明地为LLM调用添加追踪：
- 以装饰器模式包装任意`LlmBackend`实现
- 每次`complete()`调用后异步发送trace和generation事件到Langfuse `/api/public/ingestion` API
- 记录信息包括：模型名称、use_case、请求输入摘要、输出文本、工具调用数、延迟、错误状态
- 当有`Usage`数据时，上报input/output token数和cache统计
- 使用Basic Auth认证（public_key + secret_key）
- fire-and-forget模式：发送失败不影响LLM调用结果
- 配置可选，未配置时不包装后端

### 请求日志（message_log）

LLM请求payload本地日志记录：
- 仅记录`UseCase::Chat`类型的请求
- 保存完整JSON请求体到 `~/.omnish/logs/messages/{timestamp}.json`
- 滚动清理：最多保留30个日志文件
- 用途：调试和审计LLM请求内容

### 配置结构

#### `LlmBackendConfig`
LLM后端配置结构体（来自omnish-common）：
- `backend_type`: 后端类型（"anthropic" 或 "openai-compat"）
- `model`: 模型名称
- `api_key_cmd`: 获取API密钥的命令
- `base_url`: API基础URL（anthropic支持自定义，openai-compat必需）
- `max_content_chars`: 该模型的上下文最大字符数（可选，用于限制上下文大小）

#### `LangfuseConfig`（omnish-common）
Langfuse可观测性配置结构体：
- `public_key`: Langfuse公钥
- `secret_key`: Langfuse密钥（直接值，非命令）（可选，未设置时禁用Langfuse）
- `base_url`: Langfuse服务地址（默认`https://cloud.langfuse.com`）

## 关键函数说明

### `LlmBackend::complete()`
发送LLM补全请求并获取响应。

**参数:** `req: &LlmRequest` - LLM请求结构体
**返回:** `Result<LlmResponse>` - LLM响应或错误
**用途:** 主要的LLM交互接口，处理API调用、重试、错误处理和响应解析

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

### `message_log::log_request()`
保存LLM请求payload到本地日志文件。

**参数:**
- `body: &serde_json::Value` - 完整的请求JSON体
- `use_case: UseCase` - 请求用途（仅Chat类型会被记录）

**用途:** 将Chat请求的完整payload以pretty JSON格式写入`~/.omnish/logs/messages/{timestamp}.json`，滚动保留最近30个文件。

### `prompt_template()`
获取提示模板。

**参数:** `has_query: bool` - 是否有用户查询
**返回:** `&'static str` - 提示模板字符串
**用途:** 返回包含占位符的提示模板

### 常量

- `DAILY_NOTES_PROMPT` — 每日工作总结的LLM提示（中文），使用XML标签 `<commands>` 包裹上下文，输出bullet列表格式
- `HOURLY_NOTES_PROMPT` — 每小时工作总结的LLM提示（中文），使用XML标签 `<commands>`、`<hourly_summaries>` 包裹上下文（issue #96）
- `CHAT_PROMPT_JSON` — 编译内嵌的chat提示词JSON（来自`assets/chat.json`），通过`include_str!`编译到二进制
- `CHAT_OVERRIDE_EXAMPLE` — `chat.override.json`示例文件内容（来自`assets/chat.override.json.example`）
- `TEMPLATE_NAMES` — 已知模板名列表：`["chat", "chat-system", "auto-complete", "daily-notes", "hourly-notes"]`

### `template_by_name()`
根据名称返回模板内容（用于 `/template <name>` 命令）。

**参数:** `name: &str` - 模板名称
**返回:** `Option<String>` - 模板内容或None
**用途:**
- `auto-complete`: 使用 `build_simple_completion_content()` 渲染两种变体（空输入/有输入）
- `chat-system`: 使用 `PromptManager::default_chat().build()` 返回完整chat系统提示词
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

// 检查token使用情况
if let Some(usage) = &response.usage {
    println!("输入tokens: {}, 输出tokens: {}", usage.input_tokens, usage.output_tokens);
}
```

### 工具调用示例
```rust
use omnish_llm::tool::{ToolDef, ToolResult};
use serde_json::json;

// 1. 定义工具（使用ToolDef，无需实现trait）
let tool_def = ToolDef {
    name: "my_tool".to_string(),
    description: "执行某个操作".to_string(),
    input_schema: json!({
        "type": "object",
        "properties": {
            "param": {"type": "string"}
        },
        "required": ["param"]
    }),
};

// 2. 构建带工具的LLM请求
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
    tools: vec![tool_def],  // 提供工具定义
    extra_messages: vec![],
};

// 3. 发送请求并处理响应
let response = backend.complete(&request).await?;

// 检查是否有工具调用
if response.stop_reason == StopReason::ToolUse {
    for tool_call in response.tool_calls() {
        println!("LLM请求调用工具: {}", tool_call.name);
        // 执行工具并构造结果
        let result = ToolResult {
            tool_use_id: tool_call.id.clone(),
            content: "执行结果".to_string(),
            is_error: false,
        };
        // 通常需要将结果返回给LLM继续agent循环
    }
} else {
    println!("LLM文本响应: {}", response.text());
}
```

### 使用PromptManager构建系统提示词
```rust
use omnish_llm::prompt::PromptManager;

// 从内嵌JSON加载默认chat提示词
let mut pm = PromptManager::default_chat();

// 加载用户覆盖（如有）
if let Ok(override_json) = std::fs::read_to_string("~/.omnish/chat.override.json") {
    if let Ok(overrides) = PromptManager::from_json(&override_json) {
        pm = pm.merge(overrides);
    }
}

// 添加插件提供的片段
pm.add("my_plugin", "插件特定的提示词内容");

// 生成最终系统提示词
let system_prompt = pm.build();
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
- `omnish-common`: 配置类型定义（含`LangfuseConfig`）
- `omnish-protocol`: ChatTurn类型（用于多轮对话）
- `reqwest`: HTTP客户端，用于API调用
- `serde`/`serde_json`: JSON序列化和反序列化
- `anyhow`: 错误处理
- `async-trait`: 异步trait支持
- `tracing`: 日志记录
- `chrono`: 时间戳（Langfuse事件和日志文件名）
- `dirs`: 获取home目录（日志路径）
- `tokio`: 异步运行时（重试sleep、Langfuse后台发送）
- `std::process::Command`: 执行命令获取API密钥
- `std::sync::{Arc, RwLock}`: 线程安全的引用计数和读写锁

## 模块文件结构
- `lib.rs`: 模块入口，导出所有公共接口
- `backend.rs`: LlmBackend trait、LlmRequest/LlmResponse、Usage、UseCase/TriggerType等核心类型
- `tool.rs`: 工具相关类型（ToolDef、ToolCall、ToolResult）
- `anthropic.rs`: Anthropic API后端实现（含429/529重试）
- `openai_compat.rs`: OpenAI兼容API后端实现（含429重试）
- `factory.rs`: 后端工厂函数（create_backend、create_default_backend、MultiBackend、Langfuse包装逻辑）
- `template.rs`: 提示模板（build_simple_completion_content、DAILY/HOURLY_NOTES_PROMPT等）
- `prompt.rs`: PromptManager（可组合系统提示词片段管理）和内嵌chat提示词常量
- `langfuse.rs`: Langfuse可观测性集成（LangfuseBackend包装器）
- `message_log.rs`: LLM请求payload本地日志记录
- `assets/chat.json`: 默认chat系统提示词片段JSON（编译内嵌）
- `assets/chat.override.json.example`: chat覆盖文件示例

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

# 可选：Langfuse可观测性集成
[llm.langfuse]
public_key = "pk-..."
secret_key = "sk-lf-..."             # 直接值，非命令
base_url = "https://cloud.langfuse.com"  # 可选，默认cloud.langfuse.com
```

## 错误处理
模块使用`anyhow::Result`进行错误处理，包括：
- API调用失败（HTTP错误、网络问题）
- 429/529速率限制和过载错误（自动重试，最多3次，指数退避）
- 响应格式错误（缺少必需字段）
- 配置错误（缺少必需参数）
- 命令执行失败（获取API密钥失败）

## 测试
模块包含完整的单元测试，覆盖：
- API密钥解析
- 后端创建
- 提示模板构建（前缀稳定性验证）
- PromptManager片段加载、合并、构建
- chat系统提示词关键内容验证
- thinking标签的提取和剥离
- UUID生成格式验证
- 截断函数
- 错误处理场景

## 更新历史

### v0.6.0 - 可观测性、提示词管理和健壮性改进

**主要新功能:**

1. **Langfuse可观测性集成** (commit e447ff3, af926cf, be6d2af, b6d1226):
   - 新增`langfuse.rs`模块，`LangfuseBackend`以装饰器模式包装后端
   - 每次LLM调用后异步上报trace和generation事件
   - 记录模型、use_case、输入摘要、输出、工具调用数、延迟、token使用量
   - 配置字段变更：`secret_key_cmd`→`secret_key`（直接值），`host`→`base_url`
   - 修复Langfuse输入记录为实际内容而非字符数

2. **PromptManager可组合提示词** (commit 0eba0ec, efff522):
   - 新增`prompt.rs`模块，`PromptManager`管理具名提示词片段
   - 支持从JSON加载、合并覆盖、插件片段追加
   - 替代原先的`CHAT_SYSTEM_PROMPT`硬编码常量

3. **Chat提示词JSON内嵌和覆盖机制** (commit f3ce03a, c0a9c41, d615da0):
   - `assets/chat.json`通过`include_str!`编译内嵌到二进制
   - 启动时安装到`~/.omnish/chat.json`
   - `prompt.json`重命名为`tool.override.json`
   - 新增`chat.override.json`支持，可覆盖或追加提示词片段
   - Chat系统提示词基于Claude Code模式重新设计

4. **LLM请求payload日志** (commit 19de992, c00a368):
   - 新增`message_log.rs`模块
   - Chat请求的完整JSON体保存到`~/.omnish/logs/messages/`
   - 滚动清理，最多保留30个文件

5. **429/529自动重试** (commit ed37473):
   - Anthropic后端支持429（速率限制）和529（过载）自动重试
   - OpenAI兼容后端支持429自动重试
   - 最多3次重试，指数退避（5s/10s/20s），上限60s
   - 支持解析`retry-after`响应头

6. **思考模式支持chat** (commit 0d3239e):
   - `enable_thinking`支持`Some(true)`启用Anthropic扩展思考（budget_tokens: 4096）
   - Chat模式下可启用思考模式

7. **Usage解析** (commit c1a9b34):
   - 新增`Usage`结构体，从API响应中解析token使用统计
   - `LlmResponse`新增`usage`字段
   - 两个后端均支持解析input/output tokens和cache统计
   - Langfuse上报中包含usage数据

8. **Tool trait移除，工具简化** (commit ed5ab15, 7d7d8af):
   - 移除`Tool` trait，工具简化为固有方法实现
   - `tool.rs`仅保留`ToolDef`、`ToolCall`、`ToolResult`数据类型

9. **Anthropic max_tokens设置** (commit a593cf6):
   - Anthropic后端`max_tokens`固定为8192

**相关issue:** #199 (plugin prompt fragments), #205 (message logging), #207 (retry), #254 (override rename), #257 (embed chat prompt), #260 (langfuse input), #262 (thinking mode), #263 (usage parsing)

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
