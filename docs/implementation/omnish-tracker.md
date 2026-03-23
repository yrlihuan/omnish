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
- `started_at: u64`: 命令开始时间戳（毫秒，来自 CommandStart 事件）
- `stream_offset: u64`: 流偏移量
- `input_buf: Vec<u8>`: 输入缓冲区
- `output_lines: Vec<String>`: 输出行列表
- `entered: bool`: 是否已按Enter键（用于过滤shell回显）
- `osc_command_line: Option<String>`: 来自OSC 133;B的命令文本（`$BASH_COMMAND`，已展开别名）
- `osc_original_input: Option<String>`: 来自OSC 133;B的原始用户输入（`history 1`，保留别名）
- `osc_cwd: Option<String>`: 来自OSC 133;B的当前工作目录

### `Osc133Detector`
OSC 133序列检测器，实现字节级状态机解析OSC 133序列。

**主要字段:**
- `buf: Vec<u8>`: 缓冲区，用于存储跨数据块的序列
- `in_osc: bool`: 是否正在解析OSC序列
- `carried_len: usize`: 从之前feed()调用中携带的字节数

**枚举 `Osc133EventKind`:**
- `PromptStart`: OSC 133;A - 提示开始
- `CommandStart { command, cwd, original }`: OSC 133;B - 命令开始（携带`$BASH_COMMAND`、工作目录、`history 1`原始输入）
- `OutputStart`: OSC 133;C - 输出开始
- `CommandEnd { exit_code: i32 }`: OSC 133;D;{exit_code} - 命令结束（包含退出码）
- `ReadlineLine { content, point }`: OSC 133;RL - bash readline状态报告
- `NoReadline`: OSC 133;NO_READLINE - bash缺少readline支持的通知（用于检测无readline的bash环境）

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
- `PromptStart`: 如有待处理的已输入命令则先完成它，再开始新的 pending（不分配 seq）
- `CommandStart`: 更新 `pending.started_at` 为当前时间戳，存储`command`→`osc_command_line`、`cwd`→`osc_cwd`、`original`→`osc_original_input`；若此时 `pending` 为 `None`（即 PromptStart 丢失），则自动创建恢复性 pending 以避免命令丢失
- `OutputStart`: 开始收集输出
- `CommandEnd`: 完成命令并包含退出码，seq 在此时才分配

### 命令行解析优先级（`finalize_command`）

`finalize_command()` 在命令真正完成时被调用，此时才分配 seq 编号。这意味着空提示（Ctrl-C、直接按 Enter）不会消耗 seq，避免序号出现空洞。

确定最终 `command_line` 的优先级：

1. **`osc_original_input`**（最高）— `history 1` 原始输入，保留别名（如 `ll`）
2. **`osc_command_line`** — `$BASH_COMMAND`，已展开别名（如 `ls -la`）
3. **`extract_command_line()`**（回退）— 从原始PTY输入字节回放编辑操作重建

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

## CWD跟踪

命令记录中的`cwd`（当前工作目录）通过以下方式获得：

1. **优先：运行时CWD**（来自OSC 133 CommandStart事件或client通过ShellCwdProbe探针发送）
   - ShellCwdProbe通过读取`/proc/{shell_pid}/cwd`符号链接获得Shell的实际工作目录
   - 比环境变量更准确，避免$PWD过时或不正确的问题

2. **回退：会话CWD**（来自SessionUpdate中的shell_cwd属性或会话起始时的cwd）
   - 如果运行时CWD不可用，使用会话保存的cwd

这样既支持精确的运行时跟踪，又在信息不完整时有合理的回退策略。

## OSC 133;B 扩展格式

shell hook发送的 OSC 133;B payload格式：

```
B;<command>;cwd:<path>;orig:<original_input>
```

各字段可选，解析器按**未转义的分号**分隔各字段，识别`cwd:`和`orig:`前缀。

由于命令文本中可能包含分号（如 `for i in 1 2 3; do echo $i; done`），shell hook 会将命令内部的分号转义为 `\;`，解析器在拆分后再将 `\;` 还原为 `;`。

例如：

```
\x1b]133;B;ls -la;cwd:/home/user;orig:ll\x07
```

对应：`command = "ls -la"`, `cwd = "/home/user"`, `original = "ll"`

含分号的命令示例：

```
\x1b]133;B;for i in {1..3}\; do echo $i\; done;cwd:/home/user\x07
```

对应：`command = "for i in {1..3}; do echo $i; done"`, `cwd = "/home/user"`

## 无readline检测

当bash缺少readline支持时（例如某些最小化安装环境），shell hook会发送 `\x1b]133;NO_READLINE\x07` 序列。`Osc133Detector` 将其解析为 `NoReadline` 事件，`CommandTracker` 本身忽略此事件，由 `omnish-client` 的 `shell_input` 模块处理，以便在无readline环境下采用替代的输入处理策略。

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
- 当 `CommandStart` 到达时 `pending` 为空（如空闲期间 PromptStart 丢失），自动创建恢复性 pending，避免命令记录丢失

## 测试覆盖

模块包含全面的单元测试，覆盖：
- 基本命令跟踪场景
- OSC 133序列检测
- 跨数据块解析
- 边界情况处理
- 实际终端数据重现的bug修复

测试文件位于各子模块的`#[cfg(test)]`部分，确保模块的可靠性和正确性。