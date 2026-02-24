# omnish-tracker 模块

**功能:** 命令跟踪，shell提示检测，OSC 133检测

## 模块概述

omnish-tracker 负责检测shell提示，跟踪命令边界，并处理OSC 133序列以实现可靠的命令分割。该模块是omnish系统的核心组件之一，负责从连续的终端I/O流中识别出独立的命令单元。

模块包含三个主要组件：
1. **CommandTracker**: 命令跟踪器，管理命令的生命周期和状态
2. **Osc133Detector**: OSC 133序列检测器，处理终端控制序列
3. **PromptDetector**: shell提示检测器，识别shell提示符

## 重要数据结构

### `CommandTracker`
命令跟踪器，负责跟踪当前命令状态、检测命令开始和结束、积累命令输出。

**主要字段:**
- `session_id: String`: 会话标识符
- `cwd: Option<String>`: 当前工作目录
- `detector: PromptDetector`: shell提示检测器
- `pending: Option<PendingCommand>`: 当前待处理的命令
- `next_seq: u32`: 下一个命令序列号
- `seen_first_prompt: bool`: 是否已检测到第一个提示
- `osc133_mode: bool`: 是否处于OSC 133模式

**内部结构 `PendingCommand`:**
- `seq: u32`: 命令序列号
- `started_at: u64`: 命令开始时间戳（毫秒）
- `stream_offset: u64`: 流偏移量
- `input_buf: Vec<u8>`: 输入缓冲区
- `output_lines: Vec<String>`: 输出行列表
- `entered: bool`: 是否已按Enter键（用于过滤shell回显）

### `Osc133Detector`
OSC 133序列检测器，实现字节级状态机解析OSC 133序列。

**主要字段:**
- `buf: Vec<u8>`: 缓冲区，用于存储跨数据块的序列
- `in_osc: bool`: 是否正在解析OSC序列
- `carried_len: usize`: 从之前feed()调用中携带的字节数

**枚举 `Osc133EventKind`:**
- `PromptStart`: OSC 133;A - 提示开始
- `CommandStart`: OSC 133;B - 命令开始
- `OutputStart`: OSC 133;C - 输出开始
- `CommandEnd { exit_code: i32 }`: OSC 133;D;{exit_code} - 命令结束（包含退出码）

**结构 `Osc133Event`:**
- `kind: Osc133EventKind`: 事件类型
- `start: usize`: 序列在输入中的起始字节偏移
- `end: usize`: 序列在输入中的结束字节偏移（独占）

### `PromptDetector`
shell提示检测器，使用正则表达式检测常见shell提示模式。

**主要字段:**
- `patterns: Vec<Regex>`: 提示模式正则表达式列表
- `line_buf: Vec<u8>`: 行缓冲区，用于跨数据块检测

**结构 `PromptEvent`:**
- `line_start_offset: usize`: 行起始偏移量

**默认模式:** `r"[\$#%❯]\s*$"` - 匹配以$、#、%、❯结尾的行

## 关键函数说明

### `CommandTracker::new()`
创建新的命令跟踪器。

**参数:** `session_id: String`, `cwd: Option<String>`
**返回:** `CommandTracker`
**用途:** 初始化命令跟踪器实例

### `CommandTracker::feed_input()`
处理用户输入数据。

**参数:** `data: &[u8]`, `_timestamp_ms: u64`
**返回:** `()`
**用途:** 将用户输入添加到当前待处理命令的缓冲区，检测Enter键按下

### `CommandTracker::feed_output()`
处理shell输出数据，检测命令边界。

**参数:** `data: &[u8]`, `timestamp_ms: u64`, `stream_pos: u64`
**返回:** `Vec<CommandRecord>`
**用途:**
1. 将输出添加到当前命令（如果已按Enter键）
2. 检测shell提示符
3. 在检测到提示时完成当前命令并开始新命令
4. 返回已完成的命令记录

### `CommandTracker::feed_osc133()`
处理OSC 133事件，实现精确的命令边界检测。

**参数:** `event: Osc133Event`, `timestamp_ms: u64`, `stream_pos: u64`
**返回:** `Vec<CommandRecord>`
**用途:**
- `PromptStart`: 开始新命令
- `CommandStart`: 标记命令已输入（用户按Enter）
- `OutputStart`: 开始收集输出
- `CommandEnd`: 完成命令并包含退出码

### `CommandTracker::feed_output_raw()`
在OSC 133模式下收集原始输出。

