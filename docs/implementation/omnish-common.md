# omnish-common 模块

**功能:** 共享配置和工具函数

## 模块概述

omnish-common 包含客户端和守护进程共享的配置结构和工具函数。该模块提供了统一的配置加载机制、默认值设置以及共享的数据类型定义，确保客户端和守护进程使用一致的配置。

## 重要数据结构

### `ClientConfig`
客户端配置结构，包含：
- `shell`: Shell配置（`ShellConfig`类型）
- `daemon_addr`: 守护进程地址（默认：`~/.omnish/omnish.sock`）
- `completion_enabled`: 是否启用自动补全（默认：`true`）

### `DaemonConfig`
守护进程配置结构，包含：
- `listen_addr`: 监听地址（默认：`~/.omnish/omnish.sock`）
- `llm`: LLM后端配置（`LlmConfig`类型）
- `context`: 上下文构建配置（`ContextConfig`类型）
- `tasks`: 定时任务配置（`TasksConfig`类型）

### `ShellConfig`
Shell相关配置，包含：
- `command`: 要执行的shell命令（默认：`$SHELL`环境变量或`/bin/sh`）
- `command_prefix`: 触发LLM查询的命令前缀（默认`:`）
- `intercept_gap_ms`: 命令拦截间隔毫秒数（默认：1000ms）
- `ghost_timeout_ms`: ghost-text超时毫秒数（默认：10000ms）

### `LlmConfig`
LLM配置结构，包含：
- `default`: 默认LLM后端名称（默认：`"claude"`）
- `backends`: LLM后端配置映射表（`HashMap<String, LlmBackendConfig>`）
- `auto_trigger`: 自动触发配置（`AutoTriggerConfig`类型）
- `use_cases`: UseCase到后端名的映射（`HashMap<String, String>`）

### `ContextConfig`
上下文构建配置，包含：
- `completion`: 补全上下文配置（`CompletionContextConfig`类型）
- `hourly_summary`: 小时摘要上下文配置
- `daily_summary`: 日报上下文配置

### `CompletionContextConfig`
补全上下文配置，包含：
- `max_commands`: 历史命令最大数量
- `max_chars`: 上下文最大字符数
- `max_line_width`: 行最大宽度（默认：200）
- `detailed_min` / `detailed_max`: 弹性详细窗口范围（默认：20/30）
- `max_output_chars_per_command`: 每个命令输出最大字符数（可选）

### `TasksConfig`
定时任务配置，包含：
- `eviction`: 会话淘汰配置（`inactive_hours`）
- `daily_notes`: 日报生成配置（`schedule_hour`，默认18）
- `disk_cleanup`: 磁盘清理配置（`cron`表达式）

### `LlmBackendConfig`
LLM后端具体配置，包含：
- `backend_type`: 后端类型（如`"openai"`、`"anthropic"`等）
- `model`: 模型名称
- `api_key_cmd`: 获取API密钥的命令（可选）
- `base_url`: API基础URL（可选）
- `max_content_chars`: 模型上下文最大字符数（可选）

### `AutoTriggerConfig`
自动触发LLM分析的配置，包含：
- `on_nonzero_exit`: 是否在非零退出码时触发（默认：`false`）
- `on_stderr_patterns`: 匹配stderr模式时触发的正则表达式列表
- `cooldown_seconds`: 触发冷却时间（默认：5秒）

## 关键函数说明

### `omnish_dir()`
获取omnish基础目录路径。

**参数:** 无
**返回:** `PathBuf`（`~/.omnish`，回退到`/tmp/omnish`）
**用途:** 获取配置文件和会话数据的存储目录

### `load_client_config()`
从配置文件或环境变量加载客户端配置。

**参数:** 无
**返回:** `Result<ClientConfig>`
**用途:** 初始化客户端配置
**配置文件路径:** `$OMNISH_CLIENT_CONFIG`环境变量或`~/.omnish/client.toml`

### `load_daemon_config()`
从配置文件或环境变量加载守护进程配置。

**参数:** 无
**返回:** `Result<DaemonConfig>`
**用途:** 初始化守护进程配置
**配置文件路径:** `$OMNISH_DAEMON_CONFIG`环境变量或`~/.omnish/daemon.toml`

## 使用示例

### 加载客户端配置
```rust
use omnish_common::config;

let client_config = config::load_client_config()?;
println!("Using shell: {}", client_config.shell.command);
println!("Daemon address: {}", client_config.daemon_addr);
```

### 加载守护进程配置
```rust
use omnish_common::config;

let daemon_config = config::load_daemon_config()?;
println!("Listening on: {}", daemon_config.listen_addr);
println!("Sessions directory: {}", daemon_config.sessions_dir);
```

### 配置文件示例 (client.toml)
```toml
[shell]
command = "/bin/bash"
command_prefix = ":"
intercept_gap_ms = 500

daemon_addr = "/tmp/omnish.sock"
```

### 配置文件示例 (daemon.toml)
```toml
listen_addr = "/tmp/omnish.sock"
sessions_dir = "/tmp/omnish-sessions"

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
```

## auth 模块

omnish-common 包含认证令牌管理工具函数，用于客户端和守护进程之间的身份验证。

### `default_token_path()`
获取默认认证令牌文件路径。

**参数:** 无
**返回:** `PathBuf`（`~/.omnish/auth_token`）
**用途:** 获取认证令牌的默认存储路径

### `load_or_create_token()`
加载已有令牌或生成新令牌。

**参数:** `path: &Path` - 令牌文件路径
**返回:** `Result<String>` - 64字符的hex编码令牌
**用途:** 守护进程启动时调用，确保认证令牌存在
**安全:** 文件权限设置为0600（仅所有者可读写）

### `load_token()`
从文件加载令牌。

**参数:** `path: &Path` - 令牌文件路径
**返回:** `Result<String>` - 令牌字符串
**用途:** 客户端连接时加载共享令牌
**错误:** 文件不存在或为空时返回错误

## 依赖关系
- `serde`: 序列化/反序列化配置结构
- `toml`: TOML配置文件解析
- `anyhow`: 统一的错误处理
- `dirs`: 获取标准目录路径（如用户主目录）
- `rand`: 随机令牌生成
- `hex`: 令牌hex编码

## 配置加载优先级
1. 环境变量指定的配置文件路径（`OMNISH_CLIENT_CONFIG`/`OMNISH_DAEMON_CONFIG`）
2. 默认配置文件路径（`~/.omnish/client.toml`/`~/.omnish/daemon.toml`）
3. 内置默认值（如果配置文件不存在）

## 默认值说明
- Shell命令: `$SHELL`环境变量或`/bin/sh`
- 命令前缀: `:`
- 守护进程socket路径: `~/.omnish/omnish.sock`
- 默认LLM后端: `"claude"`
- 拦截间隔: 1000ms
- Ghost-text超时: 10000ms
- 自动触发冷却时间: 5秒
- 认证令牌路径: `~/.omnish/auth_token`