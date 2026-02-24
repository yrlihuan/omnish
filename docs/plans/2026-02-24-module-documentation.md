# 模块文档编写实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**目标:** 为omnish项目的每个模块的重要函数编写说明文档，放在docs/implementation目录下

**架构:** 为11个crate的每个重要模块创建独立的Markdown文档，包含模块概述、重要函数说明、使用示例和代码示例

**技术栈:** Rust, Markdown, Git

---

## 任务概览

本项目包含11个crate，需要为每个crate的重要模块编写文档：

1. **omnish-common** - 共享配置和工具函数
2. **omnish-protocol** - 通信协议定义
3. **omnish-transport** - RPC传输层
4. **omnish-pty** - PTY处理
5. **omnish-store** - 数据存储
6. **omnish-context** - 上下文构建
7. **omnish-llm** - LLM后端抽象
8. **omnish-tracker** - 命令跟踪
9. **omnish-daemon** - 守护进程
10. **omnish-client** - 客户端
11. **omnish-transport** (已覆盖)

---

### Task 1: 准备文档目录结构

**文件:**
- 创建: `docs/implementation/README.md`
- 创建: `docs/implementation/omnish-common.md`
- 创建: `docs/implementation/omnish-protocol.md`
- 创建: `docs/implementation/omnish-transport.md`
- 创建: `docs/implementation/omnish-pty.md`
- 创建: `docs/implementation/omnish-store.md`
- 创建: `docs/implementation/omnish-context.md`
- 创建: `docs/implementation/omnish-llm.md`
- 创建: `docs/implementation/omnish-tracker.md`
- 创建: `docs/implementation/omnish-daemon.md`
- 创建: `docs/implementation/omnish-client.md`

**步骤1: 创建README文件**

```markdown
# omnish 模块文档

本目录包含omnish项目各模块的详细说明文档。

## 模块列表

1. [omnish-common](./omnish-common.md) - 共享配置和工具函数
2. [omnish-protocol](./omnish-protocol.md) - 通信协议定义
3. [omnish-transport](./omnish-transport.md) - RPC传输层
4. [omnish-pty](./omnish-pty.md) - PTY处理
5. [omnish-store](./omnish-store.md) - 数据存储
6. [omnish-context](./omnish-context.md) - 上下文构建
7. [omnish-llm](./omnish-llm.md) - LLM后端抽象
8. [omnish-tracker](./omnish-tracker.md) - 命令跟踪
9. [omnish-daemon](./omnish-daemon.md) - 守护进程
10. [omnish-client](./omnish-client.md) - 客户端

## 文档结构

每个模块文档包含以下部分：
- 模块概述
- 重要数据结构
- 关键函数说明
- 使用示例
- 依赖关系
```

**步骤2: 创建空文档文件**

```bash
touch docs/implementation/omnish-common.md
touch docs/implementation/omnish-protocol.md
touch docs/implementation/omnish-transport.md
touch docs/implementation/omnish-pty.md
touch docs/implementation/omnish-store.md
touch docs/implementation/omnish-context.md
touch docs/implementation/omnish-llm.md
touch docs/implementation/omnish-tracker.md
touch docs/implementation/omnish-daemon.md
touch docs/implementation/omnish-client.md
```

**步骤3: 提交初始文件**

```bash
git add docs/implementation/README.md
git add docs/implementation/*.md
git commit -m "docs: create module documentation directory structure"
```

---

### Task 2: 编写 omnish-common 模块文档

**文件:**
- 修改: `docs/implementation/omnish-common.md`
- 读取: `crates/omnish-common/src/lib.rs`
- 读取: `crates/omnish-common/src/config.rs`

**步骤1: 分析模块代码**

```bash
cat crates/omnish-common/src/lib.rs
cat crates/omnish-common/src/config.rs
```

**步骤2: 编写模块文档**

