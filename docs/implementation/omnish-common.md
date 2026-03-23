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
- `auto_update`: 是否启用自动更新，当二进制文件变化时自动重启（默认：`false`）
- `onboarded`: 用户是否已完成新手引导（默认：`false`）；首次进入聊天后自动写入 `true`

### `DaemonConfig`
守护进程配置结构，包含：
- `listen_addr`: 监听地址（默认：`~/.omnish/omnish.sock`）
- `proxy`: 全局出站 HTTP/SOCKS 代理（可选，格式如 `http://...`、`https://...`、`socks5://...`）
- `no_proxy`: 逗号分隔的不走代理的主机/域名/CIDR（可选，如 `"localhost,127.0.0.1,*.internal.com"`）
- `llm`: LLM后端配置（`LlmConfig`类型）
- `context`: 上下文构建配置（`ContextConfig`类型）
- `tasks`: 定时任务配置（`TasksConfig`类型）
- `plugins`: 插件系统配置（`PluginsConfig`类型）
- `tools`: 每个工具的参数注入配置（`HashMap<String, HashMap<String, serde_json::Value>>`）
- `sandbox`: 沙箱规则配置（`SandboxConfig`类型）

### `ShellConfig`
Shell相关配置，包含：
- `command`: 要执行的shell命令（默认：`$SHELL`环境变量或`/bin/sh`）
- `command_prefix`: 触发LLM查询的命令前缀（默认`:`）
- `resume_prefix`: 恢复上次聊天线程的前缀（默认`::`）
- `intercept_gap_ms`: 命令拦截间隔毫秒数（默认：1000ms）
- `ghost_timeout_ms`: ghost-text超时毫秒数（默认：10000ms）
- `developer_mode`: 开发者模式。默认关闭时命令行有内容则 `:` 和 `::` 不触发聊天模式；启用后即使有内容也允许进入聊天（默认：`false`）

### `LlmConfig`
LLM配置结构，包含：
- `default`: 默认LLM后端名称（默认：`"claude"`）
- `backends`: LLM后端配置映射表（`HashMap<String, LlmBackendConfig>`）
- `use_cases`: UseCase到后端名的映射（`HashMap<String, String>`）
- `langfuse`: Langfuse可观测性集成配置（`Option<LangfuseConfig>`，可选）

注意：`auto_trigger`（`AutoTriggerConfig`）字段已从 `LlmConfig` 中移除。

### `LangfuseConfig`
Langfuse可观测性集成配置，包含：
- `public_key`: Langfuse公钥
- `secret_key`: Langfuse密钥（`Option<String>`，直接值，非shell命令）
- `base_url`: Langfuse服务URL（默认：`https://cloud.langfuse.com`）

### `ContextConfig`
上下文构建配置，包含：
- `completion`: 补全上下文配置（`CompletionContextConfig`类型）
- `hourly_summary`: 小时摘要上下文配置（`HourlySummaryConfig`类型）
- `daily_summary`: 日报上下文配置（`DailySummaryConfig`类型）

### `CompletionContextConfig`
补全上下文配置，包含：
- `detailed_commands`: 显示完整详情（输出、耗时、退出码）的近期命令数量（默认：30）
- `history_commands`: 仅显示命令行（无输出）的历史命令数量（默认：500）
- `head_lines` / `tail_lines`: 每条命令输出保留的头/尾行数（默认：20/20）
- `max_line_width`: 行最大宽度（默认：200）
- `min_current_session_commands`: 当前会话最少保留命令数（默认：5）
- `max_context_chars`: 上下文最大字符数限制（可选，超出时自动缩减窗口）
- `detailed_min` / `detailed_max`: 弹性详细窗口范围（默认：20/30）

### `TasksConfig`
定时任务配置，包含：
- `eviction`: 会话淘汰配置（`EvictionConfig`，`session_evict_hours` 默认：48小时）
- `daily_notes`: 日报生成配置（`DailyNotesConfig`，`enabled`默认`false`，`schedule_hour`默认23）
- `periodic_summary`: 周期性摘要配置（`PeriodicSummaryConfig`类型）
- `disk_cleanup`: 磁盘清理配置（`DiskCleanupConfig`，`schedule` cron表达式，默认`"0 0 */6 * * *"`）
- `auto_update`: 自动更新配置（`AutoUpdateConfig`类型）

### `PeriodicSummaryConfig`
周期性摘要任务配置，包含：
- `interval_hours`: 每隔多少小时生成一次摘要（默认：`4`）；时间窗口、cron计划和 LLM 提示词均基于此值动态计算。输出文件路径保持在 `notes/hourly/` 以兼容每日日报。

### `AutoUpdateConfig`
周期性自动更新配置，包含：
- `enabled`: 是否启用自动更新（默认：`false`）
- `schedule`: 检查更新的cron计划（默认：`"0 0 4 * * *"`，每日04:00）
- `clients`: 需要分发更新的客户端主机列表（如 `["user@host1", "user@host2"]`），原存储于 `~/.omnish/clients` 文件，现移至此处
- `check_url`: 更新源，本地目录路径或 GitHub API URL；省略时默认指向 GitHub（`yrlihuan/omnish`）

### `PluginsConfig`
插件系统配置结构，包含：
- `enabled`: 启用的插件名称列表（默认：空列表）
  - 每个插件对应 `~/.omnish/plugins/{name}/` 目录下的 `{name}` 可执行文件
  - 插件通过 JSON-RPC 协议与守护进程通信
  - 插件在守护进程启动时加载并初始化

### `SandboxConfig`
沙箱规则配置，包含：
- `plugins`: 每个工具的沙箱豁免规则（`HashMap<String, SandboxPluginConfig>`，键为工具名如 `"bash"`）

