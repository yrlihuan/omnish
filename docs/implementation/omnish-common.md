# omnish-common 模块

**功能:** 共享配置和工具函数

## 模块概述

omnish-common 包含客户端和守护进程共享的配置结构和工具函数。该模块提供了统一的配置加载机制、默认值设置以及共享的数据类型定义，确保客户端和守护进程使用一致的配置。

## 重要数据结构

### `ClientConfig`
客户端配置结构，包含：
- `shell`: Shell配置（`ShellConfig`类型）
- `daemon_addr`: 守护进程地址（默认：`~/.omnish/omnish.sock`）
- `onboarded`: 用户是否已完成新手引导（默认：`false`）；首次进入聊天后自动写入 `true`
- `sandbox`: 客户端本地沙箱配置（`ClientSandboxConfig`类型）

注意：`auto_update` 字段已从 `ClientConfig` 中移除，自动更新功能统一由守护进程端的 `TasksConfig.auto_update` 管理。

### `ClientSandboxConfig`
客户端本地沙箱设置。因沙箱能力取决于内核/OS 特性（bwrap、landlock、seatbelt），故按主机配置，包含：
- `enabled`: 总开关，控制所有客户端侧沙箱（默认：`true`）
- `backend`: 首选沙箱后端（`"bwrap"` | `"landlock"` | `"macos"`），Linux 默认 `"bwrap"`，macOS 默认 `"macos"`；运行时可用性检测可能覆盖此值
- `plugins`: 客户端本地的每工具豁免规则（`HashMap<String, SandboxPluginConfig>`），与守护进程端规则在运行时合并，客户端规则优先

### `DaemonConfig`
守护进程配置结构（派生`Serialize`和`Deserialize`，所有子结构同样），包含：
- `listen_addr`: 监听地址（默认：`~/.omnish/omnish.sock`）
- `proxy`: 代理配置（`ProxyConfig`类型）
- `llm`: LLM后端配置（`LlmConfig`类型）
- `context`: 上下文构建配置（`ContextConfig`类型）
- `tasks`: 定时任务配置（`TasksConfig`类型，即 `HashMap<String, ConfigMap>`）
- `plugins`: 插件配置（`HashMap<String, ConfigMap>`），每个插件以名称为键，`ConfigMap` 中包含 `enabled` 等键值
- `sandbox`: 沙箱规则配置（`SandboxConfig`类型）
- `client`: 守护进程端的客户端配置（`ClientSection`类型），通过 `ConfigClient` 消息推送到连接的客户端

**`normalize()`方法：** 将旧版顶层 `no_proxy` 字段迁移到 `proxy.no_proxy`，实现向后兼容。

### `ShellConfig`
Shell相关配置，包含：
- `command`: 要执行的shell命令（默认：`$SHELL`环境变量或`/bin/sh`）
- `command_prefix`: 触发LLM查询的命令前缀（默认`:`）
- `resume_prefix`: 恢复上次聊天线程的前缀（默认`::`）
- `intercept_gap_ms`: 命令拦截间隔毫秒数（默认：1000ms）
- `ghost_timeout_ms`: ghost-text超时毫秒数（默认：10000ms）
- `developer_mode`: 开发者模式。默认关闭时命令行有内容则 `:` 和 `::` 不触发聊天模式；启用后即使有内容也允许进入聊天（默认：`false`）
- `completion_enabled`: 是否启用自动补全（默认：`true`，从 `ClientConfig` 迁移至此）
- `extended_unicode`: 是否使用扩展 Unicode 字符（如 ⎿），对于字体支持不完整的终端（如 ConEmu 默认字体）设为 `false`（默认：`true`）

以上 `bool` 字段均支持 `string_or_bool` 反序列化（接受 `true`/`false` 和 `"true"`/`"false"`）。

### `LlmConfig`
LLM配置结构（派生`PartialEq`，用于热重载配置差异检测），包含：
- `default`: 默认LLM后端名称（默认：`"claude"`）
- `backends`: LLM后端配置映射表（`HashMap<String, LlmBackendConfig>`）
- `use_cases`: UseCase到后端名的映射（`HashMap<String, String>`）
- `langfuse`: Langfuse可观测性集成配置（`Option<LangfuseConfig>`，可选）

