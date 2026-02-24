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