```markdown
# omnish-common 模块

**功能:** 共享配置和工具函数

## 模块概述

omnish-common 包含客户端和守护进程共享的配置结构和工具函数。

## 重要数据结构

### `ClientConfig`
客户端配置结构，包含：
- `shell_command`: 要执行的shell命令
- `command_prefix`: 触发LLM查询的命令前缀（默认"::"）
- `daemon_addr`: 守护进程地址

### `DaemonConfig`
守护进程配置结构，包含：
- `listen_addr`: 监听地址
- `llm_config`: LLM后端配置
- `auto_trigger`: 自动触发配置

## 关键函数说明

### `load_client_config()`
从配置文件或环境变量加载客户端配置。

**参数:** 无
**返回:** `Result<ClientConfig>`
**用途:** 初始化客户端配置

### `load_daemon_config()`
从配置文件或环境变量加载守护进程配置。

**参数:** 无
**返回:** `Result<DaemonConfig>`
**用途:** 初始化守护进程配置

## 使用示例

```rust
use omnish_common::config;

let client_config = config::load_client_config()?;
println!("Using shell: {}", client_config.shell_command);
```

## 依赖关系
- serde: 序列化/反序列化
- toml: TOML配置文件解析
- anyhow: 错误处理
- dirs: 获取标准目录路径
```

**步骤3: 提交文档**

```bash
git add docs/implementation/omnish-common.md
git commit -m "docs: add omnish-common module documentation"
```

---

### Task 3: 编写 omnish-protocol 模块文档

**文件:**
- 修改: `docs/implementation/omnish-protocol.md`
- 读取: `crates/omnish-protocol/src/lib.rs`
- 读取: `crates/omnish-protocol/src/message.rs`

**步骤1: 分析模块代码**

```bash
cat crates/omnish-protocol/src/lib.rs
cat crates/omnish-protocol/src/message.rs
```

**步骤2: 编写模块文档**

```markdown
# omnish-protocol 模块

**功能:** 定义客户端和守护进程之间的通信协议

## 模块概述

omnish-protocol 定义了客户端和守护进程之间交换的消息类型，使用bincode进行二进制序列化。

## 重要数据结构

### `Message` 枚举
主要消息类型：
- `SessionStart`: 新会话开始
- `IoData`: 终端I/O数据
- `Event`: 事件通知
- `Request`: LLM请求
- `Response`: LLM响应
- `CommandComplete`: 命令完成通知

### `SessionStart`
会话开始消息，包含：
- `session_id`: 会话标识符
- `shell_command`: shell命令
- `env_vars`: 环境变量

### `IoData`
I/O数据消息，包含：
- `direction`: 输入或输出
- `data`: 原始字节数据
- `timestamp`: 时间戳

## 关键函数说明

### `Message::serialize()`
序列化消息为字节向量。

**参数:** 无
**返回:** `Vec<u8>`
**用途:** 准备网络传输

### `Message::deserialize()`
从字节向量反序列化消息。

**参数:** `data: &[u8]`
**返回:** `Result<Message>`
**用途:** 接收网络数据

## 使用示例

```rust
use omnish_protocol::Message;

let msg = Message::SessionStart(session_start);
let bytes = msg.serialize();
let restored = Message::deserialize(&bytes)?;
```

## 依赖关系
- serde: 序列化
- bincode: 二进制序列化
- omnish-store: 命令记录类型
```

**步骤3: 提交文档**

```bash
git add docs/implementation/omnish-protocol.md
git commit -m "docs: add omnish-protocol module documentation"
```

---

### Task 4: 编写 omnish-transport 模块文档

**文件:**
- 修改: `docs/implementation/omnish-transport.md`
- 读取: `crates/omnish-transport/src/lib.rs`
- 读取: `crates/omnish-transport/src/rpc_client.rs`
- 读取: `crates/omnish-transport/src/rpc_server.rs`

**步骤1: 分析模块代码**

```bash
cat crates/omnish-transport/src/lib.rs
cat crates/omnish-transport/src/rpc_client.rs
cat crates/omnish-transport/src/rpc_server.rs
```

**步骤2: 编写模块文档**

