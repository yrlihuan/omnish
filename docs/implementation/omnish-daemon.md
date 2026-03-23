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
7. 定时任务管理（会话驱逐、日报、小时摘要、磁盘清理、对话摘要、自动更新）
8. 管理多轮聊天对话（线程存储、恢复、列表、删除，支持工具使用）
9. 补全采样（pending sample 捕获、JSONL 持久化）
10. 插件管理（基于 tool.json 的元数据插件系统，内置工具 + 外部插件，所有工具强制沙箱；ToolRegistry 统一管理工具元数据）
11. 智能体循环（Agent Loop）实现多轮工具调用，支持流式消息、守护进程侧取消、客户端侧工具转发、并行执行和增量状态更新（最多 100 次迭代）
12. 提示词管理（PromptManager 可组合系统提示词片段，支持用户覆盖）
13. 工具结果格式化（FormatterManager 模块，内置 read/edit/default 格式化器注册表 + 外部格式化器子进程支持）
14. 守护进程日志轮转（每日自动轮转到 `~/.omnish/logs/daemon.log`）
15. 自动更新与优雅重启（升级后以退出码 42 退出，由 systemd 用新二进制重启）
16. 线程级模型选择（每个聊天线程可独立指定 LLM 后端模型）

守护进程以Unix domain socket方式运行，支持多个客户端同时连接。工作线程数上限为 30（`available_parallelism().min(30)`）。

## 重要数据结构

### `DaemonServer`
守护进程服务器主结构，包含：
- `session_mgr`: `Arc<SessionManager>` - 会话管理器实例
- `llm_backend`: `Option<Arc<dyn LlmBackend>>` - 可选的LLM后端
- `task_mgr`: `Arc<Mutex<TaskManager>>` - 定时任务管理器
- `conv_mgr`: `Arc<ConversationManager>` - 对话管理器
- `plugin_mgr`: `Arc<PluginManager>` - 插件管理器
- `pending_agent_loops`: `Arc<Mutex<HashMap<String, AgentLoopState>>>` - 等待客户端工具结果的暂停态智能体循环
- `tool_registry`: `Arc<ToolRegistry>` - 统一工具元数据注册表（display_name、formatter、status_template 等）
- `formatter_mgr`: `Arc<FormatterManager>` - 工具结果格式化管理器（内置格式化器 + 外部格式化器子进程）
- `cancel_flags`: `CancelFlags` - 智能体循环取消标志映射（request_id → AtomicBool），用于守护进程侧 Cancel 支持
- `chat_model_name`: `Option<String>` - 聊天后端的模型名称（用于 ChatReady ghost hint，从配置的 chat use case 或默认后端中提取）

### `AgentLoopState`
暂停态的智能体循环，等待客户端侧工具执行结果返回后恢复，包含：
- `llm_req`: `LlmRequest` - 累积的 LLM 请求（含完整对话历史）
- `prior_len`: `usize` - 本轮对话前已有的消息数量
- `pending_tool_calls`: `Vec<ToolCall>` - 当前轮次的所有工具调用
- `completed_results`: `Vec<ToolResult>` - 已完成的工具结果
- `messages`: `Vec<Message>` - 累积的响应消息（ChatToolStatus 等）
- `iteration`: `usize` - 当前迭代次数
- `cm`: `ChatMessage` - 原始聊天消息请求
- `start`: `Instant` - 循环开始时间（用于超时检测）
- `command_query_tool`: `CommandQueryTool` - 命令查询工具实例
- `effective_backend`: `Arc<dyn LlmBackend>` - 本次循环实际使用的后端（保留线程级模型覆盖，防止恢复后退回默认后端）
- `cancel_flag`: `Arc<AtomicBool>` - 守护进程侧取消标志，由 `ChatInterrupt` 设置，在每次迭代入口及工具执行间检查

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
元数据驱动的插件管理器，从 `~/.omnish/plugins/` 下各子目录的 `tool.json` 文件加载工具定义，包含：
- `plugins_dir`: `PathBuf` - 插件根目录路径
- `plugins`: `Vec<PluginInfo>` - 已加载的插件信息列表
- `tool_index`: `HashMap<String, (usize, usize)>` - 工具名 → (plugin_index, tool_index) 的快速查找表
- `prompt_cache`: `RwLock<PromptCache>` - 缓存的工具描述覆盖（来自 `tool.override.json`）

**主要特点:**
- **tool.json 驱动**：每个插件子目录必须包含 `tool.json`，定义 `plugin_type`（`"client_tool"` 或 `"daemon_tool"`）和工具列表
- **插件类型分类**（`PluginType` 枚举）：
  - `DaemonTool` — 工具在守护进程内执行（如 `omnish_list_history`、`omnish_get_output`）
  - `ClientTool` — 工具转发到客户端执行（如 `bash`、`read`、`edit`、`write`、`glob`、`grep` 等），客户端启动 `omnish-plugin` 子进程执行
- **工具定义聚合**：通过 `all_tools()` 收集所有插件的工具定义，应用 `tool.override.json` 覆盖后返回
- **状态模板插值**：每个工具可定义 `status_template`（如 `"执行: {command}"`），`tool_status_text()` 将 `{field}` 替换为实际输入参数
- **沙箱标记**：所有工具强制启用沙箱（`sandboxed` 字段已不再允许 opt-out），Landlock 沙箱对所有工具均有效
- **tool.override.json 覆盖**：用户可在插件目录下放置 `tool.override.json` 来替换（`description`）或追加（`append`）工具描述
- **inotify 热重载**：Linux 上通过 inotify 监视 `tool.override.json` 文件变更，自动调用 `reload_overrides()` 更新；非 Linux 平台每 5 秒轮询
- **内嵌资源自动安装**：守护进程启动时将编译期内嵌的 `tool.json` 写入 `~/.omnish/plugins/builtin/`（每次启动覆盖），`tool.override.json.example` 仅在不存在时写入
- **按需自动安装捆绑插件**：`auto_install_bundled_plugins()` 检测 `daemon.toml` 中 `[tools.<name>]` 配置是否包含 `api_key` 字段，若有则自动将对应捆绑插件（如 `web_search`）写入 `~/.omnish/plugins/` 目录
- **ToolRegistry 集成**：`register_all()` 方法将所有已加载插件的工具元数据（display_name、formatter 名称、status_template、plugin_type、plugin_name）批量注册到 `ToolRegistry` 中，供智能体循环统一查询
- **多行描述支持**：`tool.json` 和 `tool.override.json` 中的 `description` 字段可以是字符串或字符串数组（以 `\n` 拼接）

### `PluginInfo`（内部结构）
单个插件的加载信息：
- `dir_name`: `String` - 插件子目录名（如 `builtin`）
- `plugin_type`: `PluginType` - 插件类型
- `tools`: `Vec<ToolEntry>` - 工具条目列表

