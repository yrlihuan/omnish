# omnish-context 模块

**功能:** 上下文构建，命令选择和格式化

## 模块概述

omnish-context 模块负责构建LLM查询的上下文，选择相关命令并格式化输出。它提供了灵活的上下文策略和格式化器，可以根据不同需求构建命令历史上下文。

## 重要数据结构

### `CommandContext`
预处理的命令数据，准备进行格式化：
```rust
pub struct CommandContext {
    pub session_id: String,
    pub hostname: Option<String>,
    pub command_line: Option<String>,
    pub cwd: Option<String>,        // home 目录已替换为 ~
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub output: String,
    pub exit_code: Option<i32>,
}
```
**注意:** `cwd` 字段中的 home 目录前缀会被替换为 `~`（通过 `shorten_home`/`shorten_cwd` 函数），以缩短上下文长度。

### `StreamReader` trait
读取命令输出流的接口：
```rust
pub trait StreamReader: Send + Sync {
    fn read_command_output(&self, offset: u64, length: u64) -> Result<Vec<StreamEntry>>;
}
```

### `ContextStrategy` trait
上下文策略接口，用于选择要包含在上下文中的命令：
```rust
#[async_trait]
pub trait ContextStrategy: Send + Sync {
    async fn select_commands<'a>(&self, commands: &'a [CommandRecord]) -> Vec<&'a CommandRecord>;
}
```

### `ContextFormatter` trait
上下文格式化器接口，用于将选定的命令格式化为最终上下文字符串。接口已更新为接受 history 和 detailed 两个独立切片：
```rust
pub trait ContextFormatter: Send + Sync {
    fn format(&self, history: &[CommandContext], detailed: &[CommandContext]) -> String;
}
```
- `history`: 较旧的命令，仅包含命令行（无输出）
- `detailed`: 最近的命令，包含完整输出

### `RecentCommands`
最近命令策略实现，选择最近的N个命令：
```rust
pub struct RecentCommands {
    max: usize,
    current_session_id: Option<String>,
    min_current_session_commands: usize,
}
```

### `GroupedFormatter`
按会话分组的格式化器，将当前会话的命令放在最后（最靠近 LLM 提示词）。每个有输出的命令后跟20个连字符分隔符：
```rust
pub struct GroupedFormatter {
    current_session_id: String,
    head_lines: usize,
    tail_lines: usize,
}
```
**分隔符位置:** `--------------------`（20个连字符）放在每个命令的输出之后，用于清晰地分隔多个命令块。

### `InterleavedFormatter`
按时间交错排序的格式化器，所有命令按时间顺序排列。每个有输出的命令后跟20个连字符分隔符：
```rust
pub struct InterleavedFormatter {
    current_session_id: String,
    head_lines: usize,
    tail_lines: usize,
}
```
**分隔符位置:** `--------------------`（20个连字符）放在每个命令的输出之后，用于清晰地分隔多个命令块。

### `CompletionFormatter`
为补全场景专门设计的格式化器，优化 KV 缓存命中率：
```rust
pub struct CompletionFormatter {
    current_session_id: String,
    head_lines: usize,
    tail_lines: usize,
    max_command_output_chars: usize,  // 每个命令输出的字符上限
    live_cwd: Option<String>,         // 来自 session probe 的实时 shell cwd
}
```
**KV 缓存优化策略:**
- History 部分用 `<history>` / `</history>` 标签包裹，在弹性窗口重置时保持稳定不变（frozen history section）
- Recent 部分用 `<recent>` / `</recent>` 标签包裹，新命令追加在末尾
- 不使用复杂的子 XML 标签，而是纯文本格式加 `<cmd>` 语义标签
- 使用稳定 term 标签（stable labels，见下文），避免因当前会话变化而导致标签重排
- 当前工作目录单独包裹在 `<system-reminder>` 标签中，格式为：
  ```
  <system-reminder>
  # workingDirectory
  /path/to/current/directory
  </system-reminder>
  ```
- Claude 模型针对 `<system-reminder>` 标签有特殊训练，能更好地理解和利用工作目录上下文

**工作目录来源优先级:** 优先使用 `live_cwd`（daemon session probe 通过 `/proc/<pid>/cwd` 轮询获取的实时 shell 工作目录），回退到最后一条当前会话 CommandRecord 的 cwd。这解决了 OSC 133;B DEBUG trap 在命令执行前触发的问题--例如 `cd /tmp` 记录的是旧 cwd 而非新目录。