```markdown
# omnish-transport 模块

**功能:** RPC传输层，处理Unix socket和TCP连接

## 模块概述

omnish-transport 提供客户端和守护进程之间的RPC通信层，支持Unix socket和TCP协议。

## 重要数据结构

### `TransportAddr` 枚举
传输地址类型：
- `Unix(String)`: Unix socket路径
- `Tcp(String)`: TCP地址（主机:端口）

### `RpcClient`
RPC客户端，负责：
- 连接到守护进程
- 发送消息
- 接收响应

### `RpcServer`
RPC服务器，负责：
- 监听连接
- 处理客户端请求
- 发送响应

## 关键函数说明

### `parse_addr()`
解析地址字符串为TransportAddr。

**参数:** `addr: &str`
**返回:** `TransportAddr`
**用途:** 解析配置中的地址

### `RpcClient::connect()`
连接到RPC服务器。

**参数:** `addr: TransportAddr`
**返回:** `Result<RpcClient>`
**用途:** 建立客户端连接

### `RpcClient::send()`
发送消息到服务器。

**参数:** `msg: Message`
**返回:** `Result<Message>`
**用途:** 发送请求并等待响应

### `RpcServer::bind()`
绑定到地址并开始监听。

**参数:** `addr: TransportAddr`
**返回:** `Result<RpcServer>`
**用途:** 启动服务器

## 使用示例

```rust
use omnish_transport::{RpcClient, parse_addr};

let addr = parse_addr("/tmp/omnish.sock");
let client = RpcClient::connect(addr).await?;
let response = client.send(message).await?;
```

## 依赖关系
- omnish-protocol: 消息类型
- tokio: 异步运行时
- anyhow: 错误处理
- tracing: 日志记录
```

**步骤3: 提交文档**

```bash
git add docs/implementation/omnish-transport.md
git commit -m "docs: add omnish-transport module documentation"
```

---

### Task 5: 编写 omnish-pty 模块文档

**文件:**
- 修改: `docs/implementation/omnish-pty.md`
- 读取: `crates/omnish-pty/src/lib.rs`
- 读取: `crates/omnish-pty/src/proxy.rs`
- 读取: `crates/omnish-pty/src/raw_mode.rs`

**步骤1: 分析模块代码**

```bash
cat crates/omnish-pty/src/lib.rs
cat crates/omnish-pty/src/proxy.rs
cat crates/omnish-pty/src/raw_mode.rs
```

**步骤2: 编写模块文档**

```markdown
# omnish-pty 模块

**功能:** PTY（伪终端）处理，原始模式设置

## 模块概述

omnish-pty 提供PTY代理功能，创建伪终端并管理原始模式，确保透明转发所有I/O。

## 重要数据结构

### `PtyProxy`
PTY代理，负责：
- 创建伪终端
- 转发I/O数据
- 管理子进程

### `RawModeGuard`
原始模式守卫，RAII风格：
- 进入时设置原始模式
- 退出时恢复原始模式

## 关键函数说明

### `PtyProxy::spawn()`
创建PTY并启动子进程。

**参数:** `command: &str`, `args: &[String]`
**返回:** `Result<PtyProxy>`
**用途:** 启动shell进程

### `PtyProxy::read()`
从PTY读取数据。

**参数:** 无
**返回:** `Result<Vec<u8>>`
**用途:** 读取子进程输出

### `PtyProxy::write()`
向PTY写入数据。

**参数:** `data: &[u8]`
**返回:** `Result<()>`
**用途:** 发送输入到子进程

### `RawModeGuard::new()`
创建原始模式守卫。

**参数:** `fd: i32`
**返回:** `Result<RawModeGuard>`
**用途:** 安全设置原始模式

## 使用示例

```rust
use omnish_pty::PtyProxy;

let mut proxy = PtyProxy::spawn("bash", &[])?;
proxy.write(b"ls -la\n")?;
let output = proxy.read()?;
```

## 依赖关系
- nix: Unix系统调用
- libc: C库绑定
- anyhow: 错误处理
```

**步骤3: 提交文档**

```bash
git add docs/implementation/omnish-pty.md
git commit -m "docs: add omnish-pty module documentation"
```

