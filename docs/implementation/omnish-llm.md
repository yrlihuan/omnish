# omnish-llm 模块

**功能:** LLM后端抽象和实现

## 模块概述

omnish-llm 提供LLM后端抽象，支持多种LLM提供商，包括Anthropic、OpenAI兼容API。模块通过统一的接口封装不同LLM提供商的API调用，提供一致的LLM交互体验。从v0.5.0开始，支持工具调用（tool-use）功能，使LLM能够主动调用外部工具完成任务。v0.6.0新增了Langfuse可观测性集成、可组合的系统提示词管理（PromptManager）、请求日志记录、429/529重试机制等功能。v0.7.x支持全局代理（proxy/no_proxy）配置、可配置的定期总结间隔（默认4小时），以及在每日笔记中包含对话记录。最新版本新增了模型预设（presets）模块，从嵌入式JSON加载提供商元数据；`LlmBackend` trait精简化，将`list_backends()`等方法下沉到`MultiBackend`固有方法；支持per-backend `use_proxy`配置和`context_window`字段；新增`SharedLlmBackend`类型别名支持热重载；新增`UnavailableBackend`作为未配置LLM时的回退。

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
- `extra`: 供应商特定扩展字段（`serde_json::Map<String, serde_json::Value>`），使用 `#[serde(flatten)]` 透明序列化/反序列化，空时跳过序列化。用于保留如 Gemini `thought_signature` 等供应商特有字段，确保多轮对话回传时不丢失

#### `ToolResult`
工具执行结果：
- `tool_use_id`: 对应的工具调用ID
- `content`: 执行结果内容（字符串）
- `is_error`: 是否为错误结果

### LLM后端接口

#### `BackendInfo`
后端信息结构体，用于列举可用后端：
- `name`: 后端名称（配置中的key）
- `model`: 模型名称

#### `LlmBackend` trait
LLM后端接口，定义所有LLM后端必须实现的方法：
- `complete()`: 发送补全请求并获取响应
- `name()`: 返回后端名称标识符
- `model_name()`: 返回模型名称（必须实现的方法）
- `max_content_chars()`: 返回该后端模型的上下文最大字符数（可选，默认None）

> 注：`max_content_chars_for_use_case()`、`list_backends()`、`chat_default_name()`、`get_backend_by_name()` 已从trait中移除，其中后三者已下沉为`MultiBackend`的固有方法。

#### `UnavailableBackend`
当未配置LLM或所有后端初始化失败时使用的回退后端：
- 实现`LlmBackend` trait
- `complete()` 始终返回错误 `"LLM backend not configured"`
- `name()` 和 `model_name()` 均返回 `"unavailable"`

#### `CacheHint` / `CachedText` / `TaggedMessage`
后端无关的缓存生命周期提示（v0.8.12+）。Anthropic 后端会将提示翻译为 `cache_control` 字段，OpenAI 兼容后端忽略：
- `CacheHint::None`：不缓存
- `CacheHint::Short`：`ephemeral`（默认 5 分钟 TTL）
- `CacheHint::Long`：`ephemeral` 且 `ttl: "1h"`
- `CachedText { text, cache }`：可缓存文本，用于 `LlmRequest.system_prompt`
- `TaggedMessage { content, cache }`：带缓存提示的消息，`content` 为 Anthropic 格式 JSON（内部规范），用于 `LlmRequest.extra_messages`

#### `LlmRequest`
LLM请求结构体，包含发送给LLM的完整请求信息：
- `context`: 终端会话上下文（单轮 fallback 使用）
- `query`: 用户查询（可选，单轮 fallback 使用）
- `trigger`: 触发类型（手动、自动错误检测、自动模式检测）
- `session_ids`: 相关会话ID列表
- `use_case`: 请求用途（用于选择合适的模型）
- `max_content_chars`: 模型上下文最大字符数限制（可选，用于限制上下文大小）
- `system_prompt`: 可选的 `CachedText`（chat模式由`PromptManager`构建，带缓存提示）
- `enable_thinking`: 思考模式开关（可选，`Some(true)`启用思考模式，`Some(false)`禁用，`None`使用后端默认）
- `tools`: 工具定义列表（`Vec<ToolDef>`，每个 `ToolDef` 带 `cache: CacheHint` 字段）
- `extra_messages`: 多轮/agent 循环消息（`Vec<TaggedMessage>`，每条携带独立缓存提示）
- v0.8.12 移除已废弃的 `conversation: Vec<ChatTurn>` 字段