## 关键函数说明

### `build_context()`
构建LLM查询上下文的主要函数：
```rust
pub async fn build_context(
    strategy: &dyn ContextStrategy,
    formatter: &dyn ContextFormatter,
    commands: &[CommandRecord],
    reader: &dyn StreamReader,
    session_hostnames: &HashMap<String, String>,
    detailed_count: usize,
    max_line_width: usize,
) -> Result<String>
```
**参数:**
- `strategy`: 上下文策略，选择要包含的命令
- `formatter`: 上下文格式化器，格式化命令为文本
- `commands`: 命令记录列表
- `reader`: 流读取器，读取命令输出
- `session_hostnames`: session_id -> hostname 映射，用于在标签中显示主机名
- `detailed_count`: 最近多少条命令显示完整输出（其余仅显示命令行）
- `max_line_width`: 每行最大字符宽度，超出截断并加 `...`

**返回:** `Result<String>` 格式化后的上下文字符串

**用途:** 协调策略选择命令、读取流数据、格式化器生成文本的完整流程

### `build_context_with_session()`
与 `build_context()` 相同，但额外保证当前会话至少有指定数量的命令出现在 detailed 部分：
```rust
pub async fn build_context_with_session(
    strategy: &dyn ContextStrategy,
    formatter: &dyn ContextFormatter,
    commands: &[CommandRecord],
    reader: &dyn StreamReader,
    session_hostnames: &HashMap<String, String>,
    detailed_count: usize,
    max_line_width: usize,
    current_session_id: Option<&str>,
    min_current_session_detailed: usize,
) -> Result<String>
```

### `select_and_split()`
策略选择命令并分割为 (history, detailed) 的单一入口，供 `build_context_with_session` 和 `/sessions` 显示共用：
```rust
pub async fn select_and_split<'a>(
    strategy: &dyn ContextStrategy,
    commands: &'a [CommandRecord],
    detailed_count: usize,
    current_session_id: Option<&str>,
    min_current_session_detailed: usize,
) -> (Vec<&'a CommandRecord>, Vec<&'a CommandRecord>)
```

### `shorten_home()`
将路径中的用户 home 目录前缀替换为 `~`：
```rust
pub fn shorten_home(path: &str) -> String
```
**可见性:** `pub`（公开），供 daemon 等外部 crate 使用（如 session_mgr 对 live_cwd 进行缩写）。

### `strip_ansi()`
从原始字节中去除ANSI转义序列（CSI和OSC）：
```rust
pub fn strip_ansi(raw: &[u8]) -> String
```
**参数:** `raw: &[u8]` 包含ANSI转义序列的原始字节
**返回:** `String` 去除ANSI转义序列后的纯文本
**用途:** 清理终端输出，移除颜色和格式控制字符

**输出预处理流程（build_context_with_session 中）:**
1. 去除 ANSI 转义序列
2. 跳过 PTY 输出流的第一行（含提示符和回显命令行，避免重复）
3. 裁剪开头空白字符（`trim_start()`，含 `\r`、`\n`）
4. 按 `max_line_width` 截断过长行（避免进度条等单行噪音）

### `RecentCommands::new()`
创建最近命令策略：
```rust
pub fn new(max: usize) -> Self
```
**参数:** `max` 最大命令数量
**返回:** `RecentCommands` 策略实例
**用途:** 创建选择最近N个命令的策略实例

### `RecentCommands::with_current_session()`
为策略设置当前会话，保证该会话最少出现的命令数：
```rust
pub fn with_current_session(mut self, session_id: &str, min_commands: usize) -> Self
```

### `GroupedFormatter::new()`
创建按会话分组的格式化器：
```rust
pub fn new(current_session_id: &str, now_ms: u64, head_lines: usize, tail_lines: usize) -> Self
```
**参数:**
- `current_session_id`: 当前会话ID
- `now_ms`: 当前时间戳（毫秒）
- `head_lines`: 截断时保留的头部行数
- `tail_lines`: 截断时保留的尾部行数

**返回:** `GroupedFormatter` 分组格式化器实例