---

### Task 6: 编写 omnish-store 模块文档

**文件:**
- 修改: `docs/implementation/omnish-store.md`
- 读取: `crates/omnish-store/src/lib.rs`
- 读取: `crates/omnish-store/src/command.rs`
- 读取: `crates/omnish-store/src/session.rs`
- 读取: `crates/omnish-store/src/stream.rs`

**步骤1: 分析模块代码**

```bash
cat crates/omnish-store/src/lib.rs
cat crates/omnish-store/src/command.rs
cat crates/omnish-store/src/session.rs
cat crates/omnish-store/src/stream.rs
```

**步骤2: 编写模块文档**

```markdown
# omnish-store 模块

**功能:** 数据存储，命令记录和流存储

## 模块概述

omnish-store 提供数据持久化功能，包括命令记录、会话管理和原始流存储。

## 重要数据结构

### `CommandRecord`
命令记录，包含：
- `id`: 命令ID
- `session_id`: 会话ID
- `command_line`: 命令行
- `cwd`: 当前工作目录
- `timestamp`: 时间戳
- `output_summary`: 输出摘要

### `Session`
会话管理，包含：
- `id`: 会话ID
- `start_time`: 开始时间
- `shell_command`: shell命令
- `env_vars`: 环境变量

### `StreamStore`
流存储，负责：
- 存储原始I/O数据
- 支持按偏移量读取
- 高效写入

## 关键函数说明

### `CommandRecord::save()`
保存命令记录到文件。

**参数:** `path: &Path`
**返回:** `Result<()>`
**用途:** 持久化命令数据

### `CommandRecord::load_all()`
从文件加载所有命令记录。

**参数:** `path: &Path`
**返回:** `Result<Vec<CommandRecord>>`
**用途:** 读取历史命令

### `Session::create()`
创建新会话。

**参数:** `shell_command: String`, `env_vars: Vec<(String, String)>`
**返回:** `Session`
**用途:** 初始化会话

### `StreamStore::append()`
追加数据到流存储。

**参数:** `data: &[u8]`, `timestamp: u64`
**返回:** `Result<u64>` (偏移量)
**用途:** 存储原始I/O

### `StreamStore::read_from()`
从指定偏移量读取数据。

**参数:** `offset: u64`
**返回:** `Result<Vec<u8>>`
**用途:** 检索存储的数据

## 使用示例

```rust
use omnish_store::{CommandRecord, Session};

let session = Session::create("bash".to_string(), vec![]);
let record = CommandRecord::new(session.id, "ls -la".to_string());
record.save("/path/to/store")?;
```

## 依赖关系
- serde: 序列化
- serde_json: JSON序列化
- anyhow: 错误处理
```

**步骤3: 提交文档**

```bash
git add docs/implementation/omnish-store.md
git commit -m "docs: add omnish-store module documentation"
```

---

### Task 7: 编写 omnish-context 模块文档

**文件:**
- 修改: `docs/implementation/omnish-context.md`
- 读取: `crates/omnish-context/src/lib.rs`
- 读取: `crates/omnish-context/src/format_utils.rs`
- 读取: `crates/omnish-context/src/recent.rs`

**步骤1: 分析模块代码**

```bash
cat crates/omnish-context/src/lib.rs
cat crates/omnish-context/src/format_utils.rs
cat crates/omnish-context/src/recent.rs
```

**步骤2: 编写模块文档**

```markdown
# omnish-context 模块

**功能:** 上下文构建，命令选择和格式化

## 模块概述

omnish-context 负责构建LLM查询的上下文，选择相关命令并格式化输出。

## 重要数据结构

### `ContextStrategy` trait
上下文策略接口：
- `select_commands()`: 选择相关命令
- `build_context()`: 构建上下文文本

### `RecentContextStrategy`
最近命令策略：
- 选择最近N个命令
- 按时间排序

### `ContextFormatter`
上下文格式化器：
- 格式化命令记录为文本
- 添加元数据信息

## 关键函数说明

### `build_context()`
构建LLM查询上下文。

**参数:** `strategy: &dyn ContextStrategy`, `records: &[CommandRecord]`
**返回:** `String`
**用途:** 准备LLM提示

### `RecentContextStrategy::new()`
创建最近命令策略。

**参数:** `limit: usize`
**返回:** `RecentContextStrategy`
**用途:** 限制上下文大小

### `format_command()`
格式化单个命令记录。

**参数:** `record: &CommandRecord`
**返回:** `String`
**用途:** 可读的命令表示

## 使用示例

```rust
use omnish_context::{build_context, RecentContextStrategy};

