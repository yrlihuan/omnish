# omnish-daemon 模块

**功能:** 守护进程服务，管理会话、处理客户端请求、集成LLM后端

## 模块概述

omnish-daemon 是omnish系统的核心守护进程，负责：
1. 管理终端会话的生命周期（创建、结束、持久化）
2. 接收并处理来自客户端的I/O数据流
3. 存储命令历史记录和终端输出
4. 集成LLM后端处理用户查询和自动补全请求
5. 提供RPC服务接口供客户端调用
6. 认证和安全（令牌认证、TLS加密）
7. 定时任务管理（会话驱逐、日报、小时摘要、磁盘清理）
8. 管理多轮聊天对话（线程存储、恢复、列表、删除，支持工具使用）
9. 补全采样（pending sample 捕获、JSONL 持久化）
10. 插件管理（内置和外部插件，提供工具使用能力）
11. 智能体循环（Agent Loop）实现多轮工具调用

守护进程以Unix domain socket方式运行，支持多个客户端同时连接。

## 重要数据结构

### `DaemonServer`
守护进程服务器主结构，包含：
- `session_mgr`: `Arc<SessionManager>` - 会话管理器实例
- `llm_backend`: `Option<Arc<dyn LlmBackend>>` - 可选的LLM后端
- `task_mgr`: `Arc<Mutex<TaskManager>>` - 定时任务管理器
- `conv_mgr`: `Arc<ConversationManager>` - 对话管理器
- `plugin_mgr`: `Arc<PluginManager>` - 插件管理器

### `SessionManager`
会话管理器，负责管理所有活跃会话，包含：
- `base_dir`: `PathBuf` - 会话数据存储的基础目录
- `sessions`: `RwLock<HashMap<String, Arc<Session>>>` - 活跃会话映射表（使用 `RwLock` 替代 `Mutex`，允许多个读取者并行访问，解决锁争用问题）
- `context_config`: `ContextConfig` - 上下文构建配置
- `completion_writer`: `mpsc::Sender<CompletionRecord>` - 补全记录写入通道
- `session_writer`: `mpsc::Sender<SessionUpdateRecord>` - 会话更新记录写入通道
- `history_frozen_until`: `RwLock<Option<u64>>` - 弹性窗口的历史冻结截止点
- `last_completion_context`: `RwLock<String>` - 上一次补全上下文缓存（用于KV cache预热检测）
- `sample_writer`: `mpsc::Sender<CompletionSample>` - 补全采样写入通道
- `last_sample_time`: `Mutex<Option<Instant>>` - 上一次采样时间（全局速率限制）

### `Session`（内部结构）
活跃会话的内部表示，包含：
- `dir`: `PathBuf` - 会话数据存储目录（创建后不可变）
- `meta`: `RwLock<SessionMeta>` - 会话元数据（ID、父会话ID、属性等）
- `commands`: `RwLock<Vec<CommandRecord>>` - 命令记录列表
- `stream_writer`: `Mutex<StreamWriterState>` - 流数据写入器状态
- `last_update`: `Mutex<Option<u64>>` - 上一次 SessionUpdate 的时间戳
- `pending_sample`: `Mutex<Option<PendingSample>>` - 待写入的补全采样

### `PluginManager`
插件管理器，负责管理所有注册的插件（官方内置 + 外部子进程），包含：
- `plugins`: `Vec<Box<dyn Plugin>>` - 所有已注册的插件列表

**主要特点:**
- **统一插件接口**：所有插件（内置和外部）实现统一的 `Plugin` trait
- **工具定义聚合**：通过 `all_tools()` 收集所有插件提供的工具定义
- **路由执行**：根据工具名自动路由到对应插件执行
- **外部插件加载**：从 `~/.omnish/plugins/` 目录加载启用的外部插件
- **JSON-RPC通信**：外部插件通过 stdin/stdout 进行 JSON-RPC 2.0 协议通信

### `Plugin` Trait
统一的插件接口，定义在 `crates/omnish-daemon/src/plugin.rs`：
- `name()` - 返回插件名称（用于日志和识别）
- `tools()` - 返回该插件提供的工具定义列表
- `call_tool(tool_name, input)` - 执行指定工具，返回 `ToolResult`

**内置插件:**
- `CommandQueryTool` - 查询命令历史和输出的工具

**外部插件:**
- `ExternalPlugin` - 通过子进程运行的外部插件，支持任意语言编写

### `ExternalPlugin`
外部子进程插件实现，包含：
- `plugin_name`: `String` - 插件名称
- `stdin`: `Mutex<BufWriter<ChildStdin>>` - 写入子进程标准输入
- `stdout`: `Mutex<BufReader<ChildStdout>>` - 读取子进程标准输出
- `child`: `Mutex<Child>` - 子进程句柄
- `tool_defs`: `Vec<ToolDef>` - 该插件提供的工具定义（初始化时获取）
- `next_id`: `Mutex<u64>` - JSON-RPC 请求 ID 计数器

**JSON-RPC 方法:**
- `initialize` - 初始化插件，返回插件名和工具定义列表
- `tool/execute` - 执行工具，传入工具名和参数，返回执行结果
- `shutdown` - 关闭插件（最佳努力，drop 时自动调用）

### `ConversationManager`
多轮聊天对话管理器，负责线程的创建、存储、加载和删除，包含：
- `threads_dir`: `PathBuf` - 线程文件存储目录（`~/.omnish/threads/`）
- `threads`: `Mutex<HashMap<String, Vec<serde_json::Value>>>` - 内存中的线程缓存（thread_id → 原始JSON消息列表）

