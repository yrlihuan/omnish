# omnish-common 模块

**功能:** 共享配置和工具函数

## 模块概述

omnish-common 包含客户端和守护进程共享的配置结构和工具函数。该模块提供了统一的配置加载机制、默认值设置以及共享的数据类型定义，确保客户端和守护进程使用一致的配置。

## 重要数据结构

### `ClientConfig`
客户端配置结构，包含：
- `shell`: Shell配置（`ShellConfig`类型）
- `daemon_addr`: 守护进程地址（默认：`~/.omnish/omnish.sock`）

### `DaemonConfig`
守护进程配置结构，包含：
- `listen_addr`: 监听地址（默认：`~/.omnish/omnish.sock`）
- `llm`: LLM后端配置（`LlmConfig`类型）
- `sessions_dir`: 会话存储目录（默认：`~/.omnish/sessions`）

### `ShellConfig`
Shell相关配置，包含：
- `command`: 要执行的shell命令（默认：`$SHELL`环境变量或`/bin/sh`）
- `command_prefix`: 触发LLM查询的命令前缀（默认`:`）
- `intercept_gap_ms`: 命令拦截间隔毫秒数（默认：1000ms）

### `LlmConfig`
LLM配置结构，包含：
- `default`: 默认LLM后端名称（默认：`"claude"`）
- `backends`: LLM后端配置映射表（`HashMap<String, LlmBackendConfig>`）
- `auto_trigger`: 自动触发配置（`AutoTriggerConfig`类型）

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

## 依赖关系
- `serde`: 序列化/反序列化配置结构
- `toml`: TOML配置文件解析
- `anyhow`: 统一的错误处理
- `dirs`: 获取标准目录路径（如用户主目录）

## 配置加载优先级
1. 环境变量指定的配置文件路径（`OMNISH_CLIENT_CONFIG`/`OMNISH_DAEMON_CONFIG`）
2. 默认配置文件路径（`~/.omnish/client.toml`/`~/.omnish/daemon.toml`）
3. 内置默认值（如果配置文件不存在）

## 默认值说明
- Shell命令: `$SHELL`环境变量或`/bin/sh`
- 命令前缀: `:`
- 守护进程socket路径: `~/.omnish/omnish.sock`
- 会话目录: `~/.omnish/sessions`
- 默认LLM后端: `"claude"`
- 拦截间隔: 1000ms
- 自动触发冷却时间: 5秒