let strategy = RecentContextStrategy::new(10);
let context = build_context(&strategy, &records);
```

## 依赖关系
- omnish-store: 命令记录类型
- async-trait: 异步trait支持
- anyhow: 错误处理
```

**步骤3: 提交文档**

```bash
git add docs/implementation/omnish-context.md
git commit -m "docs: add omnish-context module documentation"
```

---

### Task 8: 编写 omnish-llm 模块文档

**文件:**
- 修改: `docs/implementation/omnish-llm.md`
- 读取: `crates/omnish-llm/src/lib.rs`
- 读取: `crates/omnish-llm/src/backend.rs`
- 读取: `crates/omnish-llm/src/anthropic.rs`
- 读取: `crates/omnish-llm/src/openai_compat.rs`
- 读取: `crates/omnish-llm/src/factory.rs`
- 读取: `crates/omnish-llm/src/template.rs`

**步骤1: 分析模块代码**

```bash
cat crates/omnish-llm/src/lib.rs
cat crates/omnish-llm/src/backend.rs
cat crates/omnish-llm/src/anthropic.rs
cat crates/omnish-llm/src/openai_compat.rs
cat crates/omnish-llm/src/factory.rs
cat crates/omnish-llm/src/template.rs
```

**步骤2: 编写模块文档**

```markdown
# omnish-llm 模块

**功能:** LLM后端抽象和实现

## 模块概述

omnish-llm 提供LLM后端抽象，支持多种LLM提供商，包括Anthropic、OpenAI兼容API和本地模型。

## 重要数据结构

### `LlmBackend` trait
LLM后端接口：
- `complete()`: 发送补全请求
- `stream_complete()`: 流式补全

### `AnthropicBackend`
Anthropic API后端：
- 支持Claude模型
- 实现`LlmBackend` trait

### `OpenAiCompatBackend`
OpenAI兼容API后端：
- 支持OpenAI、Azure、本地模型
- 实现`LlmBackend` trait

### `LlmFactory`
LLM工厂：
- 根据配置创建后端实例
- 管理后端生命周期

## 关键函数说明

### `LlmBackend::complete()`
发送LLM补全请求。

**参数:** `prompt: &str`, `context: Option<&str>`
**返回:** `Result<String>`
**用途:** 获取LLM响应

### `create_backend()`
根据配置创建LLM后端。

**参数:** `config: &LlmConfig`
**返回:** `Result<Box<dyn LlmBackend>>`
**用途:** 初始化LLM连接

### `build_prompt()`
构建LLM提示。

**参数:** `template: &str`, `context: &str`, `query: &str`
**返回:** `String`
**用途:** 格式化提示模板

## 使用示例

```rust
use omnish_llm::{create_backend, LlmBackend};

let backend = create_backend(&config).await?;
let response = backend.complete("Why did this fail?", Some(context)).await?;
```

## 依赖关系
- omnish-common: 配置类型
- reqwest: HTTP客户端
- serde_json: JSON处理
- async-trait: 异步trait支持
```

**步骤3: 提交文档**

```bash
git add docs/implementation/omnish-llm.md
git commit -m "docs: add omnish-llm module documentation"
```

---

### Task 9: 编写 omnish-tracker 模块文档

**文件:**
- 修改: `docs/implementation/omnish-tracker.md`
- 读取: `crates/omnish-tracker/src/lib.rs`
- 读取: `crates/omnish-tracker/src/command_tracker.rs`
- 读取: `crates/omnish-tracker/src/osc133_detector.rs`
- 读取: `crates/omnish-tracker/src/prompt_detector.rs`