**主要特点:**
- 每个线程以 UUID 命名，存储为 JSONL 文件（每行一个原始 JSON 消息）
- **原始JSON存储格式**：存储完整的 LLM API 消息格式，包括 tool_use、tool_result 等复杂内容块，更灵活，便于未来扩展
- 启动时从磁盘加载所有线程到内存，后续读取直接走内存缓存
- 写入时双写：同步更新内存缓存 + append 到磁盘 JSONL 文件
- **不持久化 `<system-reminder>` 标签**：在存储前自动过滤系统提醒内容
- 支持按文件修改时间排序列出所有对话
- 支持按索引（0-based）选取线程
- 支持删除线程（同时移除内存缓存和磁盘文件）
- **工具使用感知**：能够区分用户输入消息和工具结果消息（content 为字符串 vs 数组）

### `EventDetector`
事件检测器，用于检测自动触发条件，包含：
- `config`: `AutoTriggerConfig` - 自动触发配置

### `DetectedEvent`
检测到的事件枚举：
- `PatternMatch(String)` - 模式匹配事件（匹配的字符串）
- `NonZeroExit(i32)` - 非零退出码事件（退出码）

### `FileStreamReader`
文件流读取器，实现`StreamReader` trait，用于读取单个会话的流数据。

### `MultiSessionReader`
多会话流读取器，实现`StreamReader` trait，用于跨多个会话读取流数据。

## 工具使用与智能体循环

### 工具使用（Tool-Use）集成

守护进程通过 `PluginManager` 为 LLM 提供工具使用能力，允许 LLM 在聊天过程中调用工具获取额外信息。

**工具定义:**
- 工具定义（`ToolDef`）包含工具名、描述和 JSON Schema 输入规范
- 所有工具定义通过 `plugin_mgr.all_tools()` 收集并传递给 LLM
- 工具定义在系统提示词中展示给 LLM，说明可用工具及其使用方法

**工具调用流程:**
1. LLM 在响应中返回 `tool_use` 内容块（包含工具名、ID 和输入参数）
2. 守护进程提取所有 `tool_use` 块，为每个工具调用创建状态消息
3. 通过 `plugin_mgr.call_tool()` 路由到对应插件执行
4. 收集工具执行结果（`ToolResult`），构建 `tool_result` 内容块
5. 将工具结果作为用户消息发送回 LLM，继续对话

**流式状态消息:**
- 每个工具调用发送 `ChatToolStatus` 消息通知客户端
- 包含工具名、状态（`executing`）和描述（用于终端显示）
- 客户端显示类似 "🔧 查询命令历史..." 的状态提示

### 智能体循环（Agent Loop）

聊天处理实现了智能体循环模式，允许 LLM 进行多轮工具调用直到获得足够信息回答用户问题。

**循环机制:**
- 最多迭代 5 次（`max_iterations = 5`）
- 每次迭代：调用 LLM → 检查是否有工具调用 → 执行工具 → 将结果反馈给 LLM
- 循环终止条件：
  - LLM 返回文本响应（无 `tool_use` 块）
  - 达到最大迭代次数
  - 遇到错误

**消息流格式:**
```
User: {{query}}
Assistant: [text] + [tool_use blocks]
User: [tool_result blocks]
Assistant: [text] + [tool_use blocks]
User: [tool_result blocks]
...
Assistant: {{final response}}
```

**存储格式:**
- 所有消息（包括工具调用和结果）以原始 JSON 格式存储到对话线程
- 工具结果消息的 `content` 是数组（不是字符串），`ConversationManager` 能正确区分
- 不持久化 `<system-reminder>` 标签，保持线程文件简洁

### CommandQueryTool

内置工具，用于查询命令历史和获取完整命令输出，定义在 `crates/omnish-daemon/src/tools/command_query.rs`。

**工具定义:**
- 名称：`command_query`
- 操作：`list_history` 和 `get_output`
- 输入参数：`action`（必需）、`seq`（用于 get_output）、`count`（用于 list_history）

**功能:**
- `list_history(count)` - 列出最近 N 条命令（默认 20），包含序号、命令行、退出码、相对时间
- `get_output(seq)` - 获取指定序号命令的完整输出（自动跳过回显行，限制 500 行 / 50KB）

**实现细节:**
- 构造时传入当前会话的 `commands` 和 `stream_reader`
- 实现 `Tool` trait（LLM 工具接口）
- 实现 `Plugin` trait（插件接口）
- 输出自动截断并显示总行数，防止响应过大

### 聊天上下文增强

**命令历史注入:**
- 首条聊天消息自动包含最近 20 条命令列表（通过 `CommandQueryTool.list_history(20)`）
- 使用 `<system-reminder>` 标签包裹，提示 LLM 这些命令已在上下文中
- 减少简单历史查询的工具调用次数，提升响应速度
- 工具仍可用于获取完整输出或更多历史记录

**上下文格式示例:**
```
<system-reminder>
Recent commands (last 20):
[seq=1] ls -la  (exit 0, 5m ago)
[seq=2] git status  (exit 0, 3m ago)
...
</system-reminder>

User: what did I do recently?
```

## 对话管理

### ConversationManager

`ConversationManager` 管理多轮聊天线程的完整生命周期。线程以 JSONL 文件存储在 `~/.omnish/threads/` 目录下，每个文件以 UUID 命名。

#### 关键函数

##### `ConversationManager::new()`
创建对话管理器并从磁盘加载已有线程到内存。