#### `LlmResponse`
LLM响应结构体，包含LLM返回的结果：
- `content`: 响应内容块列表（`Vec<ContentBlock>`），按API返回顺序保留，可能包含 `Thinking`、`Text`、`ToolUse` 的混合
- `stop_reason`: 停止原因（`StopReason`枚举）
- `model`: 使用的模型名称
- `usage`: token使用统计（可选，`Usage`结构体）

辅助方法：
- `text()`: 提取所有文本块并用换行符连接，方便不使用tool-use的调用者
- `thinking()`: 提取所有思考块的内容（`Option<String>`），若无思考内容返回`None`
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
- `Thinking { thinking: String, signature: Option<String> }`: 思考内容块（来自支持扩展思考的模型，按API返回的原始顺序保留）。`signature`字段保存Anthropic API返回的签名（多轮对话时需要回传），OpenAI兼容后端为`None`

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
- `Summarize`: 工具结果摘要，在将工具执行结果反馈回对话前进行压缩/总结

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
- `merge(overrides)`: 合并覆盖片段--同名片段替换，新片段追加
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

### 模型预设（presets）

模型预设模块从编译时嵌入的JSON文件（`assets/model_presets.json`）加载各LLM提供商的元数据，供daemon配置初始化和安装脚本使用。

**`ProviderPreset` 结构体：**
- `backend_type`: 后端类型（"anthropic" 或 "openai-compat"）
- `base_url`: API基础URL
- `default_model`: 默认模型名称
- `context_window`: 上下文窗口大小（token数）

**公共函数：**
- `get_provider(name)`: 按名称获取提供商预设（`Option<&ProviderPreset>`）
- `default_context_window(provider)`: 获取提供商的默认上下文窗口大小（`Option<usize>`）
- `provider_names()`: 列出所有提供商名称
- `chat_providers()`: 返回支持Chat的提供商列表（有序）
- `completion_providers()`: 返回支持Completion的提供商列表（有序）

**支持的提供商：**
anthropic、openai、gemini、openrouter、deepseek、moonshot-cn、moonshot-global、minimax、custom

**实现细节：**
- JSON通过`include_str!`编译时嵌入到二进制
- 使用`OnceLock`实现单例延迟解析，仅在首次访问时解析JSON

### LLM后端实现