### `ToolEntry`（内部结构）
单个工具的定义和元数据：
- `def`: `ToolDef` - 工具定义（名称、描述、JSON Schema）
- `display_name`: `String` - 工具显示名称（可在 `tool.json` 中指定，默认等于工具名）
- `status_template`: `String` - 状态文本模板
- `formatter`: `String` - 格式化器名称（如 `"read"`、`"edit"`、`"default"`）
- `formatter_binary`: `Option<String>` - 外部格式化器二进制路径（来自 `tool.json` 的 `formatter_binary` 字段）
- `sandboxed`: `bool` - 是否启用沙箱

### `ToolRegistry`
统一工具元数据注册表（定义在 `crates/omnish-daemon/src/tool_registry.rs`），在守护进程启动时由 `PluginManager::register_all()` 和 `CommandQueryTool::register()` 填充，此后以 `Arc` 共享只读使用。

**核心字段：**
- `tools`: `HashMap<String, ToolMeta>` - 工具名 → 元数据（display_name、formatter、status_template、custom_status、plugin_type、plugin_name）
- `defs`: `HashMap<String, ToolDef>` - 工具名 → 工具定义（供智能体循环传递给 LLM）
- `descriptions`: `RwLock<HashMap<String, String>>` - 运行时描述覆盖（热重载时更新）
- `override_params`: `RwLock<HashMap<String, HashMap<String, Value>>>` - 运行时参数覆盖

**主要方法：**
- `register(meta)` / `register_def(def)` — 启动时注册工具元数据和定义
- `display_name(tool_name)` — 返回工具显示名，未注册时回退到工具名
- `formatter_name(tool_name)` — 返回格式化器名称，未注册时回退 `"default"`
- `status_text(tool_name, input)` — 若设置了 `custom_status` 则调用之，否则使用 `status_template` 插值
- `plugin_type(tool_name)` / `plugin_name(tool_name)` — 查询插件类型和插件目录名
- `all_defs()` — 返回所有工具定义（应用运行时描述覆盖后）
- `update_overrides(descriptions, override_params)` — 热重载时原子更新运行时覆盖

**ToolMeta 结构：**
- `name`: `String` - 工具名
- `display_name`: `String` - 工具显示名
- `formatter`: `String` - 格式化器名称
- `status_template`: `String` - 状态文本模板
- `custom_status`: `Option<CustomStatusFn>` - 自定义状态文本函数（`Arc<dyn Fn(&str, &Value) -> String>`）
- `plugin_type`: `Option<PluginType>` - 插件类型（DaemonTool / ClientTool）
- `plugin_name`: `Option<String>` - 所属插件目录名

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
- **全量对话历史**：`get_all_exchanges()` 提取线程中所有用户-助手交换对，用于 `/resume` 显示完整历史
- **线程元数据**（`ThreadMeta`）：每个线程有对应的 `.meta.json` sidecar 文件，包含：
  - `host`: 会话主机名
  - `cwd`: 会话工作目录
  - `summary`: LLM 生成的线程摘要（由 `thread_summary` 任务生成）；`/resume` 时在对话选择界面中显示摘要
  - `summary_rounds`: 生成摘要时的对话轮次数
  - `model`: 线程级别的模型覆盖（per-thread model override）
- **元数据延迟写入**：`ThreadMeta`（host/cwd）在 `ChatStart` 时**不立即写入磁盘**，而是推迟到第一条 `ChatMessage` 到达时才保存。这样可以防止用户取消 `/resume` 选择（即中断 ChatStart 流程）时错误覆盖线程已有的 host/cwd 记录
- **线程恢复 UX 增强**：
  - **host/cwd 不匹配提示**：恢复对话时若检测到当前主机或工作目录与线程记录不符，向用户提问是否继续（而非静默继续）
  - **锁感知选择器**：`/resume` 选择界面中，已被其他会话锁定的线程会显示锁定信息，用户仍可选择但会收到提示
  - **摘要展示**：对话选择界面显示每个线程的 `ThreadMeta.summary`（若已生成）

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

### `FileWatcher`
共享文件监视模块（定义在 `crates/omnish-daemon/src/file_watcher.rs`），为 `ConfigWatcher` 和 `PluginManager` 提供统一的文件变更通知基础设施。

**架构：**
- Linux：使用单个 `inotify` 实例监视所有注册路径。对于文件路径，监视其**父目录**（使用 `IN_CREATE | IN_CLOSE_WRITE | IN_MOVED_TO` 事件），以兼容编辑器的 save-and-rename 模式（vim、`sed -i` 等）
- 非 Linux：每 5 秒轮询检查文件修改时间
- `watch(path)` 返回 `tokio::sync::watch::Receiver<()>`，文件变更时触发通知
- 同一 `FileWatcher` 实例可被多个模块共享（`Arc` 包装），避免重复创建 inotify 描述符

### `ConfigWatcher`
配置文件热重载模块（定义在 `crates/omnish-daemon/src/config_watcher.rs`），监视 `daemon.toml` 变更并通过**分节点发布/订阅**机制通知各模块。

**核心结构：**
- `config_path`: `PathBuf` - 监视的配置文件路径
- `current`: `RwLock<DaemonConfig>` - 当前配置副本
- `senders`: `HashMap<ConfigSection, watch::Sender<Arc<DaemonConfig>>>` - 各节点的通知发送方

**`ConfigSection` 枚举：**`Tools`、`Sandbox`、`Context`、`Llm`、`Tasks`、`Plugins`

**工作流程：**
1. `ConfigWatcher::new()` 注册 `FileWatcher` 监视，内部 spawn 一个 reload 任务
2. 文件变更时调用 `reload()`：文件读取和 TOML 解析在锁外完成，再短暂获取写锁对比各节点差异
3. 仅发生变更的节点才会向订阅者发送通知（目前已实现 `Sandbox` 节点的 diff 检测）
4. 各模块通过 `subscribe(section)` 获取 `watch::Receiver`，在自身 spawn 的任务中等待变更并更新本地状态

**SandboxRules 热重载集成：**
- `server.rs` 中 `SandboxRules`（`HashMap<String, Vec<PermitRule>>`）存储在 `Arc<RwLock<...>>` 中
- `ConfigWatcher::subscribe(ConfigSection::Sandbox)` 的接收端在后台任务中监听，收到变更后调用 `sandbox_rules::compile_config()` 重新编译规则并原子替换

### `SandboxRules`（`crates/omnish-daemon/src/sandbox_rules.rs`）
沙箱许可规则模块，支持为特定工具配置**白名单规则**，允许特定命令参数绕过 Landlock 沙箱限制。

**规则格式：**`<param_field> <operator> <value>`

**支持的操作符：**
- `starts_with` — 字段值以 value 开头
- `contains` — 字段值包含 value
- `equals` — 字段值等于 value
- `matches` — 字段值匹配正则表达式 value