**参数:**
- `threads_dir`: `PathBuf` - 线程文件存储目录

**返回:** `ConversationManager` 实例

##### `ConversationManager::create_thread()`
创建新对话线程，返回其 UUID。在磁盘创建空 JSONL 文件，并在内存插入空向量。

**返回:** `String`（线程 UUID）

##### `ConversationManager::get_latest_thread()`
按文件修改时间获取最近的线程 ID。

**返回:** `Option<String>`

##### `ConversationManager::list_conversations()`
列出所有对话，按修改时间降序排列。返回 `(thread_id, last_modified, exchange_count, last_question)` 元组列表。内部调用 `resolve_interrupted()` 确保交换计数和最后问题准确。

**返回:** `Vec<(String, SystemTime, u32, String)>`

##### `ConversationManager::get_thread_by_index()`
按索引（0-based，按修改时间排序）获取线程 ID。

**参数:**
- `index`: `usize` - 0-based 索引

**返回:** `Option<String>`

##### `ConversationManager::delete_thread()`
删除线程，同时从内存和磁盘移除。

**参数:**
- `thread_id`: `&str` - 要删除的线程 ID

**返回:** `bool`（线程是否存在并已删除）

##### `ConversationManager::append_messages()`
追加原始 JSON 消息到线程。双写到内存缓存和磁盘 JSONL 文件。

**参数:**
- `thread_id`: `&str` - 线程 ID
- `messages`: `&[serde_json::Value]` - 原始 JSON 消息数组

**用途:** 用于存储完整的 LLM 交互（包括 tool_use 和 tool_result 消息）

##### `ConversationManager::load_raw_messages()`
加载线程所有原始 JSON 消息（用于 LLM 上下文）。

**参数:**
- `thread_id`: `&str` - 线程 ID

**返回:** `Vec<serde_json::Value>`

**用途:** 直接传递给 LLM API，保留完整的消息结构（包括工具使用）

##### `ConversationManager::get_last_exchange()`
获取最后一次用户输入和助手回复，以及更早的用户输入消息数量。

**参数:**
- `thread_id`: `&str` - 线程 ID

**返回:** `(Option<(String, String)>, u32)`
- 元组第一项：最后的用户问题和助手回复（合并所有助手消息的文本）
- 元组第二项：更早的用户输入消息数量

**特性:**
- 自动区分用户输入消息和工具结果消息（content 为字符串 vs 数组）
- 自动提取文本内容，过滤 `<system-reminder>` 标签
- 处理多轮工具调用场景（多个 assistant 消息的文本会拼接）

##### `ConversationManager::is_user_input()` 和 `extract_text()`
内部辅助方法，用于处理原始 JSON 消息：

**`is_user_input(msg)`:**
- 检查消息是否为用户输入消息（`role == "user"` 且 `content` 为字符串）
- 工具结果消息的 `content` 是数组，不算用户输入

**`extract_text(msg)`:**
- 从消息中提取显示文本
- 支持字符串内容和数组内容（提取所有 `type: "text"` 块）
- 自动过滤 `<system-reminder>` 标签（仅在显示时，存储的原始数据保持不变）

### 聊天消息流程

```
客户端发送 ChatStart → 守护进程创建/恢复线程 → 返回 ChatReady（含线程ID和最近交换）
客户端发送 ChatMessage → 守护进程进入智能体循环:
  1. 构建上下文（首条消息包含命令历史）+ 加载对话历史
  2. 调用 LLM（传递工具定义）
  3. 检查响应中的 tool_use 块
  4. 如有工具调用：执行工具 → 发送 ChatToolStatus → 继续循环
  5. 如无工具调用：存储消息 → 返回 ChatResponse
客户端发送 ChatInterrupt → 守护进程记录中断标记 → 返回 Ack
```

**ChatMessage 处理细节:**
- **首条消息**（对话历史为空）：
  - 注入最近 20 条命令列表（包裹在 `<system-reminder>` 中）
  - 构建 `CommandQueryTool` 并注册到 `PluginManager`
  - 收集所有工具定义传递给 LLM
- **后续消息**：直接加载对话历史（包含工具调用和结果）
- 使用 `CHAT_SYSTEM_PROMPT` 系统提示词（包含工具使用说明）
- 对话历史通过 `LlmRequest.conversation` 字段传递给 LLM
- 工具定义通过 `LlmRequest.tools` 字段传递给 LLM
- **智能体循环**：最多 5 次迭代，每次迭代发送流式状态消息
- 所有消息（包括工具调用）以原始 JSON 格式存储，但 `<system-reminder>` 在存储前被过滤

## 补全采样

补全采样机制用于收集 LLM 补全建议与用户实际行为的对比数据，持久化到 JSONL 文件供离线分析。

### 采样流程

1. **捕获 pending sample**: 每次 `handle_completion_request()` 返回 LLM 建议后，将上下文、提示词、建议列表等保存为 `PendingSample` 到对应会话的 `pending_sample` 字段
2. **更新 accepted 标志**: 当 `CompletionSummary` 消息到达时，更新 pending sample 的 `accepted` 字段
3. **写入采样**: 当下一条命令到达（`receive_command`）时，检查 pending sample 并决定是否写入：
   - 补全未被接受（`!accepted`）
   - 下一条命令非空
   - 距补全请求的经过时间不超过 15 秒（`SAMPLE_MAX_ELAPSED_SECS`）
   - 建议与实际命令的编辑距离相似度低于阈值（0.3）
   - 全局速率限制：每 5 分钟最多一条采样（`SAMPLE_RATE_LIMIT_SECS`）