**步骤1: 分析模块代码**

```bash
cat crates/omnish-tracker/src/lib.rs
cat crates/omnish-tracker/src/command_tracker.rs
cat crates/omnish-tracker/src/osc133_detector.rs
cat crates/omnish-tracker/src/prompt_detector.rs
```

**步骤2: 编写模块文档**

```markdown
# omnish-tracker 模块

**功能:** 命令跟踪，shell提示检测，OSC 133检测

## 模块概述

omnish-tracker 负责检测shell提示，跟踪命令边界，并处理OSC 133序列以实现可靠的命令分割。

## 重要数据结构

### `CommandTracker`
命令跟踪器：
- 跟踪当前命令状态
- 检测命令开始和结束
- 积累命令输出

### `Osc133Detector`
OSC 133序列检测器：
- 检测OSC 133;A (提示开始)
- 检测OSC 133;B (命令开始)
- 检测OSC 133;C (命令结束)

### `PromptDetector`
shell提示检测器：
- 检测常见shell提示模式
- 支持自定义提示模式

## 关键函数说明

### `CommandTracker::process_data()`
处理I/O数据，检测命令边界。

**参数:** `data: &[u8]`, `direction: IoDirection`
**返回:** `Vec<CommandEvent>`
**用途:** 实时命令跟踪

### `Osc133Detector::feed()`
输入数据到OSC 133检测器。

**参数:** `data: &[u8]`
**返回:** `Option<Osc133Event>`
**用途:** 检测OSC 133序列

### `PromptDetector::detect()`
检测shell提示。

**参数:** `data: &[u8]`
**返回:** `bool`
**用途:** 识别提示位置

## 使用示例

```rust
use omnish_tracker::CommandTracker;

let mut tracker = CommandTracker::new();
let events = tracker.process_data(b"$ ls -la\n", IoDirection::Output);
```

## 依赖关系
- omnish-store: 命令记录类型
- regex: 正则表达式匹配
```

**步骤3: 提交文档**

```bash
git add docs/implementation/omnish-tracker.md
git commit -m "docs: add omnish-tracker module documentation"
```

---

### Task 10: 编写 omnish-daemon 模块文档

**文件:**
- 修改: `docs/implementation/omnish-daemon.md`
- 读取: `crates/omnish-daemon/src/main.rs`
- 读取: `crates/omnish-daemon/src/server.rs`
- 读取: `crates/omnish-daemon/src/session_mgr.rs`
- 读取: `crates/omnish-daemon/src/event_detector.rs`

**步骤1: 分析模块代码**

```bash
cat crates/omnish-daemon/src/main.rs
cat crates/omnish-daemon/src/server.rs
cat crates/omnish-daemon/src/session_mgr.rs
cat crates/omnish-daemon/src/event_detector.rs
```

**步骤2: 编写模块文档**

```markdown
# omnish-daemon 模块

**功能:** 守护进程主程序，会话管理，LLM引擎

## 模块概述

omnish-daemon 是守护进程主模块，负责管理多个客户端会话，处理LLM查询，并存储会话数据。

## 重要数据结构

### `SessionManager`
会话管理器：
- 管理活跃会话
- 处理会话生命周期
- 聚合跨会话数据

### `DaemonServer`
守护进程服务器：
- 监听客户端连接
- 处理RPC请求
- 分发LLM查询

### `EventDetector`
事件检测器：
- 检测非零退出码
- 检测stderr错误模式
- 触发自动LLM分析

## 关键函数说明

### `main()`
守护进程入口点。

**参数:** 命令行参数
**返回:** `Result<()>`
**用途:** 启动守护进程

### `SessionManager::new_session()`
创建新会话。

**参数:** `session_start: SessionStart`
**返回:** `Result<()>`
**用途:** 初始化客户端会话

### `SessionManager::process_io()`
处理会话I/O数据。

**参数:** `session_id: &str`, `io_data: IoData`
**返回:** `Result<()>`
**用途:** 更新会话状态

### `DaemonServer::run()`
运行守护进程服务器。

**参数:** `config: DaemonConfig`
**返回:** `Result<()>`
**用途:** 启动服务器主循环

## 使用示例

```bash
# 启动守护进程
omnish-daemon --config ~/.omnish/daemon.toml
```

## 依赖关系
- omnish-common: 配置
- omnish-protocol: 消息类型
- omnish-transport: RPC通信
- omnish-store: 数据存储
- omnish-llm: LLM后端
- tokio: 异步运行时
```