**示例配置（`daemon.toml`）：**
```toml
[sandbox.plugins.bash]
permit_rules = [
  "command starts_with glab",
  "command contains docker",
]
```

**工作原理：**
- `compile_config()` 在守护进程启动和配置热重载时解析并预编译所有规则（正则表达式提前编译为 `Regex` 对象）
- `check_bypass(rules, input)` 对工具调用输入进行 OR 逻辑匹配，命中任一规则则返回该规则字符串（供日志记录），未命中返回 `None`
- 命中白名单规则的工具调用将不应用 Landlock 沙箱（由 `ChatToolCall` 消息的 `sandboxed` 字段控制）

## 插件系统

### 架构概述

插件系统采用 **元数据 + 子进程** 的分离架构：
- **守护进程（PluginManager）**：只负责加载 `tool.json` 定义、管理工具元数据、路由工具调用
- **执行层**：
  - `DaemonTool`（如 `command_query`）直接在守护进程内执行
  - `ClientTool`（如 `bash`、`read`、`edit`、`grep`、`glob`、`write`）由客户端启动 `omnish-plugin` 子进程执行

### 内置工具（builtin 插件）

内置工具定义在 `crates/omnish-plugin/assets/tool.json` 中，编译期内嵌到二进制文件，启动时写入 `~/.omnish/plugins/builtin/tool.json`。所有内置工具的 `plugin_type` 为 `"client_tool"`，由客户端侧的 `omnish-plugin` 可执行文件执行。

#### bash 工具
执行 shell 命令，支持可选超时（默认 120 秒，最大 600 秒）。
- 输入参数：`command`（必需）、`description`、`shell`、`cwd`、`timeout`
- 沙箱：启用（Landlock 限制写入范围）
- 输出截断：超过 30000 字符时截断

#### read 工具
读取文件内容，返回带行号的文本。
- 输入参数：`file_path`（必需，绝对路径）、`offset`（起始行号）、`limit`（行数，默认 2000）
- 超长行自动截断（2000 字符）
- 沙箱：启用

#### edit 工具
精确字符串替换编辑文件。
- 输入参数：`file_path`（必需）、`old_string`（必需）、`new_string`（必需）、`replace_all`
- `old_string` 必须在文件中唯一匹配（除非 `replace_all`）
- 沙箱：启用

#### write 工具
创建或覆盖写入文件。
- 输入参数：`file_path`（必需）、`content`（必需）
- 沙箱：启用

#### glob 工具
快速文件模式匹配，按修改时间排序返回。
- 输入参数：`pattern`（必需，如 `"**/*.rs"`）、`path`
- 沙箱：启用

#### grep 工具
基于 ripgrep 的内容搜索（原生 Rust 实现，不依赖外部 `rg` 命令）。
- 输入参数：`pattern`（必需，正则表达式）、`path`、`glob`、`type`、`output_mode`、`multiline`、`-i`、`-n`、`-A`、`-B`、`-C`/`context`、`head_limit`、`offset`
- 三种输出模式：`files_with_matches`（默认）、`content`、`count`
- 支持多行匹配（`multiline: true`）
- 沙箱：启用

### CommandQueryTool

守护进程内置的 `DaemonTool`，用于查询命令历史和获取完整命令输出，定义在 `crates/omnish-daemon/src/tools/command_query.rs`。

**工具定义（拆分为两个独立工具）:**

- **`omnish_list_history`**
  - 功能：列出最近 N 条命令（默认 20），包含序号、命令行、退出码、相对时间
  - 输入参数：`count`（可选，整数）
  - 说明：最近 5 条命令已包含在 `<system-reminder>` 中，只有需要更多历史时才调用此工具

- **`omnish_get_output`**
  - 功能：获取指定序号命令的完整输出（自动跳过回显行，限制 500 行 / 50KB）
  - 输入参数：`seq`（必需，整数，从 `omnish_list_history` 或 `<system-reminder>` 获取）

**实现细节:**
- 构造时传入所有会话的 `commands` 和 `stream_reader`
- 输出自动截断并显示总行数，防止响应过大
- 提供 `build_system_reminder()` 生成 `<system-reminder>` 标签内容
- 提供 `status_text()` 生成中文状态文本（`omnish_list_history` → "查询命令历史..."，`omnish_get_output` → "获取命令输出 [seq]..."）

### 外部插件

外部插件放置在 `~/.omnish/plugins/<name>/` 目录下，包含 `tool.json` 文件。工具执行由客户端启动的 `omnish-plugin` 子进程处理。

**Landlock 沙箱（Linux）：**
- 沙箱通过 `omnish_plugin::apply_sandbox()` 在 `pre_exec`（fork 后、exec 前）应用
- 读取权限：全文件系统
- 写入权限：仅限 `data_dir`、`/tmp`、`/dev/null`、以及可选的当前工作目录
- 所有工具均应用沙箱（无 opt-out），非 Linux 平台为 no-op

### PROMPT.md 支持

插件目录下可放置 `PROMPT.md` 文件，提供额外的系统提示词片段，由 `PromptManager` 加载合并到聊天系统提示词中。

### tool.json 文件格式

```json
{
  "plugin_type": "client_tool",
  "formatter_binary": "/path/to/my_formatter",
  "tools": [
    {
      "name": "bash",
      "display_name": "Bash",
      "description": "Run commands",
      "description": ["Line 1", "Line 2"],
      "input_schema": { "type": "object", "properties": {...}, "required": [...] },
      "status_template": "执行: {command}",
      "formatter": "default",
      "sandboxed": true
    }
  ]
}
```

### tool.override.json 文件格式

```json
{
  "tools": {
    "bash": {
      "description": "替换整个描述"
    },
    "read": {
      "append": "追加到原始描述末尾"
    }
  }
}
```

`description` 优先于 `append`；两者均支持字符串或字符串数组。

## 工具格式化模块

### FormatterManager（`crates/omnish-daemon/src/formatter_mgr.rs`）

`FormatterManager` 替代了旧的 `formatter.rs` 静态查找模式，提供统一的格式化器注册表，支持内置格式化器和外部格式化器子进程。

格式化器接口和内置实现定义在 `crates/omnish-plugin/src/formatter.rs`（供 omnish-plugin 和 omnish-daemon 共用）。

#### 核心结构

**`FormatInput`**（定义在 `omnish-plugin`）：
- `tool_name`: `String` - 工具名称
- `params`: `serde_json::Value` - 工具输入参数
- `output`: `String` - 工具执行输出（格式化器仅在工具完成后调用）
- `is_error`: `bool` - 是否为错误结果

**`FormatOutput`**（定义在 `omnish-plugin`）：
- `result_compact`: `Vec<String>` - 精简结果（适合折叠显示）
- `result_full`: `Vec<String>` - 完整结果（适合展开显示）