4. **会话结束 flush**: 会话结束时，未写入的 pending sample 不带 `next_command` 直接 flush

### 采样数据

采样通过 `mpsc` 通道发送到后台写入线程，写入 `~/.omnish/logs/samples/` 目录下的 JSONL 文件。每条 `CompletionSample` 包含：
- 时间戳、会话 ID、上下文、提示词
- LLM 返回的建议列表
- 用户输入、工作目录
- 延迟（ms）、是否接受
- 下一条实际命令、编辑距离相似度

## 任务管理

### TaskManager

`TaskManager` 是一个集中式的定时任务管理器，用于管理守护进程中的所有定时任务。它基于 `tokio-cron-scheduler` 库实现，提供了统一的任务注册、启动、列表和禁用等功能。

**结构说明:**
- `scheduler`: `JobScheduler` - 底层的 cron 任务调度器
- `tasks`: `HashMap<String, TaskEntry>` - 已注册任务的映射表，key为任务名称，value为任务信息
- `TaskEntry` 包含：
  - `uuid`: 任务在调度器中的唯一标识符
  - `cron`: cron 表达式字符串
  - `enabled`: 任务启用/禁用状态

**主要特点:**
- 支持 cron 表达式定义任务执行计划
- 使用本地时区进行时间计算（通过设置 `TZ` 环境变量）
- 支持运行时任务列表查询和禁用
- 任务注册后自动添加到调度器并记录日志

### 内置定时任务

守护进程注册了以下内置定时任务：

#### 1. `eviction` - 会话驱逐任务
**执行周期:** 每小时 (`0 0 * * * *`)
**功能:** 驱逐长时间不活跃的会话，防止内存无限增长
**相关配置:**
```toml
[tasks.eviction]
session_evict_hours = 48  # 默认：48小时后驱逐
```
**实现:** 通过 `create_eviction_job()` 函数创建，调用 `SessionManager::evict_inactive()`

#### 2. `hourly_summary` - 小时总结任务
**执行周期:** 每小时整点 (`0 0 * * * *`)
**功能:** 生成过去1小时内的命令执行摘要，保存到 `~/.omnish/notes/hourly/YYYY-MM-DD/HH.md`
**内容:** 仅保存LLM生成的摘要（不含原始命令上下文）
**特点:**
- 调用 LLM 后端（如果可用）生成执行摘要
- 支持上下文大小限制和内容精简
- 如果没有命令或上下文为空会自动跳过

**相关配置:**
```toml
[context.hourly_summary]
head_lines = 50         # 命令输出头部行数
tail_lines = 100        # 命令输出尾部行数
max_line_width = 128    # 每行最大字符数
```
**实现:** 通过 `create_hourly_summary_job()` 函数创建

#### 3. `daily_notes` - 日报生成任务
**执行周期:** 每天指定时刻 (默认 `0 0 18 * * *` - 每天18:00)
**功能:** 生成过去24小时的工作总结，保存到 `~/.omnish/notes/YYYY-MM-DD.md`
**特点:** LLM上下文中包含当天已有的小时摘要，帮助生成更准确的日报
**相关配置:**
```toml
[tasks.daily_notes]
enabled = true
schedule_hour = 18      # 每天几点生成（0-23），默认 18:00
```
**实现:** 通过 `create_daily_notes_job()` 函数创建

#### 4. `disk_cleanup` - 磁盘清理任务
**执行周期:** 默认每6小时 (`0 0 */6 * * *`)
**功能:** 清理距今超过48小时的过期会话目录，释放磁盘空间
**相关配置:**
```toml
[tasks.disk_cleanup]
schedule = "0 0 */6 * * *"  # Cron 表达式，默认每6小时
```
**实现:** 通过 `create_disk_cleanup_job()` 函数创建，调用 `SessionManager::cleanup_expired_dirs()`

### TaskManager 关键函数说明

#### `TaskManager::new()`
创建新的任务管理器实例。

**返回:** `Result<Self>`

**用途:** 初始化 TaskManager，创建内部的 JobScheduler

**注意:** 使用本地时区进行cron调度（通过`chrono::Local`）

#### `TaskManager::register()`
注册一个新的定时任务。

**参数:**
- `name`: `&str` - 任务名称
- `cron`: `&str` - cron 表达式 (格式: "second minute hour day month day_of_week")
- `job`: `Job` - 使用 `tokio_cron_scheduler::Job::new_async()` 创建的异步任务

**返回:** `Result<()>`

**用途:** 将定时任务添加到调度器，记录日志

#### `TaskManager::start()`
启动任务调度器，开始执行所有已注册的定时任务。

**返回:** `Result<()>`

**用途:** 在守护进程启动时调用，开始执行定时任务

#### `TaskManager::list()`
获取所有已注册任务的列表。

**返回:** `Vec<(String, String, bool)>`（任务名、cron表达式、启用状态）

**用途:** 查询当前注册的所有任务

#### `TaskManager::disable()`
在运行时禁用一个已注册的任务。

**参数:**
- `name`: `&str` - 要禁用的任务名称

**返回:** `Result<()>`

**用途:** 运行时管理，通过 `/tasks` 命令禁用特定任务

#### `TaskManager::format_list()`
格式化任务列表为可读的字符串（用于显示给用户）。

**返回:** `String`

**用途:** 在 `/tasks` 命令中显示当前的任务状态

### 创建 Job 的模式

omnish 中所有定时任务都遵循统一的模式，使用 `tokio_cron_scheduler::Job::new_async()` 创建异步任务：