### `InterleavedFormatter::new()`
创建按时间交错排序的格式化器：
```rust
pub fn new(current_session_id: &str, now_ms: u64, head_lines: usize, tail_lines: usize) -> Self
```
**参数:**
- `current_session_id`: 当前会话ID
- `now_ms`: 当前时间戳（毫秒）
- `head_lines`: 截断时保留的头部行数
- `tail_lines`: 截断时保留的尾部行数

**返回:** `InterleavedFormatter` 交错排序格式化器实例

### `CompletionFormatter::new()`
创建补全场景专用格式化器：
```rust
pub fn new(current_session_id: &str, head_lines: usize, tail_lines: usize) -> Self
```
默认 `max_command_output_chars` 为 500。可通过 `with_max_command_output_chars()` 调整。默认 `live_cwd` 为 `None`。可通过 `with_live_cwd()` 设置。

### `CompletionFormatter::with_live_cwd()`
设置实时 shell 工作目录，用于 `<system-reminder>` 中的 workingDirectory 输出：
```rust
pub fn with_live_cwd(mut self, cwd: Option<String>) -> Self
```
**参数:** `cwd` 来自 daemon session probe（`/proc/<pid>/cwd` 轮询）的实时工作目录。传入 `None` 则回退到最后一条当前会话命令的 cwd。
**用途:** 解决 OSC 133;B DEBUG trap 在命令执行前触发导致 `cd` 等命令记录旧 cwd 的问题。

## 格式化工具函数

### `format_relative_time()`
将毫秒时间戳格式化为相对时间字符串：
```rust
pub fn format_relative_time(timestamp_ms: u64, now_ms: u64) -> String
```
**规则:**
- <60秒: "Ns ago"
- <60分钟: "Nm ago"
- <24小时: "Nh ago"
- >=24小时: "Nd ago"
- 如果 `now_ms <= timestamp_ms`: "just now"

### `assign_term_labels()`
为会话分配终端标签（当前会话优先为 "term A"）：
```rust
pub fn assign_term_labels(
    commands: &[CommandContext],
    current_session_id: &str,
) -> HashMap<String, String>
```
**规则:**
- 当前会话ID -> "term A"
- 其他会话 -> "term B", "term C" 等（按首次出现顺序）
- 若会话有 hostname，格式为 `"hostname (term A)"`；否则仅 `"term A"`
- 使用**双射 base-26 编码**：支持超过26个会话，如 "term AA"、"term AB"、...、"term ZZ"、"term AAA" 等

### `assign_stable_term_labels()`
为会话分配稳定终端标签（不依赖当前会话，按首次出现顺序分配）：
```rust
pub fn assign_stable_term_labels(
    commands: &[super::CommandContext],
) -> HashMap<String, String>
```
**用途:** 补全场景使用，标签不随当前会话切换而变化，保持 KV 缓存前缀稳定。

### `truncate_lines()`
截断输出行，同时支持字符数限制：
```rust
pub fn truncate_lines(text: &str, max_lines: usize, head: usize, tail: usize, max_chars: Option<usize>) -> String
```
**规则:**
- 如果总行数 ≤ max_lines: 返回所有行（但仍检查 max_chars 限制）
- 否则: 保留头部head行 + "... (N lines omitted) ..." + 尾部tail行
- `max_chars`: 每个命令输出的字符上限（`CompletionFormatter` 用于限制单命令输出体积）

### `truncate_line_width()`
截断输出中过长的行：
```rust
pub fn truncate_line_width(text: &str, max_width: usize) -> String
```
**规则:** 超过 `max_width` 字符的行截断并追加 `...`；`max_width == 0` 时不截断

## 使用示例

### 基本用法
```rust
use omnish_context::{build_context, RecentCommands, GroupedFormatter};
use omnish_store::command::CommandRecord;
use std::collections::HashMap;

// 创建策略和格式化器
let strategy = RecentCommands::new(10);
let formatter = GroupedFormatter::new("current-session-id", 1700000000000, 10, 10);
let reader = MyStreamReader::new();
let session_hostnames = HashMap::new();

// 构建上下文
let context = build_context(&strategy, &formatter, &command_records, &reader,
    &session_hostnames, 5, 512).await?;
```

### 补全场景（KV 缓存优化）
```rust
use omnish_context::{build_context_with_session, RecentCommands, recent::CompletionFormatter};

let strategy = RecentCommands::new(10)
    .with_current_session("current-session-id", 3);
let formatter = CompletionFormatter::new("current-session-id", 10, 10)
    .with_max_command_output_chars(500)
    .with_live_cwd(Some("/home/user/project".to_string()));

let context = build_context_with_session(
    &strategy, &formatter, &commands, &reader,
    &session_hostnames, 5, 512,
    Some("current-session-id"), 3,
).await?;
```