**`ToolFormatter` trait**（定义在 `omnish-plugin`）：
```rust
pub trait ToolFormatter: Send + Sync {
    fn format(&self, input: &FormatInput) -> FormatOutput;
}
```

**`FormatterManager`** — 格式化器注册表：
- `builtins`: `HashMap<String, Box<dyn ToolFormatter>>` - 内置格式化器（`"default"`、`"read"`、`"edit"`/`"write"`）
- `externals`: `HashMap<String, ExternalFormatter>` - 外部格式化器（每个为一个长驻子进程）

#### 内置格式化器

**`DefaultFormatter`**（`"default"`，适用于 bash、grep、glob 等）：
- `result_compact`：输出前 5 行（超出时追加 `"(+N more lines)"` 提示）
- `result_full`：超过 50 行时截断为头 20 行 + 尾 20 行（中间插入 `"... (N lines omitted) ..."` 分隔）

**`ReadFormatter`**（`"read"`）：
- 成功时 `result_compact`：`"Read N lines"`
- 成功时 `result_full`：行数 ≤ 10 时显示带行号的 `cat -n` 格式内容；行数 > 10 时显示行数摘要
- 错误时：同 DefaultFormatter

**`EditFormatter`**（`"edit"` / `"write"`）：
- 成功时 `result_compact`：编辑摘要（如 `"Edited 1 line"`、`"Added 2 lines, removed 3 lines"`）+ 最多 50 行带颜色行号的 diff
- 成功时 `result_full`：编辑摘要 + 全部带颜色行号的 diff（仅显示变更行，不再包含完整旧/新内容）
- diff 格式：ANSI 颜色（红色 `-` 删除行、绿色 `+` 新增行、暗色上下文行），行号右对齐
- `replace_all` 多处替换时追加 `"... and N more places"` 提示
- 错误时：输出全文 + `old_string` 内容（辅助调试）

#### 外部格式化器子进程

当插件的 `tool.json` 包含 `formatter_binary` 字段时，守护进程在启动时通过 `register_external()` 启动一个长驻格式化器子进程。子进程通过 stdin/stdout 进行 newline-delimited JSON 通信：

**请求格式（一行 JSON）：**
```json
{"formatter": "格式化器名", "tool_name": "工具名", "params": {...}, "output": "原始输出", "is_error": false}
```

**响应格式（一行 JSON）：**
```json
{"summary": "可选摘要行", "compact": ["精简输出行"], "full": ["完整输出行"]}
```

- `ExternalFormatter` 内部使用 `mpsc` 队列序列化请求（保证顺序）
- 每次格式化调用超时 5 秒，超时返回 `"Formatter timeout"`
- 子进程启动失败时记录警告并跳过（不影响其他格式化器）

#### 格式化器选择顺序

`FormatterManager::format(formatter_name, input)` 的查找顺序：
1. `externals` 中查找 `formatter_name` — 优先使用外部格式化器
2. `builtins` 中查找 `formatter_name` — 匹配内置格式化器
3. 回退到 `"default"` 内置格式化器

格式化器名称来自 `ToolRegistry::formatter_name(tool_name)`，由各插件的 `tool.json` 中 `formatter` 字段指定（默认 `"default"`）。

#### 使用场景

格式化器仅在工具**完成后**调用（工具调用前的 Running 状态由 `ToolRegistry::status_text()` 生成，不经过 FormatterManager）：
- **DaemonTool 完成**：直接在 `run_agent_loop()` 内调用 `FormatterManager::format()` 生成格式化结果
- **ClientTool 完成**：`handle_tool_result()` 收到 `ChatToolResult` 后调用，生成增量 `ChatToolStatus` 更新

## 提示词管理

### PromptManager

`PromptManager`（定义在 `crates/omnish-llm/src/prompt.rs`）管理可组合的系统提示词片段，用于构建聊天的系统提示词。

**核心概念：**
- 系统提示词由多个**命名片段**（fragments）组成，按插入顺序拼接
- 基础片段来自编译期内嵌的 `chat.json`（存储在 `~/.omnish/prompts/chat.json`）
- 用户可通过 `~/.omnish/prompts/chat.override.json` 覆盖或追加片段

**片段合并规则（`merge()`）：**
- 覆盖文件中 `name` 匹配的片段替换基础内容
- 不匹配的片段追加到末尾

**内嵌资源：**
- `CHAT_PROMPT_JSON` — 编译期内嵌的聊天提示词 JSON
- `CHAT_OVERRIDE_EXAMPLE` — 覆盖文件的示例模板

**启动时行为：**
- `chat.json` 每次启动覆盖写入 `~/.omnish/prompts/`
- `chat.override.json.example` 仅在不存在时写入

### system-reminder

每条聊天用户消息自动附加 `<system-reminder>` 标签，包含丰富的环境上下文：

```
<system-reminder>
TIME: 2026-03-15 10:30:00 +0800

WORKING DIR: /home/user/project

Is directory a git repo: Yes

Platform: linux

OS Version: Linux 6.8.0-86-generic

Today's date: 2026-03-15

LAST 5 COMMANDS:
[seq=1] cargo build  (exit 0, 5m ago)
[seq=2] cargo test [FAILED]
...
</system-reminder>
```

**生成逻辑（`CommandQueryTool::build_system_reminder()`）：**
- 当前时间（含时区）和日期（已移除 `TIME` 字段，只保留 `Today's date`）
- 工作目录（优先使用会话探测的实时 `shell_cwd`，回退到最后命令记录的 cwd）
- Git 仓库检测
- 平台和操作系统版本（**来自客户端会话属性** `platform`/`os_version`，而非守护进程自身环境；客户端通过 `SessionUpdate` 上报探测结果）
- 最近 5 条命令（标记失败命令为 `[FAILED]`，过滤掉空命令和未知命令）

## 工具使用与智能体循环

### 工具使用（Tool-Use）集成

守护进程通过 `PluginManager` 为 LLM 提供工具使用能力，允许 LLM 在聊天过程中调用工具获取额外信息。

**工具定义:**
- 工具定义（`ToolDef`）包含工具名、描述和 JSON Schema 输入规范
- `omnish_list_history` 和 `omnish_get_output` 工具的定义由 `CommandQueryTool::definitions()` 生成
- 所有其他工具定义通过 `plugin_mgr.all_tools()` 收集（应用 `tool.override.json` 覆盖后）
- 两者合并后传递给 LLM

**工具调用流程（双路分发）：**
1. LLM 在响应中返回 `tool_use` 内容块（包含工具名、ID 和输入参数）
2. 守护进程检查工具的 `PluginType`：
   - **DaemonTool**（如 `command_query`）：直接在守护进程内执行
   - **ClientTool**（如 `bash`、`read`、`edit`）：通过 `ChatToolCall` 消息转发到客户端
3. 客户端执行完毕后通过 `ChatToolResult` 消息返回结果
4. 所有工具结果收集完毕后，构建 `tool_result` 内容块发送回 LLM