#### `AnthropicBackend`
Anthropic API后端实现：
- 支持Claude模型系列
- 实现`LlmBackend` trait
- `config_name`字段：存储配置中的后端名称（如"anthropic"），`name()`方法返回此名称而非硬编码字符串，用于线程级模型跟踪
- `max_content_chars`字段：存储该后端的上下文最大字符数（`Option<usize>`），由工厂函数通过`effective_max_content_chars()`计算后传入
- `model_name()`：返回模型名称（`&self.model`）
- 使用Anthropic Messages API (v1/messages，API版本2024-04-04）
- 支持`base_url`配置（默认api.anthropic.com），可用于代理或自托管
- `max_tokens`固定为8192
- 多轮对话支持：conversation历史映射为messages数组，上下文注入第一条user消息
- 系统提示词支持：通过Anthropic `system` 顶层字段（数组格式，附带`cache_control`用于提示缓存）
- 思考模式：`enable_thinking == Some(true)` 时启用（budget_tokens: 4096）；`Some(false)` 和 `None` 时均不发送思考参数（不再为 `Some(false)` 显式发送禁用参数）
- 思考块签名保留：解析响应中`thinking` block的`signature`字段并存储为`Option<String>`，多轮对话回传时包含签名（Anthropic API要求）
- 工具调用支持：通过`tools`字段提供工具定义，解析响应中的`tool_use` content blocks
- 供应商扩展字段捕获：解析 `tool_use` content blocks 时，除 `id`/`name`/`input` 外的字段保留到 `ToolCall.extra`，确保供应商特有字段（如签名等）在多轮回传时不丢失
- **提示缓存（Prompt Caching）：** 通过`cache_control: {"type": "ephemeral"}`标记实现Anthropic KV cache，设置最多3个缓存断点：
  - 工具定义：最后一个工具附带`cache_control`（工具在调用间稳定不变）
  - 系统提示词：转为数组格式并附带`cache_control`
  - 消息数组：标记倒数第二条消息（非最后一条，因为最后一条含`system-reminder`每次请求会变化）
  - `inject_cache_control()`辅助函数：为消息的最后一个content block添加`cache_control`，支持字符串内容（自动转为数组格式）和数组内容
- `strip_thinking()` 辅助函数解析content blocks（thinking vs text vs tool_use）
- 连接错误自动重试：`.send()`遇到连接错误（`is_connect()`/`is_request()`）时最多重试3次，指数退避（5s起步，最大60s）
- 429/529自动重试：最多3次重试，指数退避（默认5s起步，最大60s），支持解析`retry-after`响应头
- 错误诊断增强：响应JSON解码失败时，错误信息包含响应体预览（最多1000字节，超出截断并显示总长度），便于调试代理错误等场景
- Usage解析：从API响应中提取`input_tokens`、`output_tokens`、`cache_read_input_tokens`、`cache_creation_input_tokens`
- 请求日志：Chat请求的完整payload记录到`~/.omnish/logs/messages/`

#### `OpenAiCompatBackend`
OpenAI兼容API后端实现：
- 支持OpenAI、Azure OpenAI、本地兼容API（如vLLM）；配置中 `"openai"` 可作为 `"openai-compat"` 的别名
- 实现`LlmBackend` trait
- `max_content_chars`字段：存储该后端的上下文最大字符数（`Option<usize>`），由工厂函数通过`effective_max_content_chars()`计算后传入
- `model_name()`：返回模型名称（`&self.model`）
- 使用OpenAI兼容的Chat Completions API
- `base_url` 尾部斜杠自动剥离：初始化时去除末尾 `/`，防止拼接路径时产生双斜杠导致 404
- 多轮对话支持：conversation历史映射为messages数组
- 系统提示词支持：作为 `role: "system"` 消息前置
- 思考模式：通过 `chat_template_kwargs` 传递 `enable_thinking: false`（适配vLLM/Qwen3）
- 工具调用支持：通过`tools`字段提供工具定义，解析响应中的`tool_calls`
- `extract_thinking()` 辅助函数解析响应中的 `<think>` 标签（提取为 `ContentBlock::Thinking`）
- `convert_extra_messages()` 辅助函数将 Anthropic 格式的 extra_messages（含 `tool_use`/`tool_result`/`thinking` 内容块）转换为 OpenAI 格式（`tool_calls`/`tool` role/`reasoning_content`），同时保留 `ToolCall.extra` 中的供应商特定扩展字段
- 错误诊断增强：非 OpenAI 标准格式的错误响应（无 `error.message` 字段）和 JSON 解码失败时，错误信息包含完整响应体内容，便于调试非标准 API 实现
- 连接错误自动重试：`.send()`遇到连接错误（`is_connect()`/`is_request()`）时最多重试3次，指数退避（5s起步，最大60s）
- 429自动重试：最多3次重试，指数退避，支持解析`retry-after`响应头
- Usage解析：从API响应中提取`prompt_tokens`→`input_tokens`、`completion_tokens`→`output_tokens`、`cached_tokens`→`cache_read_input_tokens`
- 请求日志：Chat请求的完整payload记录到`~/.omnish/logs/messages/`

#### `MultiBackend`
多后端路由实现：
- 根据 `UseCase` 将请求路由到不同的后端实例
- 支持为 Completion、Analysis、Chat 分别配置不同的模型
- 创建时自动解析Langfuse配置并包装各后端
- 存储所有命名后端（`named_backends: HashMap<String, Arc<dyn LlmBackend>>`），支持按名称获取后端
- 初始化时容忍单个后端失败：use_case 对应的后端初始化失败时记录 warning 并回退到默认后端，而不是中止所有初始化
- 固有方法 `list_backends()`、`chat_default_name()`、`get_backend_by_name()` 用于每线程模型选择（从`LlmBackend` trait下沉至此）
- `model_name_for_use_case(use_case)`: 返回指定用途的模型名称
- `from_single(backend)`: 将单个后端包装为MultiBackend（用于测试）
- Per-backend `use_proxy` 支持：仅当后端配置的 `use_proxy` 为 true 时才应用全局代理

**`SharedLlmBackend` 类型别名：**
```rust
pub type SharedLlmBackend = Arc<RwLock<Arc<MultiBackend>>>;
```
用于热重载场景，所有消费者通过此共享引用读取后端，热重载时通过RwLock原子替换内部`Arc<MultiBackend>`。

#### `LangfuseBackend`（可观测性）
Langfuse可观测性包装器，透明地为LLM调用添加追踪：
- 以装饰器模式包装任意`LlmBackend`实现
- 每次`complete()`调用后异步发送trace和generation事件到Langfuse `/api/public/ingestion` API
- 记录信息包括：模型名称、use_case、请求输入摘要、输出文本、工具调用数、延迟、错误状态
- 当有`Usage`数据时，上报input/output token数和cache统计
- 使用Basic Auth认证（public_key + secret_key）
- fire-and-forget模式：发送失败不影响LLM调用结果
- 配置可选，未配置时不包装后端
- 委托方法：`name()`、`max_content_chars()`、`model_name()` 均委托给内部包装的后端

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
- `use_proxy`: 是否使用全局代理（布尔值，默认false）--仅当此字段为true时，该后端才会应用全局proxy配置
- `context_window`: 上下文窗口大小（token数，可选）--当`max_content_chars`未设置时，默认使用`context_window * 1.5`
- `max_content_chars`: 该模型的上下文最大字符数（可选，高级覆盖，优先级高于`context_window`推算值）

#### `LangfuseConfig`（omnish-common）
Langfuse可观测性配置结构体：
- `public_key`: Langfuse公钥
- `secret_key`: Langfuse密钥（直接值，非命令）（可选，未设置时禁用Langfuse）
- `base_url`: Langfuse服务地址（默认`https://cloud.langfuse.com`）
- `proxy`: 可选，发送Langfuse事件使用的代理URL（继承自全局proxy配置）
- `no_proxy`: 可选，不走代理的主机列表（继承自全局no_proxy配置）

## 关键函数说明

### `LlmBackend::complete()`
发送LLM补全请求并获取响应。

**参数:** `req: &LlmRequest` - LLM请求结构体
**返回:** `Result<LlmResponse>` - LLM响应或错误
**用途:** 主要的LLM交互接口，处理API调用、重试、错误处理和响应解析

### `create_backend()`
根据配置创建LLM后端实例。

**参数:**
- `name: &str` - 后端名称（配置中的key，存储为`config_name`用于模型跟踪）
- `config: &LlmBackendConfig` - 后端配置
- `proxy: Option<&str>` - 全局代理URL（可选，支持http/https/socks5）
- `no_proxy: Option<&str>` - 不走代理的主机列表（逗号分隔，可选）

**返回:** `Result<Arc<dyn LlmBackend>>` - 装箱的LLM后端实例
**用途:** 工厂函数，根据配置类型创建对应的后端实现。仅当后端配置的`use_proxy`为true时，HTTP客户端才会使用全局代理。创建时通过`effective_max_content_chars()`计算`max_content_chars`并传入后端实例。

### `effective_max_content_chars()`
计算后端的有效最大上下文字符数。

**参数:** `config: &LlmBackendConfig` - 后端配置
**返回:** `Option<usize>` - 有效的最大上下文字符数
**优先级:** 显式`max_content_chars` > `context_window * 1.5` > `None`
**用途:** 统一计算后端的上下文限制，支持从`context_window`自动推算

### `create_default_backend()`
从完整LLM配置创建默认后端。

**参数:**
- `llm_config: &LlmConfig` - 完整的LLM配置
- `proxy: Option<&str>` - 全局代理URL（可选）
- `no_proxy: Option<&str>` - 不走代理的主机列表（可选）

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

- `DAILY_NOTES_PROMPT` - 每日工作总结的LLM提示（英文基底），以项目/目标为主线汇总各时段 `<hourly_summaries>`，通过 `append_language_instruction()` 注入目标语言
- `HOURLY_NOTES_PROMPT` - 定期总结的LLM提示模板（英文基底），聚焦当前项目与目标进展，用"N小时"占位（实际间隔由配置决定），使用XML标签 `<commands>`、`<conversations>` 包裹上下文（issue #96），通过 `append_language_instruction()` 注入目标语言
- `THREAD_SUMMARY_PROMPT` - 线程标题生成提示（英文基底），输出 ≤20 字标题；由 `append_language_instruction()` 决定输出语言
- `CHAT_PROMPT_JSON` - 编译内嵌的chat提示词JSON（来自`assets/chat.json`），通过`include_str!`编译到二进制
- `CHAT_OVERRIDE_EXAMPLE` - `chat.override.json`示例文件内容（来自`assets/chat.override.json.example`）
- `TEMPLATE_NAMES` - 已知模板名列表：`["chat", "chat-system", "auto-complete", "daily-notes", "hourly-notes"]`

### `append_language_instruction(prompt, language)`
在提示词末尾追加语言指令。支持 `en`/`zh`/`zh-tw`/`ja`/`ko`/`fr`/`es`/`ar`，无匹配时回退英文。统一用于 daemon 端定时任务（daily/hourly/thread summary），保证 prompt 主体为英文基底、输出语言由 `daemon_config.client.language` 决定。

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

// 构建LLM请求（单轮：extra_messages 为空时，使用 context + query）
let request = LlmRequest {
    context: "终端会话上下文...".to_string(),
    query: Some("用户查询".to_string()),
    trigger: TriggerType::Manual,
    session_ids: vec![],
    use_case: UseCase::Chat,
    max_content_chars: None,
    system_prompt: None,      // Option<CachedText>，可选
    enable_thinking: None,    // 思考模式
    tools: vec![],            // 工具定义（每个 ToolDef 带 cache 字段）
    extra_messages: vec![],   // Vec<TaggedMessage>，agent 循环消息
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
    cache: CacheHint::None, // 工具定义可单独打缓存提示（通常最后一个工具设为 Long）
};

// 2. 构建带工具的LLM请求
let request = LlmRequest {
    context: "上下文".to_string(),
    query: Some("请使用工具".to_string()),
    trigger: TriggerType::Manual,
    session_ids: vec![],
    use_case: UseCase::Chat,
    max_content_chars: None,
    system_prompt: None,
    enable_thinking: None,
    tools: vec![tool_def],  // 提供工具定义
    extra_messages: vec![], // Vec<TaggedMessage>
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
- `backend.rs`: LlmBackend trait、UnavailableBackend、LlmRequest/LlmResponse、Usage、UseCase/TriggerType等核心类型
- `tool.rs`: 工具相关类型（ToolDef、ToolCall、ToolResult）
- `anthropic.rs`: Anthropic API后端实现（含连接错误重试、429/529重试）
- `openai_compat.rs`: OpenAI兼容API后端实现（含连接错误重试、429重试）
- `factory.rs`: 后端工厂函数（create_backend、create_default_backend、MultiBackend、SharedLlmBackend、effective_max_content_chars、Langfuse包装逻辑）
- `presets.rs`: 模型预设模块（ProviderPreset、从嵌入式JSON加载提供商元数据）
- `template.rs`: 提示模板（`build_user_content`、`build_completion_parts` 三层切分、`COMPLETION_INSTRUCTIONS` 静态指令、DAILY/HOURLY_NOTES_PROMPT 等）
- `prompt.rs`: PromptManager（可组合系统提示词片段管理）和内嵌chat提示词常量
- `langfuse.rs`: Langfuse可观测性集成（LangfuseBackend包装器）
- `message_log.rs`: LLM请求payload本地日志记录
- `assets/model_presets.json`: 模型预设JSON（编译内嵌），包含各提供商的backend_type、base_url、default_model、context_window
- `assets/chat.json`: 默认chat系统提示词片段JSON（编译内嵌）
- `assets/chat.override.json.example`: chat覆盖文件示例

## 配置示例
```toml
# daemon.toml 中的全局代理配置（可选）
proxy = "http://proxy.example.com:8080"    # 支持 http://、https://、socks5://
no_proxy = "localhost,127.0.0.1,*.internal.com"

# 定期总结间隔配置（可选，默认4小时）
[tasks.periodic_summary]
interval_hours = 4

# omnish.toml 中的LLM配置
[llm]
default = "anthropic"

[llm.backends.anthropic]
backend_type = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_cmd = "echo $ANTHROPIC_API_KEY"
# base_url = "https://api.anthropic.com"  # 可选，默认为api.anthropic.com
use_proxy = false                          # 是否使用全局代理（默认false）
context_window = 200000                    # 上下文窗口大小（token数，可选）
# max_content_chars = 300000              # 高级覆盖，优先级高于context_window推算

[llm.backends.openai]
backend_type = "openai-compat"
model = "gpt-4"
api_key_cmd = "echo $OPENAI_API_KEY"
base_url = "https://api.openai.com/v1"  # openai-compat必需
context_window = 128000

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
- 连接错误（`.send()`失败，自动重试最多3次，指数退避5s-60s）
- 429/529速率限制和过载错误（自动重试，最多3次，指数退避）
- 响应格式错误（缺少必需字段）；JSON解码失败时包含响应体预览（最多1000字节）
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

### 提示缓存、思考块签名、错误诊断增强、后端名称重构（#442, #446, #449）

**主要变更:**

1. **Anthropic提示缓存（Prompt Caching）** (commit 4daf5d5, a252fbb):
   - 通过`cache_control: {"type": "ephemeral"}`标记启用Anthropic KV cache
   - 新增`inject_cache_control()`辅助函数：为消息的最后一个content block添加`cache_control`，支持字符串内容（自动转数组格式）和数组内容
   - 缓存断点布局（最多3个显式断点）：
     - 工具定义：最后一个tool附带`cache_control`（工具在调用间稳定不变）
     - 系统提示词：转为数组格式`[{"type": "text", "text": ..., "cache_control": ...}]`
     - 消息数组：标记倒数第二条消息（非最后一条，因为最后一条含每次变化的`system-reminder`）
   - 使API响应中产生非零的`cache_read_input_tokens`，提升`/thread stats`的缓存率显示

2. **思考块签名保留** (commit 5db2bf3):
   - `ContentBlock::Thinking` 从 `Thinking(String)` 重构为 `Thinking { thinking: String, signature: Option<String> }`
   - Anthropic后端解析响应中`thinking` block的`signature`字段并存储
   - 多轮对话时，`content_block_to_json()`序列化思考块会包含`signature`字段（Anthropic API要求回传签名）
   - OpenAI兼容后端的`signature`始终为`None`（无签名概念）

3. **错误诊断增强** (commit eae20ff, 3597d5a):
   - Anthropic后端响应JSON解码失败时，先读取响应体为文本再手动解析JSON，错误信息包含响应体预览
   - 错误体预览上限从200字节增加到1000字节（便于查看完整的代理错误信息等），超出部分截断并显示总长度

4. **后端名称使用config_name** (commit 0e60395):
   - `AnthropicBackend`和`OpenAiCompatBackend`新增`config_name`字段
   - `name()`方法返回`config_name`（配置中的后端key，如"anthropic"）而非硬编码字符串
   - `create_backend()`的`name`参数（原`_name`）现在被实际使用，传入后端实例
   - 用途：线程级模型跟踪（ThreadMeta中记录实际使用的后端名称）

**相关issue:** #442 (thread stats), #446 (thinking signature, error diagnostics), #449 (prompt caching)

### a128d08 - 连接错误重试（#407）

**主要变更:**

1. **HTTP层连接错误重试** (commit a128d08):
   - Anthropic和OpenAI兼容后端的`.send()`调用新增连接错误重试
   - 捕获`is_connect()`和`is_request()`类型的reqwest错误
   - 最多重试3次（`MAX_RETRIES`），指数退避（`DEFAULT_BACKOFF` 5s起步，`MAX_BACKOFF` 60s封顶）
   - 与已有的429/529 HTTP状态码重试共享同一重试循环，连接错误在收到HTTP响应之前处理
   - 重试耗尽后返回包含原始错误信息的`anyhow::Error`

**相关issue:** #407

### v0.7.x - 思考块重构、每线程模型选择、OpenAI工具调用增强

**主要变更:**

1. **ContentBlock重构** (commit 7270e7c):
   - 新增 `ContentBlock::Thinking(String)` 变体，按原始顺序保留API响应中的思考块
   - 移除 `LlmResponse.thinking: Option<String>` 独立字段
   - 新增 `LlmResponse::thinking()` 方法提取思考内容
   - 思考块在工具调用循环中被正确保留（修复 #335）

2. **每线程模型选择** (commit 2a2e8d0):
   - 新增 `BackendInfo` 结构体（`name`, `model`）
   - `LlmBackend` trait 新增 `list_backends()`、`chat_default_name()`、`get_backend_by_name()` 方法（后续已下沉为`MultiBackend`固有方法）
   - `MultiBackend` 新增 `named_backends` HashMap 和相关字段，支持按名称获取后端
   - 支持 `/model` 命令在聊天中切换当前线程的模型

3. **后端初始化容错** (commit a6b6e97):
   - use_case 后端初始化失败时记录 warning 并回退到默认后端
   - 不再因单个后端失败而中止整体初始化（修复 #315）

4. **OpenAI 别名** (commit 32cb551):
   - `"openai"` 可作为 `"openai-compat"` 的别名用于 `backend_type` 配置

5. **OpenAI工具调用增强** (commit 021d446, 69d2208):
   - OpenAI兼容后端完整支持工具调用
   - 新增 `convert_extra_messages()` 函数：将 Anthropic 格式 extra_messages（含 thinking 块）转换为 OpenAI 格式
   - 修复工具调用循环中思考块被丢失的问题（修复 #339）

6. **每小时总结上下文增强** (commit 4ba3183):
   - `HOURLY_NOTES_PROMPT` 上下文新增 `<conversations>` 标签，包含相关线程对话记录

7. **线程标题总结** (commit 26491af):
   - 新增 `THREAD_SUMMARY_PROMPT` 常量，为对话线程生成≤20字的中文标题

**相关issue:** #154 (per-thread model), #335 (thinking block order), #339 (OpenAI thinking in tool loop), #315 (backend init tolerance)

8. **每日笔记包含对话记录** (commit 621c262):
   - `DAILY_NOTES_PROMPT` 新增 `<conversations>` 上下文标签，每日总结时一并发送过去24小时的对话记录
   - `ConversationManager` 提取共用的 `collect_recent_conversations_md()` 方法，供每日笔记和定期总结复用

9. **定期总结间隔可配置** (commit a716007):
   - 新增 `PeriodicSummaryConfig` 配置结构体，字段 `interval_hours`（默认4）
   - 配置路径：`[tasks.periodic_summary]` in daemon.toml
   - `HOURLY_NOTES_PROMPT` 从固定"1小时"改为"N小时"占位，实际间隔由配置注入
   - 文件输出路径保持 `notes/hourly/` 不变（向后兼容）

10. **全局proxy/no_proxy支持** (commit 0ee32f9):
    - `DaemonConfig` 新增 `proxy`（可选，支持http/https/socks5）和 `no_proxy`（逗号分隔主机列表）字段
    - `create_backend()`、`create_default_backend()`、`MultiBackend::new()` 签名新增 `proxy`/`no_proxy` 参数
    - 新增 `build_http_client()` 内部辅助函数，统一构建带代理配置的 reqwest 客户端
    - `LangfuseConfig` 新增 `proxy`/`no_proxy` 字段，`LangfuseBackend` 初始化时应用代理

### 2026-03-30 - 模型预设、LlmBackend trait精简、per-backend代理、context_window支持（#465）

**主要变更:**

1. **新增 `presets` 模块** (`presets.rs`):
   - 从编译时嵌入的 `assets/model_presets.json` 加载提供商元数据
   - `ProviderPreset` 结构体包含 `backend_type`、`base_url`、`default_model`、`context_window`
   - 公共函数：`get_provider()`、`default_context_window()`、`provider_names()`、`chat_providers()`、`completion_providers()`
   - 支持的提供商：anthropic、openai、gemini、openrouter、deepseek、moonshot-cn、moonshot-global、minimax、custom
   - `lib.rs` 新增 `pub mod presets;` 导出

2. **`LlmBackend` trait 精简**:
   - 移除方法：`max_content_chars_for_use_case()`、`list_backends()`、`chat_default_name()`、`get_backend_by_name()`
   - 新增必须实现的方法：`model_name() -> &str`
   - 新增 `UnavailableBackend` 结构体--未配置LLM时的回退后端，`complete()` 返回错误

3. **`MultiBackend` 变更**:
   - 新增类型别名 `SharedLlmBackend = Arc<RwLock<Arc<MultiBackend>>>`，用于热重载
   - `list_backends()`、`chat_default_name()`、`get_backend_by_name()` 从 trait 下沉为固有方法
   - 新增方法：`model_name_for_use_case(use_case) -> String`
   - 新增方法：`from_single(backend) -> Self`，将单个后端包装为MultiBackend（测试用）
   - 新增函数：`effective_max_content_chars(config) -> Option<usize>`，优先级：显式 max_content_chars > context_window * 1.5 > None

4. **Per-backend `use_proxy` 支持**:
   - `LlmBackendConfig` 新增 `use_proxy: bool` 字段（默认false）
   - 仅当 `use_proxy` 为 true 时，该后端才应用全局 proxy 配置
   - `LlmBackendConfig` 新增 `context_window: Option<usize>` 字段

5. **后端实现变更**:
   - `AnthropicBackend` 和 `OpenAiCompatBackend` 新增 `max_content_chars: Option<usize>` 字段
   - 两者均实现 `model_name()` 方法（返回 `&self.model`）

6. **`LangfuseBackend` 变更**:
   - `max_content_chars_for_use_case` 委托替换为 `model_name()` 委托

7. **`DAILY_NOTES_PROMPT` 简化**:
   - 现在仅引用 `<hourly_summaries>` 中各时段的工作摘要，不再直接包含原始命令和对话记录的XML标签

**相关issue:** #465

### 2026-04-02 - ToolCall扩展字段、UseCase::Summarize、思考参数精简、OpenAI错误诊断增强

**主要变更:**

1. **`ToolCall` 新增 `extra` 字段**:
   - 类型为 `serde_json::Map<String, serde_json::Value>`，使用 `#[serde(flatten)]` 透明序列化
   - 保留供应商特定字段（如 Gemini `thought_signature`），确保多轮对话回传不丢失
   - 空时通过 `skip_serializing_if` 跳过序列化

2. **`UseCase` 新增 `Summarize` 变体**:
   - 用于工具结果摘要场景，在将工具执行结果反馈回对话前进行压缩/总结
   - 可路由到独立的模型后端

3. **AnthropicBackend 思考参数精简**:
   - 思考参数仅在 `enable_thinking == Some(true)` 时发送启用参数
   - `Some(false)` 和 `None` 时均不再发送思考参数（之前 `Some(false)` 会显式发送禁用参数）
   - 解析 `tool_use` blocks 时捕获供应商扩展字段到 `ToolCall.extra`

4. **OpenAiCompatBackend 增强**:
   - `base_url` 初始化时自动剥离尾部斜杠，防止拼接路径产生双斜杠导致 404 错误
   - 错误诊断增强：非 OpenAI 标准格式的错误响应和 JSON 解码失败时，错误信息包含完整响应体
   - `convert_extra_messages()` 转换时保留 `ToolCall.extra` 中的供应商特定扩展字段

### 2026-04-16 - 补全提示词三层切分（#550）

**主要变更:**

1. **补全提示词切分为独立可缓存的三层** (commit 72caf67):
   - `COMPLETION_INSTRUCTIONS` 常量：静态指令文本（永不变化，作为 system prompt 发送，带 1h TTL 缓存）
   - 上下文块（命令历史，变化较慢，通过 `LlmRequest.context` 承载，带 ephemeral 缓存）
   - 用户输入块（当前光标处输入，每次击键变化，不缓存）
   - 新函数 `build_completion_parts(input, cursor_pos) -> (system, user)` 替代原 `build_simple_completion_content`
   - 优化目标：Anthropic prompt caching 跨相邻补全请求复用稳定前缀

2. **Anthropic cache_control TTL 使用字符串格式** (commit a618c5f):
   - 长 TTL 改为 `{"type": "ephemeral", "ttl": "1h"}`（字符串），此前误用数字秒导致 API 拒绝

### 2026-04-17 - 缓存提示后端无关化重构（#550 续）

**主要变更:**

1. **`CacheHint` 枚举与载体类型** (commit 5600f17):
   - 新增 `CacheHint { None, Short, Long }` 后端无关枚举
   - 新增 `CachedText { text, cache }`，作为 `LlmRequest.system_prompt` 类型（由 `Option<String>` 变更为 `Option<CachedText>`）
   - 新增 `TaggedMessage { content, cache }`，作为 `LlmRequest.extra_messages` 元素类型（由 `Vec<serde_json::Value>` 变更为 `Vec<TaggedMessage>`）
   - `ToolDef` 新增 `cache: CacheHint` 字段

2. **AnthropicBackend 翻译缓存提示** (commit 5600f17):
   - 新增 `cache_control_value(hint)` 将 `CacheHint` 转为 wire-level `cache_control` JSON
   - 新增 `apply_cache_hint_to_message(msg, hint)` 向消息最后一个 content block 注入 `cache_control`
   - 新增 `enforce_breakpoint_budget(req)` 强制执行 Anthropic 的 4 断点上限：统计静态断点（tools + system_prompt）占用，将剩余预算分配给 extra_messages 并只保留最后 N 个标记消息，其余降级为 None，超额时记录 warning
   - 重构为 `build_request_body(req, model)` 统一构建请求体：系统提示、工具、消息各自按 hint 打 `cache_control`
   - OpenAI 兼容后端直接忽略所有缓存提示

3. **移除已废弃字段**:
   - 删除 `LlmRequest.conversation: Vec<ChatTurn>`（曾用于早期多轮对话，现全部走 `extra_messages`）
   - 依赖项随之移除对 `omnish-protocol` 的引用

4. **单轮 vs 多轮契约文档化** (commit 52507c6):
   - `backend.rs` 明确注释：`context` 与 `query` 仅在 `extra_messages` 为空时生效；`extra_messages` 非空时，调用方需自行将额外上下文折入 `system_prompt` 或 `extra_messages`

5. **守护进程侧缓存策略**（daemon 侧对应变更）:
   - Chat agent 循环：`mark_chat_message_hints()` 每轮 LLM 调用前将所有 `extra_messages` hint 重置为 None，并将最后 2 条标为 Long；system_prompt 整体标为 Long
   - 完成补全：`system_prompt` 标为 Long，上下文通过 Anthropic user message content block 上的 `cache_control: ephemeral` 缓存（`build_request_body` 在单轮 completion 分支内构造）

**相关 issue:** #550