**参数:** `data: &[u8]`, `_timestamp_ms: u64`, `_stream_pos: u64`
**返回:** `()`
**用途:** 仅收集输出行，不进行提示检测

### `Osc133Detector::feed()`
输入数据到OSC 133检测器。

**参数:** `data: &[u8]`
**返回:** `Vec<Osc133Event>`
**用途:** 解析数据中的OSC 133序列，支持跨数据块解析

### `Osc133Detector::parse_osc133()`
解析OSC 133序列缓冲区。

**参数:** `buf: &[u8]`
**返回:** `Option<Osc133EventKind>`
**用途:** 识别序列类型并提取退出码

### `PromptDetector::feed()`
输入数据到提示检测器。

**参数:** `data: &[u8]`
**返回:** `Vec<PromptEvent>`
**用途:** 检测数据中的shell提示符，支持跨行检测

### `PromptDetector::is_prompt()`
检查当前行缓冲区是否包含shell提示。

**参数:** `()`
**返回:** `bool`
**用途:** 去除ANSI转义序列后使用正则表达式匹配

### `strip_ansi()`
去除ANSI转义序列。

**参数:** `data: &[u8]`
**返回:** `Vec<u8>`
**用途:** 移除CSI序列（ESC [ ...）和OSC序列（ESC ] ...），使输出可读

### `strip_osc133()`
去除OSC 133序列。

**参数:** `data: &[u8]`
**返回:** `Vec<u8>`
**用途:** 从数据中移除所有OSC 133序列

## 使用示例

### 基本命令跟踪
```rust
use omnish_tracker::CommandTracker;

let mut tracker = CommandTracker::new("session-123".to_string(), Some("/home/user".to_string()));

// 处理第一个提示
tracker.feed_output(b"user@host:~$ ", 1000, 0);

// 用户输入命令
tracker.feed_input(b"ls -la\r\n", 1001);

// 处理命令输出和下一个提示
let completed_commands = tracker.feed_output(
    b"total 0\r\nfile.txt\r\nuser@host:~$ ",
    1002,
    100
);

// 获取完成的命令记录
for cmd in completed_commands {
    println!("Command: {:?}", cmd.command_line);
    println!("Output summary: {}", cmd.output_summary);
}
```

### OSC 133模式
```rust
use omnish_tracker::{CommandTracker, Osc133Detector, Osc133Event, Osc133EventKind};

let mut tracker = CommandTracker::new("session-123".to_string(), None);
let mut osc_detector = Osc133Detector::new();

// 检测OSC 133序列
let data = b"some output\x1b]133;A\x07more output";
let osc_events = osc_detector.feed(data);

// 处理OSC事件
for event in osc_events {
    let completed = tracker.feed_osc133(event, 1000, 0);
    // 处理完成的命令
}
```

### 自定义提示模式
```rust
use omnish_tracker::PromptDetector;

// 使用自定义提示模式
let patterns = vec![
    r"[\$#]\s*$".to_string(),      // $ 或 # 结尾
    r"❯\s*$".to_string(),          // ❯ 结尾
    r"git@.*:\S+\s*$".to_string(), // Git提示符
];

let mut detector = PromptDetector::with_patterns(patterns);
let events = detector.feed(b"output\r\ngit@github.com:user/repo $ ");
```

## 依赖关系
- **omnish-store**: 提供`CommandRecord`类型，用于存储命令记录
- **regex**: 用于shell提示模式的正则表达式匹配
- **标准库**: 基础数据结构和字符串处理

## 设计特点

### 1. 双模式检测
- **正则表达式模式**: 基于shell提示模式检测命令边界
- **OSC 133模式**: 基于终端控制序列的精确命令边界检测

### 2. 智能输出处理
- **过滤shell回显**: 使用`entered`标志区分用户输入回显和实际命令输出
- **ANSI转义序列去除**: 使输出摘要对人类和LLM可读
- **输出摘要**: 保留头部和尾部行，中间省略以节省空间

### 3. 跨数据块解析
- 所有检测器都支持跨多个`feed()`调用的数据块解析
- 维护缓冲区状态以处理分割的序列

### 4. 错误恢复
- 自动丢弃无效的转义序列
- 在OSC 133模式下禁用正则表达式检测以避免冲突

## 测试覆盖

模块包含全面的单元测试，覆盖：
- 基本命令跟踪场景
- OSC 133序列检测
- 跨数据块解析
- 边界情况处理
- 实际终端数据重现的bug修复

测试文件位于各子模块的`#[cfg(test)]`部分，确保模块的可靠性和正确性。