**并行工具执行：**
- 当 LLM 一次返回多个 `tool_use` 块时：
  - DaemonTool 立即执行
  - 所有 ClientTool 同时通过 `ChatToolCall` 消息转发给客户端
  - 客户端并行执行后逐个返回 `ChatToolResult`
  - 守护进程等待所有结果到齐后才恢复智能体循环

**流式消息（Streaming）：**
- 智能体循环的所有消息通过 `mpsc::Sender<Vec<Message>>` **增量推送**给客户端，而不是等待循环结束后一次性返回
- LLM 的文本块（如 "I'll run this command"）通过 `ChatToolStatus` 实时转发给客户端显示
- 每个工具调用在**调用前**发送一条 `ChatToolStatus`（`Running` 状态，含 `param_desc`）
- 每个工具调用在**完成后**再发送一条 `ChatToolStatus`（`Success`/`Error` 状态，含 `result_compact`/`result_full`）
- `ChatToolStatus` 结构化字段：`tool_call_id`、`status_icon`（`StatusIcon` 枚举）、`display_name`、`param_desc`、`result_compact`（`Vec<String>`）、`result_full`（`Vec<String>`）
- 并行工具执行时，每个工具完成后**立即**发送增量状态更新（不等待其他工具完成），由 `handle_tool_result()` 在累积结果的同时同步返回

### 智能体循环（Agent Loop）

聊天处理实现了智能体循环模式，允许 LLM 进行多轮工具调用直到获得足够信息回答用户问题。

**循环机制:**
- 最多迭代 100 次（`max_iterations = 100`，从旧版 30 次提升）
- 超时限制 600 秒（10 分钟），超时后清理暂停态并返回错误
- 每次迭代：调用 LLM → 检查是否有工具调用 → 执行工具 → 将结果反馈给 LLM
- **使用线程级后端**：循环始终通过 `state.effective_backend` 调用 LLM，在客户端工具返回后恢复循环时也使用同一后端（修复了恢复后退回默认后端的 bug）
- **Thinking 块保留**：当 LLM 响应中包含 `ContentBlock::Thinking` 块时，assistant 消息以内容数组形式存储（thinking + text + tool_use），确保 thinking 上下文在多轮工具调用中正确传递
- 循环终止条件：
  - LLM 返回文本响应（无 `tool_use` 块）
  - 达到最大迭代次数
  - 遇到错误
  - 超时

**客户端侧工具转发（暂停/恢复机制）：**
- 当 LLM 请求的工具包含 `ClientTool` 时，循环暂停：
  1. 将当前 `AgentLoopState` 存入 `pending_agent_loops` 映射（以 `request_id` 为 key）
  2. 返回 `ChatToolCall` 消息给客户端
  3. 客户端执行后返回 `ChatToolResult`
  4. `handle_tool_result()` 累积结果，当所有工具完成后从映射中取出 state 恢复循环
- 后台定时器每 30 秒清理超过 120 秒的过期暂停态

**守护进程侧 Cancel（流式执行中断）：**
- 用户按 Ctrl-C 时，客户端发送 `ChatInterrupt`
- 若智能体循环正在守护进程内**主动执行**（非等待客户端工具结果），守护进程通过 `cancel_flags` 中对应 `request_id` 的 `AtomicBool` 标志通知循环中止
  - 每次迭代入口检查取消标志
  - DaemonTool 执行期间每个工具完成后也检查取消标志
- 若循环已暂停等待客户端工具结果，守护进程从 `pending_agent_loops` 中取出暂停态
- 已完成的工具结果正常存储，未完成的标记为 `"user interrupted"` 错误
- 所有消息（包括部分结果）持久化到对话线程

**消息流格式:**
```
User: {{query}} + <system-reminder>
Assistant: [text] + [tool_use blocks]
  → ChatToolStatus (LLM 文本) + ChatToolStatus (工具状态) + ChatToolCall (客户端工具)
  → 等待 ChatToolResult...
User: [tool_result blocks]
Assistant: [text] + [tool_use blocks]
  → ...
Assistant: {{final response}}
```

**存储格式:**
- 所有消息（包括工具调用和结果）以原始 JSON 格式存储到对话线程
- 用户消息存储时去除 `<system-reminder>`（只保留用户原始查询文本）
- 工具结果消息的 `content` 是数组（不是字符串），`ConversationManager` 能正确区分
- 不持久化 `<system-reminder>` 标签，保持线程文件简洁

**ChatToolCall 消息结构：**
- `request_id` / `thread_id` — 关联到智能体循环
- `tool_name` — 工具名
- `tool_call_id` — LLM 分配的工具调用 ID
- `input` — 工具输入参数（JSON 字符串，bincode 兼容）
- `plugin_name` — 插件目录名（如 `"builtin"`）
- `sandboxed` — 是否应用 Landlock 沙箱

**ChatToolResult 消息结构：**
- `request_id` / `thread_id` — 关联到智能体循环
- `tool_call_id` — 对应的工具调用 ID
- `content` — 工具执行结果文本
- `is_error` — 是否为错误结果

### Thinking 模式

聊天请求启用 thinking 模式（`enable_thinking: Some(true)`），允许 LLM 在回答前进行深度推理。补全请求则禁用 thinking 模式（`enable_thinking: Some(false)`）以减少延迟。

**Thinking 块保留（多轮工具调用）：**
- LLM 响应的 `ContentBlock` 枚举包含 `Thinking(String)` 变体
- `run_agent_loop()` 在构建 assistant 消息时，若响应中含有 `Thinking` 块，以完整内容数组（thinking + text + tool_use）而非纯字符串序列化，确保 thinking 上下文在后续工具结果循环中正确传递给 LLM
- 最终响应（非工具调用轮次）同样保留 thinking 块存入对话历史

### 聊天上下文增强

**system-reminder 注入:**
- 每条聊天消息自动附加 `<system-reminder>` 标签（由 `CommandQueryTool::build_system_reminder(5, live_cwd)` 生成）
- 包含时间、工作目录、Git 仓库状态、平台信息、最近 5 条命令
- 减少简单环境查询的工具调用次数，提升响应速度
- 工具仍可用于获取完整输出或更多历史记录

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
列出所有对话，按修改时间降序排列。返回 `(thread_id, last_modified, exchange_count, last_question)` 元组列表。

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

##### `ConversationManager::get_all_exchanges()`
获取线程中所有用户-助手交换对，按时间顺序返回。

**参数:**
- `thread_id`: `&str` - 线程 ID

**返回:** `Vec<(String, String)>` — 每对为 `(user_text, assistant_text)`

**特性:**
- 自动区分用户输入消息和工具结果消息（content 为字符串 vs 数组）
- 多轮工具调用场景中，多个 assistant 消息的文本拼接为一条
- 用于 `/resume` 命令显示完整对话历史