**步骤3: 提交文档**

```bash
git add docs/implementation/omnish-daemon.md
git commit -m "docs: add omnish-daemon module documentation"
```

---

### Task 11: 编写 omnish-client 模块文档

**文件:**
- 修改: `docs/implementation/omnish-client.md`
- 读取: `crates/omnish-client/src/main.rs`
- 读取: `crates/omnish-client/src/interceptor.rs`
- 读取: `crates/omnish-client/src/completion.rs`
- 读取: `crates/omnish-client/src/display.rs`

**步骤1: 分析模块代码**

```bash
cat crates/omnish-client/src/main.rs
cat crates/omnish-client/src/interceptor.rs
cat crates/omnish-client/src/completion.rs
cat crates/omnish-client/src/display.rs
```

**步骤2: 编写模块文档**

```markdown
# omnish-client 模块

**功能:** 客户端主程序，PTY代理，输入拦截，LLM补全

## 模块概述

omnish-client 是客户端主模块，作为PTY代理运行，拦截用户输入，提供LLM补全功能，并与守护进程通信。

## 重要数据结构

### `InputInterceptor`
输入拦截器：
- 检测命令前缀（如"::"）
- 过滤ESC序列
- 触发LLM查询

### `CompletionHandler`
补全处理器：
- 管理LLM补全请求
- 处理流式响应
- 显示补全结果

### `Display`
显示处理器：
- 格式化LLM响应
- 处理输出节流
- 管理显示状态

## 关键函数说明

### `main()`
客户端入口点。

**参数:** 命令行参数
**返回:** `Result<()>`
**用途:** 启动客户端

### `InputInterceptor::process_input()`
处理用户输入。

**参数:** `input: &[u8]`
**返回:** `InterceptResult`
**用途:** 检测LLM查询触发

### `CompletionHandler::request_completion()`
请求LLM补全。

**参数:** `query: &str`, `context: Option<&str>`
**返回:** `Result<String>`
**用途:** 获取LLM建议

### `run_pty_loop()`
PTY主循环。

**参数:** `config: ClientConfig`
**返回:** `Result<()>`
**用途:** 运行PTY代理

## 使用示例

```bash
# 启动客户端
omnish-client --shell bash
# 在shell中使用LLM查询
::ask why did make fail
```

## 依赖关系
- omnish-common: 配置
- omnish-protocol: 消息类型
- omnish-transport: RPC通信
- omnish-pty: PTY处理
- omnish-tracker: 命令跟踪
- tokio: 异步运行时
```

**步骤3: 提交文档**

```bash
git add docs/implementation/omnish-client.md
git commit -m "docs: add omnish-client module documentation"
```

---

### Task 12: 验证和最终提交

**步骤1: 验证所有文档格式**

```bash
markdownlint docs/implementation/*.md || echo "Markdown linting not available"
```

**步骤2: 检查文档完整性**

```bash
wc -l docs/implementation/*.md
```

**步骤3: 最终提交**

```bash
git add docs/implementation/
git commit -m "docs: complete module documentation for all 11 crates"
```

**步骤4: 创建文档索引**

更新README.md以包含完整的模块列表和简要描述。

---

## 执行选项

计划完成并保存到 `docs/plans/2026-02-24-module-documentation.md`。两个执行选项：

**1. 子代理驱动（本次会话）** - 我为每个任务分派新的子代理，在任务之间进行代码审查，快速迭代

**2. 并行会话（独立）** - 在新工作树中打开新会话，使用executing-plans进行批量执行和检查点

**哪种方法？**