注意：`auto_trigger`（`AutoTriggerConfig`）字段已从 `LlmConfig` 中移除。

### `LangfuseConfig`
Langfuse可观测性集成配置（派生`PartialEq`，用于热重载配置差异检测），包含：
- `public_key`: Langfuse公钥（默认：空字符串）
- `secret_key`: Langfuse密钥（`Option<String>`，直接值，非shell命令）
- `base_url`: Langfuse服务URL（默认：`https://cloud.langfuse.com`）

### `ContextConfig`
上下文构建配置，包含：
- `completion`: 补全上下文配置（`CompletionContextConfig`类型）

### `CompletionContextConfig`
补全上下文配置，包含：
- `detailed_commands`: 显示完整详情（输出、耗时、退出码）的近期命令数量（默认：30）
- `history_commands`: 仅显示命令行（无输出）的历史命令数量（默认：500）
- `head_lines` / `tail_lines`: 每条命令输出保留的头/尾行数（默认：20/20）
- `max_line_width`: 行最大宽度（默认：200）
- `min_current_session_commands`: 当前会话最少保留命令数（默认：5）
- `max_context_chars`: 上下文最大字符数限制（可选，超出时自动缩减窗口）
- `detailed_min` / `detailed_max`: 弹性详细窗口范围（默认：20/30）

### `ProxyConfig`
代理配置结构，包含：
- `http_proxy`: HTTP/SOCKS 代理 URL（可选，格式如 `http://...`、`socks5://...`）
- `no_proxy`: 逗号分隔的不走代理的主机/域名/CIDR（可选）

支持向后兼容反序列化：接受旧版 `proxy = "http://..."` 字符串格式和新版 `[proxy]` 表格式。

### `ClientSection`
守护进程端的客户端配置，通过 `ConfigClient` 协议消息推送到连接的客户端：
- `command_prefix`: 触发聊天的命令前缀
- `resume_prefix`: 恢复线程的前缀
- `completion_enabled`: 是否启用自动补全
- `ghost_timeout_ms`: ghost-text 超时
- `intercept_gap_ms`: 拦截间隔
- `developer_mode`: 开发者模式

### `ConfigMap`
动态键值配置，内部维护 `values`（用户设置）和 `defaults`（任务/插件默认值）两层，查询时 values 优先。提供 `get_bool`/`get_u64`/`get_string`/`get_opt_string`/`get`/`contains_key`/`iter` 方法，以及 `set_defaults()` 注入默认值层。`PartialEq` 仅比较 values（不含 defaults）。序列化输出合并视图（defaults + values），反序列化仅填充 values。

### `TasksConfig`
定时任务配置，类型别名 `HashMap<String, ConfigMap>`。每个任务以名称为键，`ConfigMap` 存储该任务的覆盖参数，未设置的参数由各任务内部硬编码默认值。

**内置任务名称（6个）：**
- `eviction`: 会话淘汰（默认参数如 `session_evict_hours` 由任务内部决定）
- `hourly_summary`: 小时摘要生成
- `daily_notes`: 日报生成（如 `enabled`、`schedule_hour` 等）
- `disk_cleanup`: 磁盘清理（如 `schedule` cron表达式）
- `auto_update`: 自动更新（如 `enabled`、`schedule`、`check_url` 等）
- `thread_summary`: 线程摘要生成

**示例配置：**
```toml
[tasks.daily_notes]
enabled = true
schedule_hour = 23

[tasks.auto_update]
enabled = true
schedule = "0 0 4 * * *"
```

### `PluginsConfig` / 插件配置
插件配置现为 `HashMap<String, ConfigMap>`（`DaemonConfig.plugins` 字段直接使用），每个插件以名称为键，`ConfigMap` 中包含 `enabled` 等键值对。

**示例配置：**
```toml
[plugins.web_search]
enabled = true
api_key = "my-search-api-key"

[plugins.another_plugin]
enabled = true
```

插件对应 `~/.omnish/plugins/{name}/` 目录下的 `{name}` 可执行文件，通过 JSON-RPC 协议与守护进程通信，在守护进程启动时加载并初始化。