### 自定义策略和格式化器
```rust
use omnish_context::{ContextStrategy, ContextFormatter, InterleavedFormatter};

// 使用交错排序格式化器
let formatter = InterleavedFormatter::new("current-session-id", 1700000000000, 10, 10);

// 实现自定义策略
struct MyCustomStrategy;
#[async_trait]
impl ContextStrategy for MyCustomStrategy {
    async fn select_commands<'a>(&self, commands: &'a [CommandRecord]) -> Vec<&'a CommandRecord> {
        // 自定义选择逻辑
        commands.iter().filter(|c| c.exit_code == Some(0)).collect()
    }
}
```

### 格式化工具使用
```rust
use omnish_context::format_utils::{format_relative_time, truncate_lines};

let time_str = format_relative_time(1699999999000, 1700000000000); // "1s ago"
let truncated = truncate_lines("line1\nline2\n...\nline100", 20, 10, 10, Some(500));
```

## 配置参数

以下参数由调用方在构造时传入（无硬编码常量）：

| 参数 | 说明 | 典型值 |
|------|------|--------|
| `max` (RecentCommands) | 最大命令数 | 10 |
| `head_lines` / `tail_lines` | 截断时保留的头/尾行数 | 10 / 10 |
| `detailed_count` | 显示完整输出的最近命令数 | 5 |
| `max_line_width` | 每行最大字符宽度 | 512（已从较大值降低） |
| `max_command_output_chars` | 单命令输出字符上限（CompletionFormatter） | 500 |
| `live_cwd` | 实时 shell 工作目录（CompletionFormatter） | `None`（回退到命令记录 cwd） |

## 依赖关系
- `omnish-store`: 命令记录类型 (`CommandRecord`, `StreamEntry`)
- `async-trait`: 异步trait支持
- `anyhow`: 错误处理
- `std::collections::HashMap`: 会话标签映射

## 设计特点

1. **策略模式**: 通过 `ContextStrategy` trait 支持不同的命令选择策略
2. **格式化器模式**: 通过 `ContextFormatter` trait 支持不同的输出格式
3. **History/Detailed 分离**: 较旧命令仅显示命令行（history），最近命令显示完整输出（detailed），节省 token
4. **异步支持**: 策略选择支持异步操作
5. **ANSI清理**: 自动清理终端输出中的ANSI转义序列
6. **home 目录缩写**: cwd 中的 home 目录前缀替换为 `~`，缩短上下文；`shorten_home()` 为 `pub` 可供外部 crate 使用
7. **会话管理**: 支持多会话命令的分组和标签分配；hostname 与 term 标签合并显示
8. **双射 base-26 标签**: term 标签支持任意数量会话（A, B, ..., Z, AA, AB, ...）
9. **稳定标签**: `assign_stable_term_labels` 不依赖当前会话，保持 KV 缓存前缀稳定
10. **输出截断**: 智能截断长输出，保留重要信息；支持行截断和字符数双重限制
11. **前导空白裁剪**: 命令输出去除开头的 `\r`、`\n` 等多余空白
12. **20连字符分隔符**: 命令块之间使用 `--------------------` 分隔，提升可读性
13. **时间格式化**: 相对时间显示，提高可读性
14. **KV缓存优化**: `CompletionFormatter` 将 history 区冻结，新命令仅追加到 recent 末尾，最大化缓存命中
15. **实时工作目录**: `CompletionFormatter` 通过 `live_cwd` 使用 daemon session probe 的实时 shell cwd，解决 DEBUG trap 在命令执行前触发导致 `cd` 记录旧路径的问题

## 测试覆盖

模块包含完整的单元测试，覆盖：
- 策略选择逻辑
- 格式化器输出（GroupedFormatter、InterleavedFormatter、CompletionFormatter）
- 工具函数行为
- 集成场景测试
- ANSI清理功能
- 时间格式化功能
- term 标签分配（单会话、多会话、hostname 回退、双射 base-26 多层级）
- 字符截断和行截断
- 稳定标签（stable labels）行为
- 补全格式化纯文本格式验证