```rust
pub fn create_custom_job(
    mgr: Arc<SessionManager>,
    config_param: SomeType,
) -> anyhow::Result<Job> {
    let cron = "0 0 * * * *";  // 定义 cron 表达式
    Ok(Job::new_async(cron, move |_uuid, _lock| {
        let mgr = mgr.clone();
        let param = config_param.clone();
        Box::pin(async move {
            // 任务实现逻辑
            if let Err(e) = perform_task(&mgr, &param).await {
                tracing::warn!("task failed: {}", e);
            }
        })
    })?)
}

async fn perform_task(
    mgr: &SessionManager,
    param: &SomeType,
) -> anyhow::Result<()> {
    // 实现具体逻辑
    Ok(())
}
```

**关键点:**
1. 使用 `Box::pin()` 包装异步块，符合 `tokio_cron_scheduler` 的接口要求
2. 在闭包中克隆必要的参数（Arc<T> 支持廉价克隆）
3. 使用 `tracing::warn!()` 记录任务执行错误，但不让错误中断任务调度器
4. cron 表达式采用标准 Unix cron 格式：`秒 分 时 日 月 周几`

### 配置示例

在 `~/.omnish/daemon.toml` 中配置所有定时任务：

```toml
[tasks.eviction]
session_evict_hours = 48

[tasks.daily_notes]
enabled = true
schedule_hour = 23

[tasks.disk_cleanup]
schedule = "0 0 */6 * * *"

[context.hourly_summary]
head_lines = 50
tail_lines = 100
max_line_width = 128
```

## 关键函数说明

### `PluginManager::new()`
创建新的插件管理器实例。

**返回:** `PluginManager` 实例

**用途:** 初始化空的插件管理器

### `PluginManager::register()`
注册一个插件。

**参数:**
- `plugin`: `Box<dyn Plugin>` - 插件实例

**用途:** 添加内置或外部插件到管理器

### `PluginManager::load_external_plugins()`
从 `~/.omnish/plugins/` 加载启用的外部插件。

**参数:**
- `enabled`: `&[String]` - 启用的插件名称列表（来自配置文件）

**用途:** 启动时加载外部子进程插件。对每个插件：
1. 检查可执行文件是否存在于 `~/.omnish/plugins/<name>/<name>`
2. 调用 `ExternalPlugin::spawn()` 启动子进程
3. 自动调用 `initialize` JSON-RPC 方法获取工具定义
4. 注册成功的插件

### `PluginManager::all_tools()`
收集所有插件提供的工具定义。

**返回:** `Vec<ToolDef>` - 所有工具定义的聚合列表

**用途:** 在聊天请求中传递给 LLM，告知可用工具

### `PluginManager::call_tool()`
根据工具名路由到对应插件执行。

**参数:**
- `tool_name`: `&str` - 工具名称
- `input`: `&serde_json::Value` - 工具输入参数

**返回:** `ToolResult` - 工具执行结果

**用途:** 智能体循环中执行工具调用。自动查找拥有该工具的插件并调用其 `call_tool()` 方法

### `DaemonServer::new()`
创建新的守护进程服务器实例。

**参数:**
- `session_mgr`: `Arc<SessionManager>` - 会话管理器
- `llm_backend`: `Option<Arc<dyn LlmBackend>>` - LLM后端
- `task_mgr`: `Arc<Mutex<TaskManager>>` - 定时任务管理器
- `conv_mgr`: `Arc<ConversationManager>` - 对话管理器
- `plugin_mgr`: `Arc<PluginManager>` - 插件管理器

**返回:** `DaemonServer` 实例

**用途:** 初始化守护进程服务器

### `DaemonServer::run()`
启动守护进程服务器并开始监听客户端连接。

**参数:**
- `addr`: `&str` - 监听地址（Unix socket路径）
- `auth_token`: `String` - 认证令牌
- `tls_acceptor`: `Option<TlsAcceptor>` - 可选 TLS 接受器

**返回:** `Result<()>`

**用途:** 启动RPC服务器并处理客户端请求

### `SessionManager::new()`
创建新的会话管理器。

**参数:**
- `omnish_dir`: `PathBuf` - omnish 数据根目录
- `context_config`: `ContextConfig` - 上下文构建配置

**返回:** `SessionManager` 实例

**用途:** 初始化会话管理器，创建必要的目录结构，启动后台写入线程（completions、session updates、samples）

### `SessionManager::load_existing()`
从磁盘加载已存在的会话数据。

**参数:** 无

**返回:** `Result<usize>`（加载的会话数量）

**用途:** 守护进程启动时恢复之前的会话状态。加载完成后释放写锁，再调用 `cleanup_expired_dirs()` 清理过期目录（避免死锁）

### `SessionManager::register()`
注册新会话或更新现有会话。

**参数:**
- `session_id`: `&str` - 会话ID
- `parent_session_id`: `Option<String>` - 父会话ID（可选）
- `attrs`: `HashMap<String, String>` - 会话属性

**返回:** `Result<()>`

**用途:** 客户端连接时注册会话，支持幂等操作（重新连接时更新属性）

### `SessionManager::write_io()`
写入I/O数据到会话流。

**参数:**
- `session_id`: `&str` - 会话ID
- `timestamp_ms`: `u64` - 时间戳（毫秒）
- `direction`: `u8` - 方向（0=输入，1=输出）
- `data`: `&[u8]` - I/O数据

**返回:** `Result<()>`

**用途:** 记录终端输入输出数据

### `SessionManager::receive_command()`
接收并存储命令完成记录。