### `SandboxConfig`
守护进程端沙箱配置，仅包含守护进程全局的豁免规则（`backend` 和 `enabled` 已移至客户端侧 `ClientSandboxConfig`）：
- `plugins`: 每个工具的沙箱豁免规则（`HashMap<String, SandboxPluginConfig>`，键为工具名如 `"bash"`）

### `SandboxPluginConfig`
单个工具的沙箱豁免配置，包含：
- `permit_rules`: 规则字符串列表，格式为 `"<param_field> <operator> <value>"`；当任意规则命中时，该工具调用跳过 Landlock 沙箱。支持的运算符：`starts_with`、`contains`、`equals`、`matches`（正则）。

**背景：** Snap 安装的二进制（如 `glab`、`docker`）在 Landlock 沙箱下因 `PR_SET_NO_NEW_PRIVS` 阻止 setuid 提权而失败，需通过此机制选择性豁免。

### `LlmBackendConfig`
LLM后端具体配置（派生`PartialEq`，用于热重载配置差异检测），包含：
- `backend_type`: 后端类型（默认：`"openai-compat"`，如`"openai"`、`"anthropic"`等）
- `model`: 模型名称（默认：空字符串）
- `api_key_cmd`: 获取API密钥的命令（可选）
- `base_url`: API基础URL（可选）
- `use_proxy`: 是否使用全局代理访问该后端（`bool`，默认：`false`）
- `context_window`: 上下文窗口大小，以token为单位（`Option<usize>`，模型相关）；当 `max_content_chars` 未设置时，默认取 `context_window * 1.5`
- `max_content_chars`: 模型上下文最大字符数（`Option<usize>`）。高级覆盖项，若未设置则由 `context_window * 1.5` 推导

所有字段均带有 `#[serde(default)]`，因此在 TOML 配置文件中可以只写需要覆盖的字段，省略的字段将使用默认值。

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
**容错:** 解析失败时自动调用 `sanitize_toml()` 清理重复的表头和键后重试；成功后调用 `normalize()` 迁移旧格式

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

### `set_toml_value_nested()` / `set_toml_value_nested_bool()` / `set_toml_value_nested_int()`
原地更新 TOML 配置文件中的嵌套键值，支持点分隔路径，保留原有格式。

**参数:**
- `path: &Path` - 配置文件路径
- `key: &str` - 点分隔的键路径（如 `"llm.use_cases.completion"`）；支持引号包裹含点号的后端名称（如 `"llm.backends.\"gemini-3.1\".model"`），由 `split_key_path()` 解析
- `value: &str`、`bool` 或 `i64` - 新值

**返回:** `anyhow::Result<()>`

**行为:**
- 自动创建不存在的中间表（table）
- 文件不存在时自动创建
- 确保文件末尾有换行符
- `split_key_path()` 以引号感知方式按 `.` 分割键路径，引号内的 `.` 不作为分隔符

**典型用途:**
- `/config` 命令修改daemon嵌套配置项（如 `"tasks.daily_notes.enabled"` → `true`）
- 修改含点号名称的后端配置（如 `"llm.backends.\"gemini-3.1\".model"` → `"gemini-3.1-pro"`）

### `set_toml_nested_in_doc()`
在已解析的 `DocumentMut` 上设置嵌套键值，自动创建中间表。从 `set_toml_value_nested_inner` 中提取，供外部调用方复用。

### TOML 数组与表操作
文件锁定（`with_locked_doc`，基于 `fs2::FileExt`）的原子操作系列：
- `append_to_toml_array(path, key, value)`: 向点分隔路径的 TOML 数组追加字符串元素，数组不存在时自动创建
- `remove_from_toml_array(path, key, index)`: 按索引删除数组元素，越界时报错
- `replace_in_toml_array(path, key, index, value)`: 按索引替换数组元素
- `remove_toml_table(path, key)`: 删除点分隔路径指定的嵌套表（如删除某个 LLM backend）

**典型用途:**
- `/config` 菜单中沙箱豁免规则的增删改
- LLM backend 的删除

## sandbox_rule 模块