##### `ConversationManager::is_user_input()` 和 `extract_text()`
内部辅助方法，用于处理原始 JSON 消息：

**`is_user_input(msg)`:**
- 检查消息是否为用户输入消息（`role == "user"` 且 `content` 为字符串）
- 工具结果消息的 `content` 是数组，不算用户输入

**`extract_text(msg)`:**
- 从消息中提取显示文本
- 支持字符串内容和数组内容（提取所有 `type: "text"` 块）
- `extract_text_public()` 为公开访问版本（用于 `server.rs` 的显示处理）

### 聊天消息流程

```
客户端发送 ChatStart → 守护进程创建/恢复线程 → 返回 ChatReady（含线程ID）
客户端发送 ChatMessage → 守护进程进入智能体循环:
  1. 构建 ChatSetup（工具列表 + 系统提示词）
  2. 附加 system-reminder（时间/cwd/最近5条命令）到用户消息
  3. 加载对话历史
  4. 调用 LLM（传递工具定义，启用 thinking 模式）
  5. 检查 tool_use 块:
     - DaemonTool: 直接执行
     - ClientTool: 发送 ChatToolCall，暂停循环等待结果
  6. 收到所有 ChatToolResult 后恢复循环
  7. 循环直到获得最终文本响应（最多 100 次迭代 / 600 秒超时）
  8. 存储所有消息（去除 system-reminder）→ 返回 ChatResponse
客户端发送 ChatToolResult → 守护进程累积结果 → 全部完成时恢复智能体循环
客户端发送 ChatInterrupt → 守护进程存储部分结果 → 清理暂停态 → 返回 Ack
```

**ChatMessage 处理细节:**
- 每条消息都构建 `ChatSetup`（通过 `build_chat_setup()` 共享函数）
- `ChatSetup` 包含 `CommandQueryTool`、合并的工具列表、系统提示词
- 系统提示词通过 `PromptManager` 加载：基础 `chat.json`（编译期内嵌，启动时写入 `~/.omnish/plugins/builtin/tool.json`）+ 用户 `chat.override.json`
- system-reminder 包含实时 `shell_cwd`（从会话探测获取）
- **线程级模型选择**：`ChatMessage.model` 字段指定模型名时，保存到 `ThreadMeta.model`，后续对话从元数据中读取并通过 `backend.get_backend_by_name()` 解析为具体后端
- 所有消息（包括工具调用）以原始 JSON 格式存储，但 `<system-reminder>` 在存储前被过滤
- thinking 块以完整 `ContentBlock::Thinking` 数组存储到对话历史，供后续轮次使用

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
**特点:** LLM上下文中包含当天已有的小时摘要，帮助生成更准确的日报。日报内容中还包含当天所有聊天对话线程的摘要（来自 `ThreadMeta.summary`）
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

#### 5. `thread_summary` - 对话摘要任务
**执行周期:** 每10分钟 (`0 */10 * * * *`)
**功能:** 扫描所有对话线程，为有新对话轮次的线程生成或更新摘要，存储到线程的 `.meta.json` sidecar 文件中（`ThreadMeta.summary` 字段）
**特点:**
- 只对有内容（`rounds > 0`）且摘要已过期（新增轮次超过阈值）的线程生成摘要
- 摘要间隔可通过配置调整（`periodic_summary_interval`，默认 4 小时）
- 调用 LLM 后端生成简短摘要文本
- 无 LLM 后端时自动跳过
**实现:** 通过 `create_thread_summary_job()` 函数创建

#### 6. `auto_update` - 自动更新任务
**执行周期:** 可配置（默认不启用）
**功能:** 自动从 GitHub 或本地目录下载并安装新版本，完成后优雅重启守护进程
**机制:**
- Phase 1：运行 `~/.omnish/install.sh --upgrade` 升级服务端二进制
- Phase 2：运行 `~/.omnish/deploy.sh` 将新版本分发到配置的客户端机器
- 升级成功后通知 `restart_signal`（`Arc<Notify>`），主循环检测到信号后以退出码 42（`EXIT_RESTART`）退出
- systemd 的 `Restart=on-failure` 配置使其自动用新二进制重启
- SIGUSR1 信号也可触发同样的 42 退出码重启流程
**相关配置:**
```toml
[tasks.auto_update]
enabled = true
schedule = "0 0 3 * * *"   # 每天凌晨3点检查
check_url = "https://github.com/..."  # 可选，默认使用 GitHub
clients = ["user@host1", "user@host2"]  # 要分发的客户端机器列表
```
**实现:** 通过 `create_auto_update_job()` 函数创建（`crates/omnish-daemon/src/auto_update.rs`）

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

[tasks.auto_update]
enabled = true
schedule = "0 0 3 * * *"
clients = ["user@host1"]
check_url = "https://github.com/..."