**参数:**
- `session_id`: `&str` - 会话ID
- `record`: `CommandRecord` - 命令记录

**返回:** `Result<()>`

**用途:** 客户端发送命令完成通知时，填充流偏移量并保存命令记录。同时检查并 flush 该会话的 pending sample（补全采样）

### `SessionManager::end_session()`
结束指定会话。

**参数:**
- `session_id`: `&str` - 会话ID

**返回:** `Result<()>`

**用途:** 客户端断开连接时标记会话结束时间，并 flush 未写入的 pending sample

### `SessionManager::get_session_context()`
获取单个会话的上下文信息。

**参数:**
- `session_id`: `&str` - 会话ID

**返回:** `Result<String>`（格式化后的上下文字符串）

**用途:** 为LLM查询构建当前会话的上下文

### `SessionManager::get_all_sessions_context()`
获取所有会话的上下文信息。

**参数:**
- `current_session_id`: `&str` - 当前会话ID（用于格式化）

**返回:** `Result<String>`（格式化后的上下文字符串）

**用途:** 为LLM查询构建跨会话的完整上下文

### `SessionManager::build_completion_context()`
构建补全专用上下文，优化 KV cache 命中率。

**参数:**
- `current_session_id`: `&str` - 当前会话ID
- `max_context_chars`: `Option<usize>` - 最大上下文字符数

**返回:** `Result<String>`

**用途:** 使用弹性窗口和 `CompletionFormatter` 构建前缀稳定的补全上下文。使用会话属性中的 `shell_cwd`（实时工作目录）作为 `<current_path>` 标签值，而非上一条命令记录的 cwd

### `SessionManager::store_pending_sample()`
存储一个 pending 补全采样。

**参数:**
- `sample`: `PendingSample` - 待采样数据

**用途:** 在 `handle_completion_request()` 获得 LLM 建议后调用

### `SessionManager::update_pending_sample_accepted()`
更新 pending sample 的 accepted 标志。

**参数:**
- `session_id`: `&str` - 会话ID
- `accepted`: `bool` - 补全是否被用户接受

**用途:** 当 `CompletionSummary` 消息到达时调用

### `EventDetector::new()`
创建新的事件检测器。

**参数:**
- `config`: `AutoTriggerConfig` - 自动触发配置

**返回:** `EventDetector` 实例

**用途:** 初始化事件检测器

### `EventDetector::check_output()`
检查输出数据是否匹配触发条件。

**参数:**
- `data`: `&[u8]` - 输出数据

**返回:** `Vec<DetectedEvent>`（检测到的事件列表）

**用途:** 检测stderr输出中的模式匹配事件

### `handle_message()`
处理来自客户端的消息。

**参数:**
- `msg`: `Message` - 协议消息
- `mgr`: `Arc<SessionManager>` - 会话管理器
- `llm`: `&Option<Arc<dyn LlmBackend>>` - LLM后端
- `task_mgr`: `&Arc<Mutex<TaskManager>>` - 任务管理器
- `conv_mgr`: `&Arc<ConversationManager>` - 对话管理器
- `plugin_mgr`: `&Arc<PluginManager>` - 插件管理器

**返回:** `Vec<Message>`（响应消息列表，支持多消息流式响应）

**用途:** 分发处理不同类型的客户端消息，包括：
- `SessionStart/SessionEnd/SessionUpdate` - 会话生命周期
- `IoData` - I/O 数据记录
- `CommandComplete` - 命令完成 + KV cache 预热
- `Request` - LLM 查询或内部命令
- `CompletionRequest/CompletionSummary` - 补全请求和结果汇总
- `ChatStart/ChatMessage/ChatInterrupt` - 多轮聊天（支持工具使用）

**注意:** 返回值从单个 `Message` 改为 `Vec<Message>`，以支持智能体循环中的流式状态消息

### `handle_chat_message()`
处理聊天消息请求，实现智能体循环。

**参数:**
- `cm`: `ChatMessage` - 聊天消息请求
- `mgr`: `&SessionManager` - 会话管理器
- `llm`: `&Option<Arc<dyn LlmBackend>>` - LLM后端
- `conv_mgr`: `&Arc<ConversationManager>` - 对话管理器
- `plugin_mgr`: `&Arc<PluginManager>` - 插件管理器

**返回:** `Vec<Message>` - 包含 ChatToolStatus 和 ChatResponse 的消息列表

**用途:** 实现智能体循环的核心逻辑：
1. 加载对话历史（原始 JSON 格式）
2. 如果是首条消息，注入命令历史并构建 CommandQueryTool
3. 进入循环（最多 5 次迭代）：
   - 调用 LLM（传递工具定义）
   - 检查响应中的 tool_use 块
   - 如有工具调用：执行并发送 ChatToolStatus，继续循环
   - 如无工具调用：跳出循环
4. 存储所有消息（过滤 `<system-reminder>`）
5. 返回最终响应

**流式消息格式:**
- `ChatToolStatus { tool_name, status: "executing", description }` - 每个工具调用发送一次
- `ChatResponse { text, ... }` - 最终响应（仅一次）

### `handle_llm_request()`
处理LLM查询请求。

**参数:**
- `req`: `&Request` - 查询请求
- `mgr`: `&SessionManager` - 会话管理器
- `backend`: `&Arc<dyn LlmBackend>` - LLM后端

**返回:** `Result<LlmResponse>`

**用途:** 构建上下文并调用LLM后端处理用户查询

### `handle_completion_request()`
处理自动补全请求。