沙箱豁免规则的共享工具函数，供守护进程和客户端共用：
- `OPERATORS`: 支持的运算符常量列表（`starts_with`、`contains`、`equals`、`matches`）
- `parse_rule_parts(rule)`: 将规则字符串（如 `"command starts_with glab"`）解析为 `(field, operator, value)` 三元组，operator 缺省时默认为 `"starts_with"`
- `check_bypass_raw(rules, input)`: 检查规则列表中是否有任一匹配给定工具输入（OR 逻辑），返回首个匹配的规则字符串

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

// 顶层键
let path = Path::new("/home/user/.omnish/client.toml");
config_edit::set_toml_value(path, "onboarded", true)?;

// 嵌套键路径
let daemon_path = Path::new("/home/user/.omnish/daemon.toml");
config_edit::set_toml_value_nested(daemon_path, "llm.use_cases.completion", "claude-fast")?;
config_edit::set_toml_value_nested_bool(daemon_path, "tasks.daily_notes.enabled", true)?;
config_edit::set_toml_value_nested_int(daemon_path, "tasks.hourly_summary.interval_hours", 4)?;

// 含点号的后端名称（引号包裹）
config_edit::set_toml_value_nested(daemon_path, "llm.backends.\"gemini-3.1\".model", "gemini-3.1-pro")?;
```

### 配置文件示例 (client.toml)
```toml
[shell]
command = "/bin/bash"
command_prefix = ":"
resume_prefix = "::"
intercept_gap_ms = 500
# developer_mode = false
# completion_enabled = true
# extended_unicode = true

daemon_addr = "/tmp/omnish.sock"
onboarded = false
```

### 配置文件示例 (daemon.toml)
```toml
listen_addr = "/tmp/omnish.sock"

# 代理配置（可选）
[proxy]
# http_proxy = "http://proxy.example.com:8080"
# no_proxy = "localhost,127.0.0.1,*.internal.com"

[llm]
default = "claude"

[llm.backends.claude]
backend_type = "anthropic"
model = "claude-3-haiku-20240307"
api_key_cmd = "pass show api/anthropic"
# use_proxy = false
context_window = 200000
# max_content_chars = 300000  # 高级覆盖项，若未设置则由 context_window * 1.5 推导

[llm.langfuse]
public_key = "pk-lf-..."
secret_key = "sk-lf-..."
base_url = "https://cloud.langfuse.com"

# 定时任务（每个任务以名称为键，ConfigMap 存储覆盖参数）
[tasks.hourly_summary]
interval_hours = 4

[tasks.daily_notes]
enabled = true
schedule_hour = 23

[tasks.auto_update]
enabled = true
schedule = "0 0 4 * * *"
# check_url = "https://api.github.com/repos/yrlihuan/omnish/releases/latest"

# 插件配置（每个插件以名称为键，ConfigMap 存储参数）
[plugins.web_search]
enabled = true
api_key = "my-search-api-key"

[plugins.another_plugin]
enabled = true