### `SandboxPluginConfig`
单个工具的沙箱豁免配置，包含：
- `permit_rules`: 规则字符串列表，格式为 `"<param_field> <operator> <value>"`；当任意规则命中时，该工具调用跳过 Landlock 沙箱。支持的运算符：`starts_with`、`contains`、`equals`、`matches`（正则）。

**背景：** Snap 安装的二进制（如 `glab`、`docker`）在 Landlock 沙箱下因 `PR_SET_NO_NEW_PRIVS` 阻止 setuid 提权而失败，需通过此机制选择性豁免。

### `LlmBackendConfig`
LLM后端具体配置，包含：
- `backend_type`: 后端类型（如`"openai"`、`"anthropic"`等）
- `model`: 模型名称
- `api_key_cmd`: 获取API密钥的命令（可选）
- `base_url`: API基础URL（可选）
- `max_content_chars`: 模型上下文最大字符数（可选）

## 关键函数说明

### `omnish_dir()`
获取omnish基础目录路径。

**参数:** 无
**返回:** `PathBuf`
**优先级:** `$OMNISH_HOME` 环境变量 > `~/.omnish` > `/tmp/omnish`
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

## config_edit 模块

`omnish-common` 包含 `config_edit` 子模块，提供格式保留的 TOML 配置文件原地修改能力（基于 `toml_edit`）。

### `set_toml_value()`
原地更新 TOML 配置文件中的顶层键值，保留原有注释和格式。

**参数:**
- `path: &Path` - 配置文件路径
- `key: &str` - 要更新的顶层键名
- `value: impl Into<TomlValue>` - 新值（支持 `bool`、`String`、`&str`、`i64`）

**返回:** `anyhow::Result<()>`

**行为:**
- 读取文件并用 `toml_edit` 解析，设置键值后写回
- 自动删除文件中所有含该键名的注释行（避免 `# key = old_value` 残留）
- 确保文件末尾有换行符

**典型用途:**
- 首次进入聊天后将 `onboarded = true` 写入 `client.toml`
- 用户通过 `/auto_update` 命令切换并持久化 `auto_update` 字段

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
```

### 原地更新配置文件
```rust
use omnish_common::config_edit;
use std::path::Path;

let path = Path::new("/home/user/.omnish/client.toml");
config_edit::set_toml_value(path, "auto_update", true)?;
config_edit::set_toml_value(path, "onboarded", true)?;
```

### 配置文件示例 (client.toml)
```toml
[shell]
command = "/bin/bash"
command_prefix = ":"
resume_prefix = "::"
intercept_gap_ms = 500
# developer_mode = false

daemon_addr = "/tmp/omnish.sock"
auto_update = true
onboarded = false
```

### 配置文件示例 (daemon.toml)
```toml
listen_addr = "/tmp/omnish.sock"

# 全局出站代理（可选）
# proxy = "http://proxy.example.com:8080"
# no_proxy = "localhost,127.0.0.1,*.internal.com"

[llm]
default = "claude"

[llm.backends.claude]
backend_type = "anthropic"
model = "claude-3-haiku-20240307"
api_key_cmd = "pass show api/anthropic"
max_content_chars = 200000

[llm.langfuse]
public_key = "pk-lf-..."
secret_key = "sk-lf-..."
base_url = "https://cloud.langfuse.com"

[tasks.periodic_summary]
interval_hours = 4

[tasks.auto_update]
enabled = true
schedule = "0 0 4 * * *"
clients = ["user@host1", "user@host2"]
# check_url = "https://api.github.com/repos/yrlihuan/omnish/releases/latest"

[plugins]
enabled = ["example_plugin", "another_plugin"]

# 每个工具的参数注入
[tools.web_search]
api_key = "my-search-api-key"

# 沙箱豁免规则（用于 snap 安装的工具等）
[sandbox.plugins.bash]
permit_rules = ["command starts_with glab", "command starts_with docker"]
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
- `toml_edit`: 格式保留的TOML编辑（用于 `config_edit` 模块）
- `anyhow`: 统一的错误处理
- `dirs`: 获取标准目录路径（如用户主目录）
- `rand`: 随机令牌生成
- `hex`: 令牌hex编码
- `serde_json`: 工具参数注入的值类型支持（`tools` 配置节）

## 配置加载优先级
1. 环境变量指定的配置文件路径（`OMNISH_CLIENT_CONFIG`/`OMNISH_DAEMON_CONFIG`）
2. 默认配置文件路径（`~/.omnish/client.toml`/`~/.omnish/daemon.toml`）
3. 内置默认值（如果配置文件不存在）

首次安装时，`install.sh` 会生成带有注释默认值的 `daemon.toml` 和 `client.toml`，所有配置项均以注释形式呈现，方便用户按需启用。

## 默认值说明
- Shell命令: `$SHELL`环境变量或`/bin/sh`
- 命令前缀: `:`
- 恢复线程前缀: `::`
- 开发者模式: `false`
- 守护进程socket路径: `~/.omnish/omnish.sock`（或 `$OMNISH_HOME/omnish.sock`）
- 默认LLM后端: `"claude"`
- 拦截间隔: 1000ms
- Ghost-text超时: 10000ms
- 认证令牌路径: `~/.omnish/auth_token`
- 自动更新: `false`
- Langfuse base_url: `https://cloud.langfuse.com`
- 会话淘汰时间: 48小时
- 周期性摘要间隔: 4小时
- 磁盘清理计划: `"0 0 */6 * * *"`（每6小时）
- 自动更新检查计划: `"0 0 4 * * *"`（每日04:00）
- 全局代理: 无（`proxy` 和 `no_proxy` 均为 `None`）