**参数:**
- `req`: `&CompletionRequest` - 补全请求
- `mgr`: `&SessionManager` - 会话管理器
- `backend`: `&Arc<dyn LlmBackend>` - LLM后端

**返回:** `Result<Vec<CompletionSuggestion>>`

**用途:** 为shell命令提供智能补全建议。处理完成后：
- 截断建议中的 `&&`（当用户输入不含 `&&` 时）
- 存储 pending sample 用于补全采样
- 禁用 thinking 模式（`enable_thinking: Some(false)`）

### `resolve_chat_context()`
为聊天请求构建上下文。

**参数:**
- `req`: `&Request` - 请求
- `mgr`: `&SessionManager` - 会话管理器
- `max_context_chars`: `Option<usize>` - 最大上下文字符数

**返回:** `Result<String>`

**用途:** 仅包含最近带输出的命令（不含完整历史），根据 `RequestScope` 支持单会话、所有会话或指定会话列表

### UseCase路由

守护进程根据请求类型自动选择合适的LLM后端：
- **Chat**: 用户主动发起的聊天查询（`:` 前缀触发）
- **Completion**: 自动补全请求
- **Analysis**: 自动触发的错误分析

通过`LlmConfig.use_cases`映射配置不同use case使用不同后端，未配置时回退到默认后端。

### KV Cache预热

守护进程支持在补全上下文前缀变化时主动预热LLM的KV cache：
- 检测补全上下文前缀（指令+历史命令部分）是否变化
- 前缀变化时发送预热请求（空输入），使LLM服务器预先缓存前缀对应的KV
- 后续补全请求仅变化末尾的用户输入行，实现高KV cache命中率
- 使用`prefix_match_ratio`日志监控命中率

### `parse_completion_suggestions()`
解析LLM返回的补全建议JSON。

**参数:**
- `content`: `&str` - LLM响应内容

**返回:** `Result<Vec<CompletionSuggestion>>`

**用途:** 从LLM响应中提取结构化的补全建议

## 使用示例

### 启动守护进程
```bash
# 使用默认配置启动
omnishd

# 指定socket路径
OMNISH_SOCKET=/tmp/my-omnish.sock omnishd

# 查看日志输出
RUST_LOG=debug omnishd
```

### 配置文件示例 (daemon.toml)
```toml
listen_addr = "~/.omnish/omnish.sock"

[llm]
default = "claude"

[llm.backends.claude]
backend_type = "anthropic"
model = "claude-3-haiku-20240307"
api_key_cmd = "pass show api/anthropic"
max_content_chars = 200000

[llm.auto_trigger]
on_nonzero_exit = true
on_stderr_patterns = ["error:", "fatal:", "not found"]
cooldown_seconds = 10

[llm.use_cases]
completion = "claude-haiku"
chat = "claude-sonnet"

# 插件配置
[plugins]
enabled = ["example_plugin"]  # 从 ~/.omnish/plugins/ 加载的外部插件列表

[tasks.eviction]
session_evict_hours = 48

[tasks.daily_notes]
schedule_hour = 18

[tasks.disk_cleanup]
schedule = "0 0 */6 * * *"

[context.completion]
max_commands = 50
max_chars = 8000
```

### 程序化使用示例
```rust
use omnish_daemon::session_mgr::SessionManager;
use std::sync::Arc;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 创建会话管理器
    let store_dir = PathBuf::from("~/.omnish");
    let session_mgr = Arc::new(SessionManager::new(store_dir, Default::default()));

    // 加载现有会话
    let count = session_mgr.load_existing().await?;
    println!("Loaded {} existing sessions", count);

    // 注册新会话
    session_mgr.register(
        "session-123",
        None,
        std::collections::HashMap::new(),
    ).await?;

    // 写入I/O数据
    session_mgr.write_io(
        "session-123",
        1000,
        0,  // 0 = 输入
        b"ls -la\n",
    ).await?;

    Ok(())
}
```

### 消息处理流程
```
客户端连接 → 发送SessionStart → 守护进程注册会话
客户端输入 → 发送IoData(Input) → 守护进程记录到流文件
命令执行 → 发送CommandComplete → 守护进程保存命令记录 + flush pending sample + 触发KV cache预热
用户查询 → 发送Request → 守护进程调用LLM后端 → 返回Response
自动补全 → 发送CompletionRequest → 守护进程生成建议 + 存储pending sample → 返回CompletionResponse
补全结果 → 发送CompletionSummary → 守护进程更新pending sample的accepted标志
聊天开始 → 发送ChatStart → 守护进程创建/恢复线程 → 返回ChatReady
聊天消息 → 发送ChatMessage → 守护进程进入智能体循环:
  1. 注册 CommandQueryTool 到 PluginManager（首次）
  2. 加载对话历史 + 工具定义
  3. 调用 LLM（传递工具定义）
  4. 检查 tool_use → 执行工具 → 发送 ChatToolStatus（可能多次）
  5. 循环直到获得最终文本响应
  6. 存储所有消息（包括工具调用）→ 返回 ChatResponse
聊天中断 → 发送ChatInterrupt → 守护进程记录中断标记
客户端断开 → 发送SessionEnd → 守护进程标记会话结束 + flush pending sample
```

## 依赖关系

### 内部依赖
- `omnish-common`: 配置加载
- `omnish-protocol`: 消息协议定义
- `omnish-transport`: RPC传输层
- `omnish-store`: 会话和命令存储、补全采样存储
- `omnish-context`: 上下文构建
- `omnish-llm`: LLM后端集成