# 沙箱豁免规则（用于 snap 安装的工具等）
[sandbox.plugins.bash]
permit_rules = ["command starts_with glab", "command starts_with docker"]
```

## update 模块

`omnish-common` 包含 `update` 子模块，提供客户端和守护进程共享的更新工具函数。

### 常量
- `MAX_CACHED_PACKAGES`: 每个平台保留的最大更新包数量（默认：3）

### `checksum()`
计算文件的 SHA-256 校验和。

**参数:** `path: &Path`
**返回:** `anyhow::Result<String>`（hex编码的SHA-256值）

### `local_cached_package()`
查找本地缓存中指定平台的最新更新包。

**参数:** `os: &str`, `arch: &str`
**返回:** `Option<(String, PathBuf)>`（版本号和文件路径）
**路径:** `~/.omnish/updates/{os}-{arch}/`

### `prune_packages()`
清理旧更新包，保留最新的指定数量。

**参数:** `dir: &Path`, `os: &str`, `arch: &str`, `keep: usize`
**行为:** 按版本降序排列，删除超出 `keep` 数量的旧包

### `extract_version()` / `normalize_version()` / `compare_versions()`
版本字符串工具函数：
- `extract_version`: 从包文件名（如 `omnish-0.8.4-linux-x86_64.tar.gz`）提取版本号
- `normalize_version`: 规范化版本字符串（去除git hash后缀，替换`-`为`.`）
- `compare_versions`: semver风格的版本比较

### `extract_and_run_installer()`
解压tar.gz更新包并运行其内置的 `install.sh --upgrade`。

**参数:**
- `tar_gz_path: &Path` - 更新包路径
- `version: &str` - 版本号（用于日志文件名）
- `client_only: bool` - 是否传递 `--client-only` 跳过daemon安装

**行为:**
- 解压到PID唯一目录（`{path}.extracted.{pid}`）避免daemon和client并行更新时的竞态条件
- 运行包内的 `install.sh --upgrade [--client-only]`，支持跨版本升级（安装逻辑在包内脚本中）
- 日志保存到 `~/.omnish/logs/updates/update-{version}-{timestamp}.log`
- exit code 2 表示已是最新版本（非错误）
- 完成后自动清理解压目录
- 解压和安装各步骤附带 `anyhow::Context` 错误上下文，便于排查失败原因

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
- `serde_json`: `ConfigMap` 值类型支持（任务和插件配置）
- `sha2`: SHA-256校验和计算（用于 `update` 模块）
- `flate2`: gzip解压（用于 `update` 模块）
- `tar`: tar归档解压（用于 `update` 模块）
- `tracing`: 日志记录（用于 `update` 模块）

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
- LLM后端类型: `"openai-compat"`
- 拦截间隔: 1000ms
- Ghost-text超时: 10000ms
- 认证令牌路径: `~/.omnish/auth_token`
- Langfuse base_url: `https://cloud.langfuse.com`
- 定时任务: `TasksConfig` 默认为空 `HashMap`，各任务的默认参数由任务实现内部硬编码
- 插件配置: 默认为空 `HashMap`
- 全局代理: 无（`proxy.http_proxy` 和 `proxy.no_proxy` 均为 `None`）
- 扩展 Unicode: `true`
- 补全: `true`

## 更新历史

- **2026-03-30**: `LlmBackendConfig` 新增 `use_proxy`（是否使用全局代理）和 `context_window`（上下文窗口大小）字段；`max_content_chars` 更新为高级覆盖项，未设置时由 `context_window * 1.5` 推导；`LlmConfig`、`LangfuseConfig`、`LlmBackendConfig` 新增 `PartialEq` 派生，用于热重载配置差异检测。
- **2026-04-02**: 统一动态配置架构——新增 `ConfigMap` 新类型（含 `get_bool`/`get_u64`/`get_string`/`get_opt_string` 访问方法）；`TasksConfig` 从带类型字段的结构体改为 `HashMap<String, ConfigMap>`，删除 `EvictionConfig`、`HourlySummaryConfig`、`DailyNotesConfig`、`DiskCleanupConfig`、`AutoUpdateConfig`、`ThreadSummaryConfig`、`PeriodicSummaryConfig` 等子结构，各任务默认值改为内部硬编码；`PluginsConfig` 从 `enabled: Vec<String>` 改为 `HashMap<String, ConfigMap>`；`ContextConfig` 移除 `hourly_summary`/`daily_summary` 死代码字段；`DaemonConfig` 移除 `tools` 字段（合并入 `plugins`）；`config_edit` 新增 `set_toml_value_nested_int()` 和引号感知的 `split_key_path()`，支持含点号的后端名称（如 `gemini-3.1`）。
- **2026-04-09b**: `SandboxConfig` 新增 `backend` 字段（`"bwrap"` | `"landlock"` | `"macos"`），支持多后端沙箱选择，Linux 默认 `"bwrap"`，macOS 默认 `"macos"`。
- **2026-04-09**: 配置架构重构——`proxy`/`no_proxy` 从顶层字段迁移到 `ProxyConfig` 结构（`[proxy]` 表），自定义反序列化支持旧版字符串格式向后兼容；`completion_enabled` 从 `ClientConfig` 迁移到 `ShellConfig`；新增 `extended_unicode` 配置项；`ConfigMap` 封装内部结构，新增 `defaults` 层和 `set_defaults()`/`get()`/`contains_key()`/`iter()` 方法；新增 `ClientSection` 结构支持守护进程到客户端的配置推送；`load_daemon_config()` 新增 `sanitize_toml()` 容错（处理重复表头和重复键）和 `normalize()` 旧格式迁移；新增 `string_or_bool` serde helper。
