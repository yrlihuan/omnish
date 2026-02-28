# omnish-daemon 模块

**功能:** 守护进程服务，管理会话、处理客户端请求、集成LLM后端

## 模块概述

omnish-daemon 是omnish系统的核心守护进程，负责：
1. 管理终端会话的生命周期（创建、结束、持久化）
2. 接收并处理来自客户端的I/O数据流
3. 存储命令历史记录和终端输出
4. 集成LLM后端处理用户查询和自动补全请求
5. 提供RPC服务接口供客户端调用

守护进程以Unix domain socket方式运行，支持多个客户端同时连接。

## 重要数据结构

### `DaemonServer`
守护进程服务器主结构，包含：
- `session_mgr`: `Arc<SessionManager>` - 会话管理器实例
- `llm_backend`: `Option<Arc<dyn LlmBackend>>` - 可选的LLM后端

### `SessionManager`
会话管理器，负责管理所有活跃会话，包含：
- `base_dir`: `PathBuf` - 会话数据存储的基础目录
- `sessions`: `Mutex<HashMap<String, ActiveSession>>` - 活跃会话映射表

### `ActiveSession`
活跃会话的内部表示，包含：
- `meta`: `SessionMeta` - 会话元数据（ID、父会话ID、属性等）
- `stream_writer`: `StreamWriter` - 流数据写入器
- `commands`: `Vec<CommandRecord>` - 命令记录列表
- `dir`: `PathBuf` - 会话数据存储目录
- `last_command_stream_pos`: `u64` - 上一个命令结束时的流位置

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
**功能:** 生成过去1小时内的命令执行摘要，保存到 `~/.omnish/notes/hourly/YYYY-MM-DD-HH.md`
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
**执行周期:** 每天指定时刻 (默认 `0 0 23 * * *` - 每天23:00)
**功能:** 生成过去24小时内的命令记录和 LLM 摘要，保存到 `~/.omnish/notes/YYYY-MM-DD.md`
**相关配置:**
```toml
[tasks.daily_notes]
enabled = true
schedule_hour = 23      # 每天几点生成（0-23），默认 23:00
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

**注意:** 自动设置 `TZ` 环境变量为空字符串，使用本地时区

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

### `DaemonServer::new()`
创建新的守护进程服务器实例。

**参数:**
- `session_mgr`: `Arc<SessionManager>` - 会话管理器
- `llm_backend`: `Option<Arc<dyn LlmBackend>>` - LLM后端

**返回:** `DaemonServer` 实例

**用途:** 初始化守护进程服务器

### `DaemonServer::run()`
启动守护进程服务器并开始监听客户端连接。

**参数:**
- `addr`: `&str` - 监听地址（Unix socket路径）

**返回:** `Result<()>`

**用途:** 启动RPC服务器并处理客户端请求

### `SessionManager::new()`
创建新的会话管理器。

**参数:**
- `base_dir`: `PathBuf` - 会话数据存储的基础目录

**返回:** `SessionManager` 实例

**用途:** 初始化会话管理器，创建必要的目录结构

### `SessionManager::load_existing()`
从磁盘加载已存在的会话数据。

**参数:** 无

**返回:** `Result<usize>`（加载的会话数量）

**用途:** 守护进程启动时恢复之前的会话状态

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

**用途:** 客户端发送命令完成通知时，填充流偏移量并保存命令记录

### `SessionManager::end_session()`
结束指定会话。

**参数:**
- `session_id`: `&str` - 会话ID

**返回:** `Result<()>`

**用途:** 客户端断开连接时标记会话结束时间

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
- `mgr`: `&SessionManager` - 会话管理器
- `llm`: `&Option<Arc<dyn LlmBackend>>` - LLM后端

**返回:** `Message`（响应消息）

**用途:** 分发处理不同类型的客户端消息

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

**用途:** 为shell命令提供智能补全建议

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
sessions_dir = "~/.omnish/sessions"

[llm]
default = "claude"

[llm.backends.claude]
backend_type = "anthropic"
model = "claude-3-haiku-20240307"
api_key_cmd = "pass show api/anthropic"

[llm.auto_trigger]
on_nonzero_exit = true
on_stderr_patterns = ["error:", "fatal:", "not found"]
cooldown_seconds = 10
```

### 程序化使用示例
```rust
use omnish_daemon::session_mgr::SessionManager;
use std::sync::Arc;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 创建会话管理器
    let store_dir = PathBuf::from("~/.omnish/sessions");
    let session_mgr = Arc::new(SessionManager::new(store_dir));

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
命令执行 → 发送CommandComplete → 守护进程保存命令记录
用户查询 → 发送Request → 守护进程调用LLM后端 → 返回Response
自动补全 → 发送CompletionRequest → 守护进程生成建议 → 返回CompletionResponse
客户端断开 → 发送SessionEnd → 守护进程标记会话结束
```

## 依赖关系

### 内部依赖
- `omnish-common`: 配置加载
- `omnish-protocol`: 消息协议定义
- `omnish-transport`: RPC传输层
- `omnish-store`: 会话和命令存储
- `omnish-context`: 上下文构建
- `omnish-llm`: LLM后端集成

### 外部依赖
- `tokio`: 异步运行时
- `anyhow`: 错误处理
- `tracing`: 结构化日志
- `serde`: 序列化/反序列化
- `chrono`: 时间处理

## 数据持久化

### 会话目录结构
```
~/.omnish/sessions/
├── 2026-02-24T10-30-00Z_session-abc123/
│   ├── meta.json          # 会话元数据
│   ├── commands.json      # 命令记录
│   └── stream.bin         # 二进制I/O流数据
└── 2026-02-24T11-15-00Z_session-def456/
    ├── meta.json
    ├── commands.json
    └── stream.bin
```

### 文件格式说明
- `meta.json`: JSON格式的会话元数据（ID、时间戳、属性等）
- `commands.json`: JSON数组格式的命令记录
- `stream.bin`: 二进制格式的I/O流数据，包含时间戳、方向和原始字节

## 错误处理

守护进程采用以下错误处理策略：
1. **会话级别错误**: 单个会话的错误不会影响其他会话
2. **LLM后端错误**: LLM后端不可用时，查询返回错误信息但不崩溃
3. **存储错误**: 文件系统错误会记录警告但尝试继续运行
4. **网络错误**: 客户端连接错误自动恢复，不影响服务

## 性能考虑

1. **内存使用**: 活跃会话数据常驻内存，历史会话按需加载
2. **并发控制**: 使用`tokio::sync::Mutex`保护会话状态
3. **I/O优化**: 流数据使用二进制格式，批量写入
4. **上下文构建**: 按需构建上下文，避免不必要的计算

## 调试支持

在调试构建中，守护进程支持特殊的调试请求：
```
__debug:context  # 获取原始上下文信息
```

可以通过发送包含`__debug:`前缀的查询来获取内部状态信息。