### 外部依赖
- `tokio`: 异步运行时
- `anyhow`: 错误处理
- `tracing`: 结构化日志
- `serde`: 序列化/反序列化
- `chrono`: 时间处理
- `tokio-cron-scheduler`: 定时任务调度
- `uuid`: 对话线程ID生成

## 数据持久化

### 会话目录结构
```
~/.omnish/
├── sessions/
│   ├── 2026-02-24T10-30-00Z_session-abc123/
│   │   ├── meta.json          # 会话元数据
│   │   ├── commands.json      # 命令记录
│   │   └── stream.bin         # 二进制I/O流数据
│   └── 2026-02-24T11-15-00Z_session-def456/
│       ├── meta.json
│       ├── commands.json
│       └── stream.bin
├── threads/
│   ├── a1b2c3d4-...-uuid1.jsonl   # 对话线程（每行一个原始 JSON 消息）
│   └── e5f6g7h8-...-uuid2.jsonl
├── logs/
│   ├── completions/           # 补全记录（JSONL）
│   ├── sessions/              # 会话更新记录（JSONL）
│   └── samples/               # 补全采样数据（JSONL）
└── notes/
    ├── 2026-02-24.md          # 日报
    └── hourly/
        └── 2026-02-24/
            └── 14.md          # 14点摘要
```

### 文件格式说明
- `meta.json`: JSON格式的会话元数据（ID、时间戳、属性等）
- `commands.json`: JSON数组格式的命令记录
- `stream.bin`: 二进制格式的I/O流数据，包含时间戳、方向和原始字节
- `*.jsonl`（threads/）: 每行一个原始 JSON 消息，保留完整的 LLM API 格式（包括 tool_use、tool_result 等复杂内容块）。`<system-reminder>` 标签在存储前被过滤。
- `*.jsonl`（logs/samples/）: 每行一个 `CompletionSample` JSON对象

## 并发与锁设计

### RwLock 分层
守护进程使用 `tokio::sync::RwLock` 替代 `Mutex` 管理会话状态，允许多个客户端并行读取会话数据：

- `sessions: RwLock<HashMap<...>>` - 顶层会话映射表，大多数操作仅需读锁
- `Session.meta: RwLock<SessionMeta>` - 会话元数据，读多写少
- `Session.commands: RwLock<Vec<CommandRecord>>` - 命令列表，读多写少
- `Session.stream_writer: Mutex<StreamWriterState>` - 流写入器，独占写入
- `Session.pending_sample: Mutex<Option<PendingSample>>` - 采样状态，短暂持有

### 锁争用修复
- `evict_inactive()`: 两阶段操作 — 先在读锁下扫描候选者，再切换为写锁移除，避免长时间持有写锁
- `cleanup_expired_dirs()`: 先在短暂读锁下快照已加载的会话ID，释放后再进行磁盘I/O，避免读锁持有期间执行文件系统操作导致其他客户端阻塞
- `load_existing()`: 加载完成后显式 `drop(sessions)` 释放写锁，再调用 `cleanup_expired_dirs()`，避免死锁

## 错误处理

守护进程采用以下错误处理策略：
1. **会话级别错误**: 单个会话的错误不会影响其他会话
2. **LLM后端错误**: LLM后端不可用时，查询返回错误信息但不崩溃
3. **存储错误**: 文件系统错误会记录警告但尝试继续运行
4. **网络错误**: 客户端连接错误自动恢复，不影响服务

## 性能考虑

1. **内存使用**: 活跃会话数据常驻内存，历史会话按需加载
2. **并发控制**: 使用`tokio::sync::RwLock`保护会话状态，允许并发读取
3. **I/O优化**: 流数据使用二进制格式，批量写入
4. **上下文构建**: 按需构建上下文，避免不必要的计算
5. **对话缓存**: 对话线程启动时全量加载到内存，后续读取零磁盘I/O
6. **日志抑制**: 过滤 rustls 的 debug 日志（`rustls=off` 指令），防止日志洪泛

## 内部命令

守护进程支持`__cmd:`前缀的内部命令请求，所有命令响应以 JSON 格式返回（包含 `"display"` 字段用于终端展示，部分命令附加结构化数据字段）：

- `__cmd:context [template]` — 获取LLM上下文（支持`completion`、`chat`、`daily-notes`、`hourly-notes`等模板名）
- `__cmd:context chat:<thread_id>` — 获取指定聊天线程的对话上下文
- `__cmd:template [name]` — 查看实际的 LLM 提示词模板（**从 v0.5.0 起移到守护进程处理，显示实际的工具定义**）
  - 支持参数：`chat`、`auto-complete`、`daily-notes`、`hourly-notes`
  - 聊天模板包含实际注册的工具定义（来自 `PluginManager`）
- `__cmd:sessions` — 列出所有活跃会话
- `__cmd:session` — 显示当前会话调试信息
- `__cmd:conversations` — 列出所有聊天对话（含 `thread_ids` 数组），按修改时间降序排列，显示相对时间（如 "12s ago"、"1h ago"）、交换次数、最后问题
- `__cmd:resume` — 恢复最近的对话（等同于 `__cmd:resume 1`），返回 `thread_id`、`last_exchange`、`earlier_count`
- `__cmd:resume N` — 按索引恢复指定对话（1-based），返回相同结构化数据
- `__cmd:conversations del N` — 按索引删除指定对话（1-based），返回 `deleted_thread_id`
- `__cmd:tasks [disable <name>]` — 查看或管理定时任务

这些命令由客户端的`/`命令转发，通过`handle_builtin_command()`函数处理。