[context.hourly_summary]
head_lines = 50
tail_lines = 100
max_line_width = 128
```

## 关键函数说明

### `PluginManager::load()`
从指定目录加载所有插件。每个包含 `tool.json` 的子目录被视为一个插件。

**参数:**
- `plugins_dir`: `&Path` - 插件根目录

**返回:** `PluginManager` 实例

**用途:** 初始化插件管理器，解析所有 `tool.json` 文件，构建工具索引，加载 `tool.override.json` 覆盖

### `PluginManager::reload_overrides()`
重新读取所有 `tool.override.json` 文件并更新提示词缓存。

**用途:** 由 inotify 监视器或轮询定时器在检测到文件变更时调用

### `PluginManager::all_tools()`
收集所有插件提供的工具定义（应用描述覆盖后）。

**返回:** `Vec<ToolDef>` - 所有工具定义的聚合列表

**用途:** 在聊天请求中传递给 LLM，告知可用工具

### `PluginManager::tool_status_text()`
根据工具名和输入参数生成状态文本。

**参数:**
- `tool_name`: `&str` - 工具名称
- `input`: `&serde_json::Value` - 工具输入参数

**返回:** `String` - 插值后的状态文本

**用途:** 生成 `ChatToolStatus` 消息的显示文本

### `PluginManager::tool_plugin_type()`
查询工具的插件类型。

**返回:** `Option<PluginType>` - `DaemonTool` 或 `ClientTool`

**用途:** 智能体循环中决定工具在守护进程内执行还是转发到客户端

### `PluginManager::tool_sandboxed()`
查询工具是否应启用沙箱。

**返回:** `Option<bool>`

**用途:** 构建 `ChatToolCall` 消息时传递给客户端

### `PluginManager::watch_overrides()`
监视 `tool.override.json` 文件变更并自动热重载。

**实现:**
- Linux：使用 `nix::sys::inotify` 监视 `IN_CREATE | IN_CLOSE_WRITE | IN_MOVED_TO` 事件
- 非 Linux：每 5 秒轮询检查文件修改时间

### `DaemonServer::new()`
创建新的守护进程服务器实例。

**参数:**
- `session_mgr`: `Arc<SessionManager>` - 会话管理器
- `llm_backend`: `Option<Arc<dyn LlmBackend>>` - LLM后端
- `task_mgr`: `Arc<Mutex<TaskManager>>` - 定时任务管理器
- `conv_mgr`: `Arc<ConversationManager>` - 对话管理器
- `plugin_mgr`: `Arc<PluginManager>` - 插件管理器
- `chat_model_name`: `Option<String>` - 聊天后端模型名称（用于 `ChatReady` 的 ghost hint）

**返回:** `DaemonServer` 实例

**用途:** 初始化守护进程服务器

### `DaemonServer::run()`
启动守护进程服务器并开始监听客户端连接。

**参数:**
- `addr`: `&str` - 监听地址（Unix socket路径）
- `auth_token`: `String` - 认证令牌
- `tls_acceptor`: `Option<TlsAcceptor>` - 可选 TLS 接受器

**返回:** `Result<()>`

**用途:** 启动RPC服务器并处理客户端请求。同时启动后台任务定期清理超过 120 秒的过期 `pending_agent_loops` 条目

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
- `pending_loops`: `&Arc<Mutex<HashMap<String, AgentLoopState>>>` - 暂停态循环映射

**返回:** `Vec<Message>`（响应消息列表，支持多消息流式响应）

**用途:** 分发处理不同类型的客户端消息，包括：
- `SessionStart/SessionEnd/SessionUpdate` - 会话生命周期
- `IoData` - I/O 数据记录
- `CommandComplete` - 命令完成 + KV cache 预热
- `Request` - LLM 查询或内部命令
- `CompletionRequest/CompletionSummary` - 补全请求和结果汇总
- `ChatStart/ChatMessage/ChatInterrupt` - 多轮聊天（支持工具使用）
- `ChatToolResult` - 客户端侧工具执行结果

**注意:** 返回值从单个 `Message` 改为 `Vec<Message>`，以支持智能体循环中的流式状态消息

### `handle_chat_message()`
处理聊天消息请求，初始化智能体循环。

**参数:**
- `cm`: `ChatMessage` - 聊天消息请求
- `mgr`: `&SessionManager` - 会话管理器
- `llm`: `&Option<Arc<dyn LlmBackend>>` - LLM后端
- `conv_mgr`: `&Arc<ConversationManager>` - 对话管理器
- `plugin_mgr`: `&Arc<PluginManager>` - 插件管理器
- `pending_loops`: `&Arc<Mutex<HashMap<String, AgentLoopState>>>` - 暂停态循环映射

**返回:** `Vec<Message>` - 包含 ChatToolStatus、ChatToolCall 和/或 ChatResponse 的消息列表

**用途:** 构建初始 `AgentLoopState` 并调用 `run_agent_loop()`

### `run_agent_loop()`
智能体循环核心逻辑，被 `handle_chat_message()`（初始启动）和 `handle_tool_result()`（恢复）共同调用。

**流程:**
1. 使用 `state.effective_backend` 调用 LLM（保留线程级模型覆盖，避免恢复后退回默认后端）
2. 保留完整 assistant 消息块（包括 `ContentBlock::Thinking`），以正确顺序序列化为 JSON（thinking → text → tool_use）
3. 检查响应中的 `tool_use` 块
4. 区分 DaemonTool 和 ClientTool：
   - DaemonTool 直接执行，执行前后各发送一条 `ChatToolStatus`
   - 有 ClientTool 时暂停循环，将 state 存入 `pending_agent_loops`，返回 `ChatToolCall` 消息
5. 全部为 DaemonTool 时继续循环
6. 无工具调用时存储消息并返回最终响应（含 thinking 块时以内容数组存储，否则以纯字符串存储）
7. 达到最大迭代次数时返回错误提示

### `handle_tool_result()`
处理客户端返回的工具执行结果，恢复暂停的智能体循环。

**参数:**
- `tr`: `ChatToolResult` - 工具执行结果

**流程:**
1. 从 `pending_agent_loops` 中查找对应的暂停态
2. 检查超时（600 秒）
3. 累积结果到 `completed_results`
4. **立即**通过 Formatter 格式化当前工具结果，生成增量 `ChatToolStatus` 更新消息（并行执行场景下每个工具完成时立即通知客户端，不等待其他工具）
5. 若仍有工具未完成，直接返回增量状态消息，继续等待
6. 所有工具完成后调用 `run_agent_loop()` 恢复循环，将增量消息前置于后续消息列表

### `build_chat_setup()`
构建聊天所需的共享状态（工具列表和系统提示词），被 `handle_chat_message()` 和 `/template chat` 共同使用。

**返回:** `ChatSetup { command_query_tool, tools, system_prompt }`

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

### 全局代理支持

守护进程支持通过 `daemon.toml` 配置出站请求的 HTTP/HTTPS 代理：

```toml
proxy = "http://proxy.example.com:8080"
no_proxy = "localhost,127.0.0.1"
```

代理设置通过 `DaemonOpts` 结构传递，在执行 DaemonTool 子进程时注入为环境变量（`HTTP_PROXY`、`HTTPS_PROXY`、`NO_PROXY` 及小写变体）。LLM 后端 HTTP 客户端同样尊重这些环境变量（通过 `reqwest` 的代理支持）。

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
  1. 构建 ChatSetup（CommandQueryTool + 插件工具 + 系统提示词）
  2. 附加 system-reminder（时间/cwd/Git/平台/最近5条命令）
  3. 加载对话历史 + 工具定义
  4. 调用 LLM（启用 thinking，传递工具定义）
  5. DaemonTool 直接执行 / ClientTool 转发 ChatToolCall
  6. 等待 ChatToolResult（所有工具完成后恢复循环）
  7. 循环直到获得最终文本响应（最多 100 次 / 600 秒超时）
  8. 存储所有消息（去除 system-reminder）→ 返回 ChatResponse
工具结果 → 发送ChatToolResult → 守护进程累积结果 → 全部完成时恢复智能体循环
聊天中断 → 发送ChatInterrupt → 守护进程存储部分结果 + 清理暂停态
客户端断开 → 发送SessionEnd → 守护进程标记会话结束 + flush pending sample
```

## 依赖关系

### 内部依赖
- `omnish-common`: 配置加载
- `omnish-protocol`: 消息协议定义（含 `ChatToolCall`/`ChatToolResult` 消息类型）
- `omnish-transport`: RPC传输层
- `omnish-store`: 会话和命令存储、补全采样存储
- `omnish-context`: 上下文构建
- `omnish-llm`: LLM后端集成、PromptManager、工具定义
- `omnish-plugin`: 工具实现（bash/read/edit/write/glob/grep）、Landlock 沙箱

