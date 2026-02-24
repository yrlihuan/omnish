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
    pub command_line: Option<String>,
    pub cwd: Option<String>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub output: String,
    pub exit_code: Option<i32>,
}
```

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
上下文格式化器接口，用于将选定的命令格式化为最终上下文字符串：
```rust
pub trait ContextFormatter: Send + Sync {
    fn format(&self, commands: &[CommandContext]) -> String;
}
```

### `RecentCommands`
最近命令策略实现，选择最近的N个命令：
```rust
pub struct RecentCommands {
    max: usize,
}
```

### `GroupedFormatter`
按会话分组的格式化器，将当前会话的命令放在前面：
```rust
pub struct GroupedFormatter {
    current_session_id: String,
    now_ms: u64,
}
```

### `InterleavedFormatter`
按时间交错排序的格式化器，所有命令按时间顺序排列：
```rust
pub struct InterleavedFormatter {
    current_session_id: String,
    now_ms: u64,
}
```

## 关键函数说明

### `build_context()`
构建LLM查询上下文的主要函数：
```rust
pub async fn build_context(
    strategy: &dyn ContextStrategy,
    formatter: &dyn ContextFormatter,
    commands: &[CommandRecord],
    reader: &dyn StreamReader,
) -> Result<String>
```
**参数:**
- `strategy`: 上下文策略，选择要包含的命令
- `formatter`: 上下文格式化器，格式化命令为文本
- `commands`: 命令记录列表
- `reader`: 流读取器，读取命令输出

**返回:** `Result<String>` 格式化后的上下文字符串

**用途:** 协调策略选择命令、读取流数据、格式化器生成文本的完整流程

### `strip_ansi()`
从原始字节中去除ANSI转义序列（CSI和OSC）：
```rust
pub fn strip_ansi(raw: &[u8]) -> String
```
**参数:** `raw: &[u8]` 包含ANSI转义序列的原始字节
**返回:** `String` 去除ANSI转义序列后的纯文本
**用途:** 清理终端输出，移除颜色和格式控制字符

### `RecentCommands::new()`
创建最近命令策略：
```rust
pub fn new() -> Self
```
**返回:** `RecentCommands` 默认限制为10个命令的策略
**用途:** 创建选择最近N个命令的策略实例

### `GroupedFormatter::new()`
创建按会话分组的格式化器：
```rust
pub fn new(current_session_id: &str, now_ms: u64) -> Self
```
**参数:**
- `current_session_id`: 当前会话ID
- `now_ms`: 当前时间戳（毫秒）

**返回:** `GroupedFormatter` 分组格式化器实例
**用途:** 创建按会话分组显示命令的格式化器

### `InterleavedFormatter::new()`
创建按时间交错排序的格式化器：
```rust
pub fn new(current_session_id: &str, now_ms: u64) -> Self
```
**参数:**
- `current_session_id`: 当前会话ID
- `now_ms`: 当前时间戳（毫秒）

**返回:** `InterleavedFormatter` 交错排序格式化器实例
**用途:** 创建按时间顺序显示所有命令的格式化器

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
为会话分配终端标签：
```rust
pub fn assign_term_labels(
    commands: &[CommandContext],
    current_session_id: &str,
) -> HashMap<String, String>
```
**规则:**
- 当前会话ID -> "term A"
- 其他会话 -> "term B", "term C" 等（按首次出现顺序）

### `truncate_lines()`
截断输出行：
```rust
pub fn truncate_lines(text: &str, max_lines: usize, head: usize, tail: usize) -> String
```
**规则:**
- 如果总行数 ≤ max_lines: 返回所有行
- 否则: 保留头部head行 + "... (N lines omitted) ..." + 尾部tail行

## 使用示例

### 基本用法
```rust
use omnish_context::{build_context, RecentCommands, GroupedFormatter};
use omnish_store::command::CommandRecord;

// 创建策略和格式化器
let strategy = RecentCommands::new();
let formatter = GroupedFormatter::new("current-session-id", 1700000000000);
let reader = MyStreamReader::new();

// 构建上下文
let context = build_context(&strategy, &formatter, &command_records, &reader).await?;
```

### 自定义策略和格式化器
```rust
use omnish_context::{ContextStrategy, ContextFormatter, InterleavedFormatter};

// 使用交错排序格式化器
let formatter = InterleavedFormatter::new("current-session-id", 1700000000000);

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
let truncated = truncate_lines("line1\nline2\n...\nline100", 20, 10, 10);
```

## 配置常量

模块中定义了以下配置常量：

```rust
const MAX_COMMANDS: usize = 10;        // 默认最大命令数
const MAX_OUTPUT_LINES: usize = 20;    // 默认最大输出行数
const HEAD_LINES: usize = 10;          // 截断时保留的头部行数
const TAIL_LINES: usize = 10;          // 截断时保留的尾部行数
```

## 依赖关系
- `omnish-store`: 命令记录类型 (`CommandRecord`, `StreamEntry`)
- `async-trait`: 异步trait支持
- `anyhow`: 错误处理
- `std::collections::HashMap`: 会话标签映射

## 设计特点

1. **策略模式**: 通过 `ContextStrategy` trait 支持不同的命令选择策略
2. **格式化器模式**: 通过 `ContextFormatter` trait 支持不同的输出格式
3. **异步支持**: 策略选择支持异步操作
4. **ANSI清理**: 自动清理终端输出中的ANSI转义序列
5. **会话管理**: 支持多会话命令的分组和标签分配
6. **输出截断**: 智能截断长输出，保留重要信息
7. **时间格式化**: 相对时间显示，提高可读性

## 测试覆盖

模块包含完整的单元测试，覆盖：
- 策略选择逻辑
- 格式化器输出
- 工具函数行为
- 集成场景测试
- ANSI清理功能
- 时间格式化功能