### 外部依赖
- `tokio`: 异步运行时
- `anyhow`: 错误处理
- `tracing`: 结构化日志
- `serde`: 序列化/反序列化
- `chrono`: 时间处理
- `tokio-cron-scheduler`: 定时任务调度
- `uuid`: 对话线程ID生成
- `nix`: inotify 文件监视（Linux）
- `landlock`: 文件系统沙箱（通过 `omnish-plugin`，Linux）

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
│   ├── a1b2c3d4-...-uuid1.jsonl        # 对话线程（每行一个原始 JSON 消息）
│   ├── a1b2c3d4-...-uuid1.meta.json    # 线程元数据（host/cwd/summary/model）
│   └── e5f6g7h8-...-uuid2.jsonl
├── plugins/
│   ├── builtin/
│   │   ├── tool.json               # 内置工具定义（每次启动覆盖）
│   │   ├── tool.override.json      # 用户自定义工具描述覆盖（可选）
│   │   └── tool.override.json.example
│   └── <external_plugin>/
│       ├── tool.json               # 外部插件工具定义
│       └── tool.override.json      # 外部插件描述覆盖（可选）
├── prompts/
│   ├── chat.json                   # 聊天系统提示词（每次启动覆盖）
│   ├── chat.override.json          # 用户自定义聊天提示词覆盖（可选）
│   └── chat.override.json.example
├── logs/
│   ├── completions/           # 补全记录（JSONL）
│   ├── sessions/              # 会话更新记录（JSONL）
│   ├── samples/               # 补全采样数据（JSONL）
│   └── daemon.log.YYYY-MM-DD  # 每日轮转的守护进程日志
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
- `*.jsonl`（threads/）: 每行一个原始 JSON 消息，保留完整的 LLM API 格式（包括 tool_use、tool_result、thinking 等复杂内容块）。`<system-reminder>` 标签在存储前被过滤。
- `*.meta.json`（threads/）: 线程元数据（ThreadMeta），含 host/cwd/summary/summary_rounds/model 字段。
- `*.jsonl`（logs/samples/）: 每行一个 `CompletionSample` JSON对象
- `tool.json`: 插件工具定义文件，包含 `plugin_type` 和工具列表
- `tool.override.json`: 用户自定义工具描述覆盖
- `chat.json`: 聊天系统提示词片段数组
- `chat.override.json`: 用户自定义聊天提示词覆盖

## 并发与锁设计

### RwLock 分层
守护进程使用 `tokio::sync::RwLock` 替代 `Mutex` 管理会话状态，允许多个客户端并行读取会话数据：

- `sessions: RwLock<HashMap<...>>` - 顶层会话映射表，大多数操作仅需读锁
- `Session.meta: RwLock<SessionMeta>` - 会话元数据，读多写少
- `Session.commands: RwLock<Vec<CommandRecord>>` - 命令列表，读多写少
- `Session.stream_writer: Mutex<StreamWriterState>` - 流写入器，独占写入
- `Session.pending_sample: Mutex<Option<PendingSample>>` - 采样状态，短暂持有
- `PluginManager.prompt_cache: std::sync::RwLock<PromptCache>` - 工具描述缓存，读多写少
- `pending_agent_loops: tokio::sync::Mutex<HashMap<...>>` - 暂停态循环映射，短暂持有
- `sandbox_rules: Arc<RwLock<HashMap<String, Vec<PermitRule>>>>` - 沙箱许可规则，热重载时原子替换

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
5. **插件加载错误**: 格式错误的 `tool.json` 跳过并记录警告，不影响其他插件
6. **工具名冲突**: 重复的工具名跳过并记录警告

## 性能考虑

1. **内存使用**: 活跃会话数据常驻内存，历史会话按需加载
2. **并发控制**: 使用`tokio::sync::RwLock`保护会话状态，允许并发读取
3. **I/O优化**: 流数据使用二进制格式，批量写入
4. **上下文构建**: 按需构建上下文，避免不必要的计算
5. **对话缓存**: 对话线程启动时全量加载到内存，后续读取零磁盘I/O
6. **日志抑制**: 过滤 rustls 的 debug 日志（`rustls=off` 指令），防止日志洪泛
7. **工作线程**: 上限 30 个 tokio 工作线程（`available_parallelism().min(30)`）
8. **工具索引**: `PluginManager` 使用 `HashMap<String, (usize, usize)>` 实现 O(1) 工具查找

## 内部命令

守护进程支持`__cmd:`前缀的内部命令请求，所有命令响应以 JSON 格式返回（包含 `"display"` 字段用于终端展示，部分命令附加结构化数据字段）：

- `__cmd:context [template]` — 获取LLM上下文（支持`completion`、`chat`、`daily-notes`、`hourly-notes`等模板名）
- `__cmd:context chat` — 显示当前的 system-reminder（时间/cwd/命令等环境信息）
- `__cmd:context chat:<thread_id>` — 获取指定聊天线程的对话上下文 + system-reminder
- `__cmd:template [name]` — 查看实际的 LLM 提示词模板（通过 `build_chat_setup()` 构建，显示实际的工具定义和插件工具）
  - 支持参数：`chat`、`auto-complete`、`daily-notes`、`hourly-notes`
  - 聊天模板包含实际注册的工具定义（来自 `PluginManager`）
- `__cmd:sessions` — 列出所有活跃会话
- `__cmd:session` — 显示当前会话调试信息
- `__cmd:daemon` — 显示守护进程版本号及当前定时任务列表（等同于 `/debug daemon`）
- `__cmd:conversations` — 列出所有聊天对话（含 `thread_ids` 数组），按修改时间降序排列，显示相对时间（如 "12s ago"、"1h ago"）、交换次数、最后问题
- `__cmd:resume` — 恢复最近的对话（等同于 `__cmd:resume 1`），返回结构化历史（`history` 数组含 `user_input`、`llm_text`、`tool_status`、`response`、`separator` 类型条目）及 `thread_id`
- `__cmd:resume N` — 按索引恢复指定对话（1-based），返回结构化历史及 `thread_id`
- `__cmd:resume_tid <thread_id>` — 按线程 ID 恢复对话（跨删除操作稳定），返回结构化历史
- `__cmd:conversations del <thread_id>` — 按线程 ID 删除对话，返回 `deleted_thread_id`
- `__cmd:models [thread_id]` — 列出所有可用后端（含 `name`、`model`、`selected` 字段），可选传入线程 ID 以显示该线程的当前模型选择
- `__cmd:tasks [disable <name>]` — 查看或管理定时任务
- `__cmd:debug commands [N]` — 显示最近 N 条（默认 30）shell 命令历史（完整格式，含参数）
- `__cmd:debug command <seq>` — 显示指定序号命令的完整详情和输出（通过 `CommandQueryTool::get_command_detail(seq)` 获取）

这些命令由客户端的`/`命令转发，通过`handle_builtin_command()`函数处理。
