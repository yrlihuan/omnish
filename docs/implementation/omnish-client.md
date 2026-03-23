# omnish-client 模块

**功能:** 终端客户端，提供交互式shell包装和LLM集成界面

## 模块概述

omnish-client 是终端用户直接交互的客户端程序，作为PTY代理运行shell，拦截用户输入以提供LLM集成功能。主要功能包括：

1. **PTY管理**: 创建伪终端并运行用户指定的shell
2. **输入拦截**: 检测命令前缀（如`:`）进入聊天模式，支持双前缀快速恢复对话
3. **多轮聊天**: 支持多轮对话循环，包含线程管理（/resume, /thread list, /thread del）
4. **交互式界面**: 提供美观的终端界面显示聊天提示、输入回显和LLM响应，支持Widgets系统（Picker、LineEditor、ScrollView、InlineNotice、LineStatus、ChatLayout）
5. **守护进程通信**: 与omnish-daemon建立连接，发送查询和接收响应，支持协议版本检查
6. **智能完成**: 提供LLM驱动的shell命令完成建议
7. **会话管理**: 跟踪shell会话状态和命令历史
8. **命令跟踪**: 通过OSC 133协议实时跟踪命令执行、CWD（当前工作目录）和退出码
9. **Agent工具使用**: 支持工具调用的Agent循环，实时显示工具执行状态，客户端本地执行工具
10. **客户端插件系统**: 通过omnish-plugin子进程执行客户端侧工具，支持Landlock沙箱；`/lock on/off` 命令控制整个 shell 的沙箱状态；可配置沙箱放行规则绕过特定工具的沙箱
11. **自更新**: `/update`命令透明自重启，支持检测磁盘二进制变更后自动更新
12. **粘贴支持**: 括号粘贴模式、快速粘贴检测、多行粘贴折叠显示
13. **Markdown渲染**: LLM响应使用pulldown-cmark解析并渲染为ANSI终端样式

## 重要数据结构

### `InputInterceptor`
输入拦截器，负责检测命令前缀和管理聊天模式状态。

**字段:**
- `prefix: Vec<u8>` - 命令前缀字节序列（如`b":"`）
- `buffer: VecDeque<u8>` - 当前输入缓冲区
- `in_chat: bool` - 是否处于聊天模式
- `suppressed: bool` - 是否抑制拦截（非at_prompt状态时抑制，如在vim等全屏程序或子进程中）
- `guard: Box<dyn InterceptGuard>` - 拦截策略守卫
- `esc_filter: Option<EscSeqFilter>` - ESC序列过滤器

**方法:**
- `feed_byte(byte: u8) -> InterceptAction` - 处理单个输入字节
- `set_suppressed(suppressed: bool)` - 设置抑制状态
- `note_output(data: &[u8])` - 通知有shell输出（重置聊天状态）
- `finish_batch() -> Option<InterceptAction>` - 完成批次处理
- `inject_byte(byte: u8)` - 注入字节到缓冲区（用于接受完成建议）
- `current_buffer() -> Vec<u8>` - 获取当前缓冲区内容
- `is_in_chat() -> bool` - 检查是否在聊天模式
- `is_suppressed() -> bool` - 获取抑制状态（用于调试）
- `expire_prefix() -> Option<InterceptAction>` - 前缀匹配超时后触发，返回 `Chat("")` 进入新聊天

**前缀匹配与双前缀恢复 (issue #116, #261):**
- 前缀完全匹配后进入计时状态（`Buffering`），等待250ms超时或双前缀检测
- 超时后调用 `expire_prefix()` 返回 `Chat("")` 进入新聊天
- 双前缀（如`::`）在250ms内再次匹配前缀则返回 `ResumeChat`，自动恢复最近对话
- 后续输入由 `run_chat_loop` 中的 `read_chat_input` 函数处理
- 退格处理正确支持UTF-8多字节字符（issue #141）

### `InterceptAction` 枚举
输入拦截器返回的动作类型。

**变体:**
- `Buffering(Vec<u8>)` - 正在缓冲输入，不发送到PTY
- `Forward(Vec<u8>)` - 转发字节到PTY
- `Chat(String)` - 聊天消息完成（前缀匹配超时后触发，字符串为空）
- `ResumeChat` - 恢复最近对话（双前缀检测，如`::`）
- `Backspace(Vec<u8>)` - 退格操作，更新后的缓冲区
- `Cancel` - 用户按ESC取消聊天
- `Pending` - ESC序列处理中
- `Tab(Vec<u8>)` - Tab键按下，当前缓冲区

### `InterceptGuard` trait
拦截策略守卫，决定何时允许拦截。

**方法:**
- `note_input(&mut self)` - 记录用户输入
- `should_intercept(&self) -> bool` - 是否应该拦截

**实现:**
- `TimeGapGuard` - 基于时间间隔的守卫（默认）
- `AlwaysIntercept` - 总是拦截（测试用）

### `TimeGapGuard`
基于时间间隔的拦截守卫，假设用户在一段时间未输入后处于新的shell提示符。

**字段:**
- `last_input: Option<Instant>` - 最后输入时间
- `min_gap: Duration` - 最小间隔时间（配置：`shell.intercept_gap_ms`）

### `EscSeqFilter`
ESC序列过滤器，区分裸ESC键和ESC序列（如箭头键）。

**状态:**
- `EscGot` - 收到`\x1b`，等待下一字节
- `CsiParam(Vec<u8>, Vec<u8>)` - CSI序列参数收集中（序列字节 + 参数字节）
- `Paste(Vec<u8>)` - 收集粘贴内容
- `PasteEsc(Vec<u8>)` - 粘贴内容中收到ESC
- `PasteCsi(Vec<u8>, Vec<u8>)` - 粘贴内容中CSI参数收集

### `ShellCompleter`
Shell完成建议处理器，管理LLM驱动的命令完成。

**字段:**
- `last_change: Option<Instant>` - 最后输入变化时间
- `pending_seq: u64` - 待处理序列ID
- `sent_seq: u64` - 已发送序列ID
- `current_ghost: Option<String>` - 当前幽灵文本建议
- `active_requests: HashMap<u64, RequestState>` - 活跃完成请求（支持多个并发）
- `ghost_input: String` - 产生当前建议的输入
- `ghost_set_at: Option<Instant>` - 当前幽灵文本设置时间
- `sent_input: String` - 最后发送请求的输入
- `last_completion: Option<CompletionInfo>` - 最后完成信息（用于跟踪）

**方法:**
- `on_input_changed(input: &str, sequence_id: u64) -> bool` - 输入变化通知，返回true表示幽灵文本被清除
- `should_request(sequence_id: u64, current_input: &str) -> bool` - 是否应该发送请求
- `mark_sent(sequence_id: u64, input: &str)` - 标记请求已发送
- `on_response(response: &CompletionResponse, current_input: &str) -> Option<&str>` - 处理响应
- `accept() -> Option<String>` - 接受当前建议
- `clear()` - 清除建议
- `ghost() -> Option<&str>` - 获取当前建议
- `note_activity()` - 重置防抖计时器（所有输入活动都应调用，issue #100）
- `cleanup_timed_out_requests() -> usize` - 清理超时请求
- `is_ghost_expired(timeout_ms: u64) -> bool` - 检查幽灵文本是否超时
- `take_completion_summary(session_id: &str, accepted: bool, cwd: Option<String>) -> Option<CompletionSummary>` - 获取完成摘要用于追踪
- `get_debug_state() -> (usize, u64, u64, Vec<u64>)` - 获取调试状态
- `build_request(session_id: &str, input: &str, sequence_id: u64, cwd: Option<String>) -> Message` - 构建完成请求

**完成建议修复:**
- 防抖重置：所有输入活动（包括不改变序列ID的操作）都重置防抖计时器，防止逐字符触发请求（issue #100）
- isearch模式处理：通过 `in_isearch` 标志追踪Ctrl+R状态，discarding responses during isearch（issue #88）
- 过时建议丢弃：当建议与当前输入不匹配时自动丢弃（issue #113）
- 即时提示后请求：新提示符后允许立即发送完成请求
- 短前缀优先第二建议：当第一个建议是短前缀时偏好第二个（issue #95）
- `/update`后防洪：防止 `/update` 执行后因状态重置导致的完成请求洪泛（issue #224）

### `ShellInputTracker`
Shell命令行输入跟踪器，通过观察转发的字节和OSC 133状态转换跟踪当前shell命令行输入。

**生命周期:**
1. OSC 133;A/D (PromptStart/CommandEnd) → `on_prompt()`: `at_prompt = true`, `in_isearch = false`
2. 回车键 (0x0d) 在 `feed_forwarded` 中 → `at_prompt = false`
   (OSC 133;B/C 不用于 `at_prompt`，因为bash DEBUG陷阱在PS1命令替换期间触发，而不仅是在用户按回车时)
3. 在 `at_prompt` 为true时，转发的可打印字节追加到 `input`
4. 退格键 (0x7f / 0x08) 移除最后一个字符
5. Ctrl+C (0x03) / Ctrl+U (0x15) 清除输入
6. 回车键 (0x0d) 清除输入（命令提交）

**字段:**
- `input: String` - 当前输入文本
- `at_prompt: bool` - 是否在提示符处
- `sequence_id: u64` - 单调递增序列ID，每次输入变化时递增
- `changed: bool` - 自上次 `take_change()` 以来输入是否变化
- `esc_state: u8` - ESC序列状态：0=正常，1=看到ESC，2=在CSI参数中
- `cursor_at_end: bool` - 光标是否在输入末尾（默认true）
- `rl_content: Option<String>` - 最新readline报告的内容
- `in_isearch: bool` - 是否处于Ctrl+R isearch模式（issue #88）

**方法:**
- `new() -> Self` - 创建新跟踪器
- `on_prompt()` - OSC 133;A或133;D检测到时调用
- `feed_forwarded(bytes: &[u8])` - 馈送转发到PTY的字节
- `inject(text: &str)` - 追加文本到输入（例如Tab接受后写入PTY）
- `input() -> &str` - 当前输入文本
- `sequence_id() -> u64` - 当前序列ID
- `at_prompt() -> bool` - 用户是否在提示符处
- `take_change() -> Option<(&str, u64)>` - 检查输入是否变化并返回当前状态
- `set_readline(content: &str, point: Option<usize>)` - 更新readline状态和光标位置
- `cursor_at_end() -> bool` - 光标是否在输入末尾
- `enter_isearch()` - 标记进入Ctrl+R isearch模式
- `in_isearch() -> bool` - 是否处于isearch模式
- `pending_rl_report() -> bool` - 是否有待处理的readline报告
- `mark_pending_report()` - 标记readline报告为待处理
- `get_debug_info() -> (String, u64, bool, bool, u8)` - 获取调试信息

### `CursorColTracker`
光标位置跟踪器，跟踪终端输出中的光标行列位置。

**字段:**
- `col: u16` - 当前列位置
- `row: u16` - 当前行位置（用于InlineNotice渲染模式选择和/update恢复）
- `state: ColTrackState` - 解析状态
- `csi_params: Vec<u8>` - CSI参数缓冲区
- `utf8_buf: [u8; 4]` - UTF-8字符缓冲区
- `utf8_len: u8` - 已收集字节数
- `utf8_need: u8` - 需要字节数

**状态枚举 `ColTrackState`:**
- `Normal` - 正常文本
- `Esc` - ESC序列开始
- `Csi` - CSI序列中
- `Osc` - OSC序列中

**CSI序列行列跟踪:**
- `CUU (\x1b[nA)` - 光标上移n行
- `CUD (\x1b[nB)` - 光标下移n行
- `CUP (\x1b[n;mH)` - 光标绝对定位到(n,m)
- `\r` - 列归零
- `\n` - 行加一

### `DsrDetector`
DSR（Device Status Report）响应检测器，检测stdin中的光标位置报告响应 `\x1b[row;colR`。

**用途:**
- 在InlineNotice渲染前查询终端光标位置，决定使用bottom模式还是top模式
- 在 `/update` 恢复时传递光标位置

**方法:**
- `feed(byte: u8) -> Option<Option<(u16, u16)>>` - 馈送字节，完整响应时返回 `Some(Some((row, col)))`，中间字节返回 `Some(None)`，非DSR字节返回 `None`
- DSR查询通过 `send_dsr_query()` 发送 `\x1b[6n` 到终端

### `AltScreenDetector`
全屏程序检测器，检测vim/less等程序的交替屏幕切换。

**字段:**
- `active: bool` - 是否在全屏模式
- `seq_buf: Vec<u8>` - 序列匹配缓冲区

### `ChatAction` 枚举
聊天动作解析结果。

**变体:**
- `Command { result: String, redirect: Option<String>, limit: Option<OutputLimit> }` - 本地命令执行
- `LlmQuery(String)` - LLM查询
- `DaemonQuery { query: String, redirect: Option<String>, limit: Option<OutputLimit> }` - 需要守护进程数据的查询

### `OutputLimit`
命令输出限制，用于 `| head` / `| tail` 管道支持。

**字段:**
- `kind: OutputLimitKind` - 限制类型（Head 或 Tail）
- `count: usize` - 行数

## Widgets 系统

omnish-client 的交互式UI组件统一组织在 `widgets` 模块下（`crates/omnish-client/src/widgets/`）。

### 模块结构
```
widgets/
  mod.rs            - 模块导出
  line_editor.rs    - 行编辑器（光标移动、多行编辑、粘贴块）
  line_status.rs    - 临时状态显示（工具执行进度）
  inline_notice.rs  - 内联通知（重连、更新、错误消息）
  scroll_view.rs    - 可滚动内容查看器（长LLM响应）
  chat_layout.rs    - 聊天区域布局管理器
  picker.rs         - 交互式选择器（单选/多选）
  text_view.rs      - 静态文本视图
```

### LineEditor
行编辑器，提供聊天输入的完整编辑能力，替代了原来的逐字节输入处理。

**数据结构:**
- `lines: Vec<Vec<char>>` - 编辑器内容，按行存储字符
- `cursor: (usize, usize)` - 光标位置 (row, col)，以字符索引表示

**光标移动方法:**
- `move_left()` / `move_right()` - 左右移动，支持跨行
- `move_up()` / `move_down()` - 上下移动，列位置自动钳位
- `move_home()` / `move_end()` - 行首/行尾
- `move_word_left()` / `move_word_right()` - 按词移动（Alt+Left/Right）

**编辑方法:**
- `insert(ch: char)` - 在光标位置插入字符
- `delete_back() -> bool` - 退格删除（支持跨行合并），返回false表示在起始位置
- `delete_forward()` - 向前删除（支持跨行合并）
- `kill_to_start()` - 删除到行首（Ctrl+U）
- `newline()` - 插入换行（Shift+Enter / Ctrl+J）
- `insert_paste_block()` - 插入粘贴块占位符（`\u{FFFC}`字符）

**查询方法:**
- `content() -> String` - 获取完整内容（行间以`\n`连接）
- `is_empty() -> bool` - 是否为空
- `cursor() -> (usize, usize)` - 当前光标位置
- `cursor_display_col() -> usize` - 光标显示列（考虑Unicode宽度）
- `line_count() -> usize` - 行数
- `line(row: usize) -> &[char]` - 获取指定行内容
- `set_content(s: &str)` - 设置内容，光标移到末尾

**渲染方法:**
- `render(prefix: &str, ghost: &str) -> Vec<String>` - 渲染编辑器内容，第一行带前缀，后续行自动缩进，最后一行可附加灰色幽灵文本

**粘贴块支持:**
- 大量粘贴（>=10行）在编辑器中以 `\u{FFFC}` 占位符存储
- 显示为 `[pasted text #N +M lines]` 样式的折叠标记
- 提交时将 `\u{FFFC}` 替换为实际粘贴内容
- 退格删除粘贴块需要两步：先合并空行，再删除占位符

### LineStatus
临时多行状态显示组件，在当前光标位置下方渲染状态消息。

**字段:**
- `lines: usize` - 当前占用的屏幕行数
- `content: Vec<String>` - 所有累积的消息行
- `max_cols: usize` - 每行最大显示宽度（超出截断并加"..."）
- `max_lines: usize` - 最大可见行数（超出时只显示最新的N行）

**方法:**
- `new(max_cols: usize, max_lines: usize) -> Self` - 创建
- `show(text: &str) -> String` - 替换当前状态，返回ANSI序列
- `append(text: &str) -> String` - 追加新行，返回ANSI序列
- `clear() -> String` - 完全擦除，返回ANSI序列
- `is_visible() -> bool` - 是否有内容显示
- `lines_content() -> Vec<String>` - 返回当前样式化的内容行（用于ChatLayout集成）

**使用场景:**
- 工具执行时显示 "(thinking...)" 和工具调用状态
- 多工具并行执行时追加多行状态

### InlineNotice
内联通知组件，在当前光标位置上方插入一行dim样式的通知消息，不干扰光标位置。

**渲染模式（根据光标位置自动选择）:**
- **Bottom模式** (`at_bottom = true`) - 光标在屏幕底部附近时使用：Scroll Up + Insert Line
- **Top模式** (`at_bottom = false`) - 光标在屏幕顶部时使用：Insert Line + Move Down

**方法:**
- `render_at(message: &str, max_cols: usize, at_bottom: bool) -> String` - 生成ANSI序列

**使用场景:**
- 守护进程重连通知：`[omnish] reconnected`
- `/update` 更新消息
- 协议版本不匹配警告
- 启动时消息（如bash readline不可用警告）
- 错误消息

**通知队列 (`notice_queue` 模块):**
- `push(msg)` - 入队通知，立即显示或延迟（聊天模式中）
- `defer()` - 进入延迟模式（聊天模式开始时调用）
- `flush()` - 退出延迟模式，显示所有延迟的通知
- `set_cursor_row(row)` - 更新光标行位置，决定bottom/top渲染模式

### ScrollView
可滚动内容查看器，用于显示长LLM响应。

**两种模式:**
- **Compact模式**（默认）: 显示最后 `compact_height` 行，类似tail视图。新行自动滚动到底部。当内容超出时显示 `... +N lines (ctrl+o to view)` 提示。
- **Expanded模式**: 显示 `expanded_height` 行，右侧有滚动条，底部有操作提示行。用户可以用方向键/j/k上下滚动。

**字段:**
- `lines: Vec<String>` - 所有内容行
- `compact_height: usize` - compact模式可见行数
- `expanded_height: usize` - expanded模式可见行数
- `scroll_offset: usize` - expanded模式滚动偏移
- `rendered_lines: usize` - 当前占用的屏幕行数
- `mode: ViewMode` - 当前模式
- `max_cols: usize` - 最大显示宽度

**方法:**
- `push_line(line: &str) -> String` - 添加行，compact模式返回重绘序列
- `enter_browse() -> String` - 进入expanded模式
- `exit_browse() -> String` - 退出expanded模式
- `scroll_up(n: usize) -> String` / `scroll_down(n: usize) -> String` - 滚动
- `run_browse(&mut self)` - 进入browse模式并处理键盘输入直到退出（q/Esc/Ctrl-O）
- `compact_lines() -> Vec<String>` - 返回compact视图行（用于ChatLayout集成）
- `clear() -> String` - 清除所有内容

**滚动条:**
- 使用 `▐`（thumb）和 `│`（track）字符
- 位于行宽-2列处
- 自动计算thumb大小和位置

**使用场景:**
- LLM响应超过屏幕高度时自动启用
- `/resume` 恢复对话时显示历史（issue #275）
- 用户按Ctrl+O进入expanded模式浏览完整内容

### ChatLayout
聊天区域布局管理器，统一管理聊天循环中的多个widget区域。

**概念:**
ChatLayout 将聊天界面分为多个有名称的区域（Region），每个区域包含若干行。布局系统负责：
- 跟踪每个区域的高度和内容
- 当某个区域的高度变化时，自动重绘该区域及其下方所有区域
- 光标约定：光标始终停留在布局最后一行之后（row = total_height）

**数据结构:**
- `Region { id: &'static str, height: usize, content: Vec<String> }` - 区域
- `ChatLayout { regions: Vec<Region>, cols: usize }` - 布局管理器

**方法:**
- `push_region(id: &'static str)` - 添加区域
- `update(id: &str, lines: Vec<String>) -> String` - 更新区域内容，返回ANSI序列
- `hide(id: &str) -> String` - 隐藏区域（高度置0）
- `set_content(id: &str, lines: Vec<String>)` - 更新区域内容但不输出ANSI（用于与外部渲染同步）
- `redraw_all() -> String` - 从头重绘所有区域
- `cursor_to(id: &str) -> String` - 将光标定位到指定区域的最后一行
- `total_height() -> usize` - 所有区域总高度
- `region_offset(id: &str) -> usize` - 区域起始行偏移

**聊天循环中的区域划分:**
```
scroll_view  - LLM响应内容（ScrollView输出）
editor       - 输入编辑器（LineEditor渲染）
status       - 工具执行状态（LineStatus输出）
```

### Picker 选择器

omnish-client 提供了一个交互式选择器组件，用于在终端中进行单选或多选操作。选择器在终端底部渲染，通过向上推送现有内容来保留用户的视觉上下文。

#### 模块位置
`crates/omnish-client/src/widgets/picker.rs`

#### 公共API

**`pick_one(title: &str, items: &[&str]) -> Option<usize>`**
- 单选模式，返回选中项的索引（从0开始）
- 用户按ESC取消时返回None
- 用于 `/resume` 命令选择对话

**`pick_one_at(title: &str, items: &[&str], initial: usize) -> Option<usize>`**
- 单选模式，带预选索引，初始光标定位在 `initial` 项
- 用于 `/model` 命令，预选当前模型（commit 2a2e8d0）
- 初始化时 `scroll_offset` 自动居中在 `initial` 位置：`cursor.saturating_sub(vis/2).min(max_scroll)`

**`pick_many(title: &str, items: &[&str]) -> Option<Vec<usize>>`**
- 多选模式，返回选中项的索引列表（从0开始）
- 用户按ESC取消时返回None
- 用于 `/thread del` 命令删除多个对话

#### 使用场景

**已集成命令:**
- **`/resume`** (无参数) - 使用单选picker选择要恢复的对话线程（issue #157），显示所有会话的线程（issue #220）
- **`/thread del`** (无参数) - 使用多选picker选择要删除的对话线程

#### 滚动视口
- 超过10项时启用滚动视口（`MAX_VISIBLE = 10`）
- 滚动提示 `(▼ N more)` 显示在 hint 行（"ESC cancel" 之后），分隔线始终保持全宽（commit f333e28，#371）
- 光标移出视口时自动滚动
- `scroll_offset` 溢出修复（commit 81d0a6b）：`max_scroll` 使用 `items.len().saturating_sub(vis)` 计算，防止当 `initial >= items.len()` 时 `scroll_offset` 超出合法范围

#### 渲染方式

**布局特点:**
1. 通过打印N个空行将屏幕内容向上推送
2. 光标回退N行后在创建的空间中渲染组件
3. 组件包含：标题行、分隔线、选项列表、分隔线、提示行

**单选模式布局示例:**
```
Title text
──────────────────────────────────────
  [1] 5m ago  | 4 turns | What is 2+2
> [2] 1h ago  | 2 turns | 三原色          ← 高亮显示
  [3] 20h ago | 3 turns | 我的问题
──────────────────────────────────────
↑↓ move  Enter confirm  ESC cancel
```

**多选模式布局示例:**
```
Title text
──────────────────────────────────────
  [ ] [1] 5m ago  | 4 turns | What is 2+2
> [x] [2] 1h ago  | 2 turns | 三原色       ← 光标位置，已选中
  [ ] [3] 20h ago | 3 turns | 我的问题
──────────────────────────────────────
↑↓ move  Space select  Enter confirm  ESC cancel
```

#### 快捷键支持 (commit 75f71bc, #374)

picker 项目文本中的 `[X]` 模式（如 `[Y]es`、`[C]ancel`、`[N]o`）注册为快捷键：
- 按对应字母（大小写不敏感）直接选中并确认该项
- 禁用项的快捷键被忽略
- `extract_shortcut(text)` 解析 `[单字母]` 模式；`build_shortcut_map(items)` 构建映射表
- 用于 resume 不匹配提示的 `[Y]es / [N]o / [C]ancel` 选项

#### 禁用项支持 (commit bebbcc3)

- `pick_one_with_disabled(title, items, disabled)` 接受 `&[Option<DisabledIcon>]` 数组
- `DisabledIcon` 枚举：`Lock`（🔒）、`Key`（⚿）、`Forbidden`（⊘）
- 禁用项显示为 dim 样式并附加对应图标，Enter/Space/快捷键均不可选中
- 用于 `/resume` picker 中显示已被其他会话占用的线程

#### 交互键位

| 按键 | 单选模式 | 多选模式 |
|-----|---------|---------|
| ↑/↓ | 移动光标 | 移动光标 |
| Enter | 确认选择，返回当前项索引 | 确认选择，返回所有已选中项索引 |
| ESC | 取消，返回None | 取消，返回None |
| Space | 无效 | 切换当前项的选中状态 |
| 快捷键字母 | 直接确认对应项 | 直接确认对应项 |

#### 视觉效果

**高亮样式:**
- 当前光标项：`> ` 前缀 + 粗体反显（bold + reverse video）
- 非光标项：`  ` 前缀 + 普通文本
- 多选模式选中标记：`[x]`（已选）、`[ ]`（未选）

**光标隐藏 (issue #158):**
- picker组件交互期间自动隐藏终端光标（`\x1b[?25l`）
- 退出时恢复光标显示（`\x1b[?25h`）

#### 清理机制

当用户确认或取消选择后：
1. 光标移动到组件的标题行
2. 使用 `\x1b[J` 清除从光标到屏幕底部的所有内容
3. 光标回到原始位置

#### 性能优化

**增量渲染:**
- 上下移动光标时只重绘两行（旧光标行和新光标行）
- 空格切换选中状态时只重绘当前行
- 避免全屏重绘，提升响应速度

### TextView
静态文本视图组件，存储预样式化的行列表。

**字段:**
- `content: Vec<String>` - 内容行

**方法:**
- `new(lines: Vec<String>) -> Self` - 创建
- `lines() -> &[String]` - 获取内容行

## Markdown 渲染 (`markdown.rs`)

使用 `pulldown-cmark` 库解析Markdown，渲染为ANSI终端样式输出。

**支持的语法:**
- 标题：粗体青色（`\x1b[1;36m`）
- 粗体：`\x1b[1m`
- 斜体：`\x1b[3m`
- 删除线：`\x1b[9m`
- 内联代码：暗灰背景 + 黄色文字（`\x1b[48;5;236m\x1b[33m`）
- 代码块：暗灰背景 + 黄色文字
- 无序列表：`•` 符号前缀
- 有序列表：数字编号
- 引用块：dim绿色
- 链接：下划线青色
- 水平线：dim `───`
- 表格：基础 `|` 分隔支持

**输出特点:**
- 所有换行使用 `\r\n`（适配raw mode终端）
- 自动去除尾部空行

**函数:**
- `render(content: &str) -> String` - 将Markdown渲染为ANSI终端输出

## 粘贴支持

### 括号粘贴模式 (Bracketed Paste)
- 进入聊天输入时启用括号粘贴模式（`\x1b[?2004h`），退出时禁用（`\x1b[?2004l`）
- 终端将粘贴内容包裹在 `\x1b[200~` 和 `\x1b[201~` 之间
- 检测到 PasteStart/PasteEnd CSI序列后缓冲粘贴内容

### 快速粘贴检测 (Fast-paste Detection)
- 作为括号粘贴的回退方案，通过逐字节计时检测高速输入
- **向后检测**: 字节到达间隔小于1ms视为粘贴（捕获第2..N字节）
- **向前检测**: stdin缓冲区中已有更多数据视为粘贴（捕获第1字节）
- 粘贴缓冲结束时（无更多数据到达，2ms超时），最终化粘贴内容

### 多行粘贴与折叠显示
- 粘贴内容大于等于10行时，折叠为 `[pasted text #N +M lines]` 标记
- 在LineEditor中以 `\u{FFFC}`（Object Replacement Character）占位符表示
- 支持多次粘贴，每次递增编号
- 提交时将占位符替换为实际粘贴内容
- 退格删除粘贴块支持两步操作（先合并空行，再删除占位符）
- 少于10行的粘贴内容直接插入编辑器

### 不完整UTF-8缓冲
- 在写入PTY前缓冲不完整的UTF-8字符字节（issue #229），防止终端乱码

## 客户端插件系统 (`client_plugin.rs`)

### `ClientPluginManager`
通过短生命周期子进程执行客户端侧工具。每次工具调用生成一个新进程：写入JSON到stdin，从stdout读取JSON。

**字段:**
- `plugin_bin: PathBuf` - omnish-plugin二进制路径（与omnish-client同目录）

**方法:**
- `new() -> Self` - 创建，自动查找同目录下的 `omnish-plugin` 二进制
- `execute_tool(plugin_name, tool_name, input, cwd, sandboxed) -> (String, bool)` - 执行工具
  - `plugin_name`: `"builtin"` 使用omnish-plugin二进制，否则在 `~/.omnish/plugins/{name}/{name}` 查找
  - `tool_name`: 工具名称
  - `input`: JSON格式的工具输入
  - `cwd`: 可选工作目录（自动注入到input中）
  - `sandboxed`: 是否应用Landlock沙箱

**Landlock沙箱:**
- 通过 `pre_exec` 在子进程fork后exec前应用
- 使用 `omnish_plugin::apply_sandbox(data_dir, cwd)` 限制文件系统访问
- 工具只能访问自己的数据目录（`~/.omnish/data/{plugin_name}/`）和可选的CWD目录
- 特权模式工具（如write和edit）可以访问CWD进行文件写入（issue #219）
- 沙箱内部重构：`apply_sandbox` 拆分为 `apply_landlock` + `common_writable_paths`，新增 `apply_lock_sandbox`（不含插件数据目录，用于 `/lock on`）（commit c73013e）

**可配置沙箱放行规则 (commit f4a4c77, #379):**
- Snap 安装的二进制（如 glab、docker）因 `PR_SET_NO_NEW_PRIVS` 阻断 setuid 导致 Landlock 下失败
- 在 `daemon.toml` 中配置 `[sandbox_permit]` 规则，可按工具名和输入字段匹配（`starts_with`、`contains`、`equals`、`matches`）有选择地绕过沙箱
- 规则引擎在 `omnish-daemon/src/sandbox_rules.rs`，守护进程发送 `ChatToolCall` 时携带 `sandboxed` 字段

**`/lock on/off` 命令 (commit c73013e, #378):**
- `/lock on` — 使用 Landlock 沙箱重启 shell；`/lock off` — 不使用沙箱重启 shell
- 通过 `PtyProxy::respawn(pre_exec, cwd)` 在原地重启 shell 进程
- `ChatExitAction` 枚举信号主循环执行 shell 重启
- 当前锁定状态在 `/debug client` 输出中显示

**协议格式:**
- 请求: `{"name": "tool_name", "input": {...}}`
- 响应: `{"content": "结果文本", "is_error": false}`

### 客户端侧工具执行流程
1. 守护进程发送 `ChatToolCall` 消息（包含plugin_name、tool_name、input）
2. 客户端本地通过 `ClientPluginManager` 执行工具
3. 多个工具调用并行执行（`tokio::task::spawn_blocking`）
4. 结果通过 `ChatToolResult` 消息返回守护进程
5. 中间结果使用 `rpc.call()` 发送，最后一个使用 `rpc.call_stream()` 以获取新的响应流

## /update 自更新系统 (issue #217)

### 透明自重启 (`exec_update()`)

`/update` 命令实现了透明的进程自重启，在不中断shell会话的情况下更新客户端二进制。

**流程:**
1. 获取当前二进制路径（处理Linux `/proc/self/exe` 的 `" (deleted)"` 后缀）
2. 运行磁盘二进制的 `--version` 获取版本号
3. 比较运行版本和磁盘版本，相同则提示已是最新
4. macOS上对新二进制进行ad-hoc代码签名（`codesign --force --sign -`），防止SIGKILL
5. 清除PTY master fd的 `FD_CLOEXEC` 标志使其在exec后存活
6. 使用 `execvp` 替换当前进程为新二进制，传递 `--resume --fd=N --pid=N --session-id=S --cursor-col=N --cursor-row=N` 参数
7. 新进程从 `--resume` 参数恢复PTY代理和会话状态

**恢复模式 (`--resume`):**
- 新进程接收PTY master fd和子进程PID，重建 `PtyProxy`
- 保留session_id确保守护进程连接的连续性
- 恢复光标位置（`CursorColTracker`）（issue #234）
- 显示InlineNotice通知恢复成功

### 自动更新 (`auto_update`)
- 每60秒检查磁盘二进制的修改时间
- 仅在以下条件全部满足时触发：at_prompt、空闲超过60秒、不在聊天模式
- 检测到mtime变化后自动执行 `exec_update()`
- 检查后更新mtime缓存，避免重复检查（issue #223）
- `/update auto` 命令可运行时切换自动更新开关（不持久化）

## 协议版本不匹配警告 (issue #117)

连接守护进程认证时：
- `Auth` 消息携带客户端 `PROTOCOL_VERSION`
- `AuthOk` 响应携带守护进程 `protocol_version`
- 版本不匹配时立即断开连接（不再仅警告），使重连循环能以指数退避持续重试，直到守护进程升级后版本一致（commit d8c340a，#369）
- 旧行为（仅警告继续连接）已改为立即bail，确保自动重连机制能正常工作

## 多轮聊天模式 (Multi-turn Chat)

### 概述
当用户输入命令前缀（如`:`）后，客户端进入多轮聊天循环（`run_chat_loop`），支持与LLM进行多轮对话，以及执行聊天内命令。退出方式包括ESC、Ctrl-D（空输入时）、backspace退出（仅首次进入且未执行任何命令时，issue #120, #124, #127, #151）。

### 重要改进

**命令简化 (commit 48beea5, e775d88):**
- 移除了 `/new`, `/chat`, `/ask` 命令
- 用户直接输入问题即可自动创建新对话线程（懒创建）
- 简化的命令集使交互更直观

**线程创建延迟 (commit 9dfeb9c, bef24ac):**
- 进入聊天模式时不再立即发送 `ChatStart` 消息
- 线程在首条用户消息发送前才懒创建（发送 `ChatStart` → 等待 `ChatReady`）
- 这样避免了因用户直接退出聊天模式而产生空线程

**聊天模式入口Ghost Hint (commit 60fb568):**
- 进入聊天模式时（新聊天或resume），在 `> ` 提示符后方显示一行dim ghost提示
- 新聊天：显示 "type to start, /resume to continue"
- resume后有非默认模型：显示 "model for conversation: {model_name}"（来自 `ChatReady.model_name`）
- 模型名自动去除 `-YYYYMMDD` 日期后缀（`strip_date_suffix()`）
- 仅显示一次（`ghost_hint_shown` 标志控制）

**resume分隔线Ctrl+O提示 (commit 76cc3da):**
- resume显示历史后的分隔线改用 `render_separator()`（含 "ctrl+o to expand" 提示）
- 之前误用了 `render_separator_plain()`，没有提示

**双前缀快速恢复 (issue #261, #361):**
- 连续快速输入两次前缀（如`::`）在250ms内触发 `InterceptAction::ResumeChat`
- `::` 优先恢复当前会话上一次使用的线程（`last_thread_id`，通过 `handle_resume_tid` 直接恢复指定线程ID），而非总是取最新线程（commit bd6898f）
- `last_thread_id` 在所有聊天退出路径上持久保存（commit 81c84f1）
- 单次前缀在250ms超时后进入新聊天

**developer_mode 聊天触发策略 (commit 6d2794a, #393):**
- 默认（`developer_mode = false`）：命令行已有内容时 `:` 或 `::` 直接转发给 shell，仅在空命令行触发聊天
- 启用 `developer_mode = true` 后：即使命令行有内容也允许进入聊天模式
- Readline 报告（`RL;content;point`）实时刷新命令行内容状态，Ctrl+U/Ctrl+W 清空后恢复正常拦截

**线程绑定与多会话保护 (commit 7ab2968, 43004b3, f820330, #357, #367):**
- 守护进程维护 `ActiveThreads` 映射（thread_id → session_id），防止两个会话同时进入同一线程
- `try_claim_thread()` 原子检查所有权并释放之前持有的线程
- 所有恢复路径（`/resume`、`/resume N`、`ChatStart.thread_id`）均先检查该映射
- 退出聊天模式时发送 `ChatEnd` 消息，守护进程释放线程绑定，其他会话可立即进入该线程
- 尝试进入已锁定线程时显示错误，跳过技术错误消息（commit 27e811d）
- 会话结束时（`SessionEnd`）自动释放持有的线程
- 协议升级到 v8：`ChatStart.thread_id`（恢复指定线程）、`ChatReady.history/error/error_display`（结构化恢复响应）、`ChatEnd`（释放绑定）

**自动关闭空闲聊天会话 (commit 65b6b15, #360):**
- 客户端以30分钟超时轮询stdin；超时后自动退出聊天模式，显示 "(chat closed due to inactivity)"
- 守护进程后台任务每60秒清理超过30分10秒未活动的线程绑定，作为崩溃客户端的安全网
- `ChatMessage` 和 `ChatToolResult` 消息刷新线程活跃时间戳

**线程恢复 UX 改进 (commit d497b68, bd9eb7f, 82382eb, bebbcc3, 75f71bc, 344eec7, #372, #374):**
- 恢复对话时检测上次使用的 host 和 cwd，若不同则弹出提示
  - 不同主机：选择继续 `[Y]es` 或取消 `[C]ancel`
  - 相同主机、不同 cwd：选择切换到原目录 `[Y]` / 留在当前目录 `[N]` / 取消 `[C]`
- 提示转换为带快捷键的 picker（commit 75f71bc）：picker 项目中的 `[X]` 模式（如 `[Y]es`）可直接按对应字母选择
- 选择切换目录时实际在 shell 中执行 `cd /path`，并立即更新守护进程的 `shell_cwd`（commit bd9eb7f）
- 恢复时先渲染历史再显示不匹配提示（commit 436b410）
- 恢复时不注入 system-reminder（commit f12fb7d，#382）
- 发生 host/cwd 不匹配时跳过线程摘要显示（commit fb69c9f）
- 锁定线程在 picker 中显示为 dim + 🔒 图标且不可选择；进入被锁定线程时回退到 picker 选择其他线程（commit bebbcc3）

**`::` auto-resume 取消退出聊天模式 (commit eac5984, b3e8f72, #377):**
- `::` 触发的 auto-resume 在 cwd/host 不匹配提示中取消时，完全退出聊天模式而非停留在 `>` 提示符
- 使用 `is_fast_resume` 标志区分自动触发（pending_input）和手动 `/resume` 命令

**Ctrl+C 中断显示改进 (commit 82382eb, #384):**
- Ctrl+C 中断 LLM 响应后显示 "User interrupted. What should I do instead?" 作为普通响应行，替代原来的 dim "(interrupted)" 文本

**聊天模式退出改进 (issue #148, #151):**
- **自动退出** (issue #148): 检查命令（如 `/debug client`, `/context`, `/sessions`）作为首个动作执行后自动退出聊天模式，回到shell
- **backspace退出条件** (issue #151): 仅当没有执行过任何命令时，空输入按退格键才会退出聊天模式
- 这些改进使检查命令的使用更符合直觉（查看信息后立即返回shell），同时防止误触backspace退出正在进行的对话

**聊天历史持久化 (issue #149):**
- 聊天历史导航使用上下箭头键，支持连续导航
- 历史记录跨会话持久化到磁盘
- 正确处理UTF-8多字节字符

**输入编辑器集成:**
- `read_chat_input()` 使用 `LineEditor` 组件替代了原来的逐字节输入处理
- 支持光标左右移动、按词移动（Alt+Left/Right）
- 支持Home/End跳到行首/行尾
- 支持Shift+Enter / Ctrl+J插入换行（多行输入）
- 支持Delete键向前删除
- 更快的ESC检测和即时退出反馈（issue #222）

**进入聊天模式时立即更新 CWD (commit 61c1dc4, #354):**
- `ChatSession::run()` 开始时立即发送 `SessionUpdate`，包含从 `/proc/pid/cwd` 读取的最新 shell cwd
- 消除轮询延迟（最长60秒退避）导致的守护进程 cwd 陈旧问题，确保聊天上下文中 cwd 信息准确

**Markdown渲染:**
- LLM响应通过 `markdown::render()` 渲染为ANSI样式（issue #272）
- 支持标题、粗体、代码块、列表、引用等Markdown语法

**ScrollView集成 (issue #274, #275):**
- 长LLM响应自动使用ScrollView的compact模式显示尾部内容
- 用户可按Ctrl+O进入expanded模式浏览完整内容
- `/resume` 恢复对话时使用ScrollView显示对话历史

**ChatLayout统一渲染:**
- 聊天循环使用ChatLayout管理三个区域：scroll_view、editor、status
- 区域高度变化时自动协调重绘，防止内容重叠
- 编辑器重绘使用相对光标移动（issue #278），避免全屏重绘

### `ChatSession` 数据结构

`ChatSession` 封装了多轮聊天循环的全部状态，由 `run_chat_loop` 持有并驱动。

**字段:**
- `current_thread_id: Option<String>` - 当前会话线程ID，懒创建（首条消息发送时才创建，issue #130）
- `cached_thread_ids: Vec<String>` - 从 `/thread list` 缓存的线程ID列表，用于 `/resume N` 的稳定索引（issue #133, #150）
- `chat_history: VecDeque<String>` - 聊天历史记录（跨会话持久化，issue #149）
- `history_index: Option<usize>` - 历史导航索引
- `completer: GhostCompleter` - 命令补全器（用于 `/` 前缀的ghost text自动完成）
- `scroll_history: Vec<ScrollEntry>` - 可浏览的完整会话历史（Ctrl+O browse mode）
- `thinking_visible: bool` - 是否显示 "(thinking...)" 指示器
- `has_activity: bool` - 是否执行过命令（控制backspace退出和自动退出行为，issue #148, #151）
- `pending_input: Option<String>` - 进入聊天时携带的初始消息
- `client_plugins: Arc<ClientPluginManager>` - 客户端插件管理器
- `ghost_hint_shown: bool` - 入口ghost hint是否已显示
- `pending_model: Option<String>` - 待应用的模型名（新线程首条消息时随 `ChatMessage.model` 发出）
- `resumed_model: Option<String>` - 恢复的线程的非默认模型名（显示为ghost hint提示）
- `lines_printed: usize` - 已打印的终端行数（用于追踪工具区段的屏幕位置）
- `tool_section_start: Option<usize>` - 当前工具批次头部在屏幕中的起始行（用于 `redraw_tool_section()`）
- `tool_section_hist_idx: Option<usize>` - `scroll_history` 中当前工具批次开始的索引

**关键方法:**
- `new(chat_history: VecDeque<String>) -> Self` - 创建新实例
- `into_history(self) -> VecDeque<String>` - 取出聊天历史（会话结束时持久化）
- `run(rpc, session_id, proxy, initial_msg, ...) -> async` - 多轮聊天主循环
- `redraw_tool_section()` - 重新渲染工具区段（见下文）
- `handle_thread_del(trimmed, session_id, rpc)` - 处理 `/thread del` 命令
- `handle_thread_list(session_id, rpc)` - 处理 `/thread list` 命令
- `handle_resume(trimmed, session_id, rpc)` - 处理 `/resume` 命令
- `handle_model(session_id, rpc)` - 处理 `/model` 命令（模型picker选择）
- `handle_test_picker(selected_idx)` - 处理 `/test picker` 命令（集成测试用）
- `read_input(allow_backspace_exit) -> Option<String>` - 读取用户输入（使用LineEditor）

### `redraw_tool_section()` 方法

并行工具执行时，当某个工具完成后需要原地更新对应行的状态图标和输出。`redraw_tool_section()` 实现了完整的工具区段重绘。

**工作原理:**
1. 从 `tool_section_start`（已打印行数）计算需要上移多少行
2. 发送 `\x1b[{N}A` 上移光标到工具区段起始行
3. 发送 `\x1b[J` 擦除光标到屏幕底部的全部内容
4. 重新渲染 `scroll_history[tool_section_hist_idx..]` 中所有 `ToolStatus` 条目
5. 更新 `lines_printed` 为重绘后的实际行数

**工具输出超出终端宽度时的渲染修复 (commit 225b451, #386):**
- 原实现按逻辑行（`\r\n`）统计 `lines_printed`，但终端会对超宽行自动换行，导致光标数学计算偏低、遗留孤立工具头行
- 修复：使用 `display_width()` 计算每行实际占用的终端行数（逻辑行宽 / 终端列数，向上取整）
- 同时截断 `result_compact` 输出最多 3 个终端行，防止过多换行

**触发时机:**
- 流式消息中收到带 `result_compact` 的第二次 `ChatToolStatus`（工具完成）
- 并行发送中间工具结果时（`rpc.call()` 返回带 `result_compact` 的 `ChatToolStatus`）

### `ScrollEntry` 枚举

`scroll_history` 的条目类型，用于 Ctrl+O browse mode 重现会话内容。

**变体:**
- `UserInput(String)` - 用户输入文本
- `ToolStatus(ChatToolStatus)` - 工具执行状态（包含 display_name、param_desc、status_icon、result_compact、result_full）
- `LlmText(String)` - LLM中间文本（thinking/streaming text）
- `Response(String)` - LLM最终响应（Markdown格式）
- `Separator` - 响应后的分隔线
- `SystemMessage(String)` - 系统消息（如 "(interrupted)", "(resumed conversation)"）

`ToolStatus` 变体使用结构化渲染：browse mode 下调用 `render_tool_header_full()` + `render_tool_output(result_full)`，inline模式下调用 `render_tool_header()` + `render_tool_output(result_compact)`。

### `run_chat_loop()` 函数
多轮聊天主循环，接管用户输入直到退出。

**参数:**
- `rpc: &RpcClient` - RPC客户端
- `session_id: &str` - 会话ID
- `proxy: &PtyProxy` - PTY代理
- `initial_msg: Option<String>` - 初始消息（如果在前缀匹配前已有输入）
- `client_debug_fn: &dyn Fn() -> String` - 客户端调试状态生成闭包
- `chat_history: &mut VecDeque<String>` - 聊天历史
- `auto_update_enabled: &AtomicBool` - 自动更新开关
- `cursor_col: u16` - 当前光标列
- `cursor_row: u16` - 当前光标行

**流程:**
1. 创建 `ChatSession`（或由上层传入），设置 `pending_input`
2. 若无 pending_input，渲染 `> ` 提示符；首次进入时显示 ghost hint（"type to start, /resume to continue" 或非默认模型名）
3. 通过 `read_input()` 读取用户输入（使用LineEditor）
4. 处理聊天内命令（`/resume`, `/thread list`, `/thread del`, `/model`, `/context`, 等）
5. 检查命令执行后是否应该自动退出（检查类命令且作为首个动作）
6. 非命令输入作为LLM查询发送：先懒创建线程（`ChatStart` → `ChatReady`），再发送 `ChatMessage`
7. 流式处理响应：`ChatToolStatus`（工具状态）→ `ChatToolCall`（工具调用）→ `ChatResponse`（最终响应）
8. 工具调用通过 `ClientPluginManager` 并行执行，中间结果通过 `rpc.call()` 发送，最后结果通过 `rpc.call_stream()` 获取新响应流
9. 工具完成后调用 `redraw_tool_section()` 更新工具区段的状态图标
10. Markdown渲染LLM最终响应，打印分隔线（含 ctrl+o 提示），循环继续

### `read_chat_input()` 函数
在原始模式下使用LineEditor读取聊天输入。

**参数:**
- `completer: &mut GhostCompleter` - 幽灵文本完成器（用于 `/` 命令补全）
- `allow_backspace_exit: bool` - 是否允许空输入时退格退出
- `chat_history: &mut VecDeque<String>` - 聊天历史
- `history_index: &mut Option<usize>` - 历史导航索引
- `last_scroll_view: &mut Option<ScrollView>` - 上一次响应的ScrollView
- `layout: &mut ChatLayout` - 布局管理器

**退出方式:**
- `ESC` — 返回None，退出聊天（快速检测，即时反馈，issue #222）
- `Ctrl-D` — 仅在输入为空时返回None退出（issue #124）
- `Backspace` — 仅在输入为空且 `allow_backspace_exit=true` 且无粘贴块时退出（issue #120, #127）

**输入键位:**
- 方向键左/右 — 光标移动
- Alt+Left/Right — 按词移动
- Home/End — 行首/行尾
- Shift+Enter / Ctrl+J — 插入换行
- Delete — 向前删除
- Ctrl+U — 删除到行首
- Tab — 接受幽灵文本补全
- 方向键上/下 — 历史导航（连续导航支持）
- Ctrl+O — 进入ScrollView浏览模式
- Enter — 提交输入

**粘贴处理:**
- 括号粘贴模式启用/禁用
- 快速粘贴检测（向前+向后两个方向）
- 大量粘贴折叠为占位符
- 提交时替换占位符为实际内容

### 聊天内命令

**线程管理命令:**
- `/resume [N]` — 恢复对话。无参数时使用picker选择器交互式选择（issue #157），显示所有会话的线程（issue #220）；带编号时使用 `cached_thread_ids` 缓存的索引（issue #133），自动获取并显示最后一轮对话（issue #137），使用ScrollView显示历史（issue #275）
- `/thread list` — 列出所有对话线程（原 `/conversations` 命令，commit b2f5a6f, 096b094），同时缓存线程ID供 `/resume N` 使用，刷新缓存以保持索引稳定（issue #150）
- `/thread del [N]` 或 `/thread del 1,2-4,5` — 删除对话线程（原 `/conversations del`，commit 096b094）
  - 无参数时使用多选picker交互式选择要删除的线程（commit 3743aec）
  - 带单个编号时删除指定序号的线程（issue #142）
  - 支持多索引语法：逗号分隔和范围语法，如 `1,2-4,5` 删除序号1, 2, 3, 4, 5的线程（issue #156）
  - 索引按数值排序而非字典序（fix f7b4ebb）

**模型选择命令:**
- `/model` — 显示所有已配置LLM backend的picker选择器（commit 2a2e8d0），选中后切换当前线程使用的模型
  - 已有线程：发送带空 `query` 的 `ChatMessage`（仅 `model` 字段）到守护进程，返回 `Ack` 表示成功
  - 新线程（未发过消息）：保存到 `pending_model`，下一条消息时随 `ChatMessage.model` 一并发出
  - 选择结果通过 `ThreadMeta` 持久化到守护进程
  - 使用 `pick_one_at()` 预选当前模型（`selected=true` 的条目）
  - 守护进程命令：`__cmd:models [thread_id]`，返回 `models` 数组（包含 `name`, `model`, `selected` 字段）

**上下文命令:**
- `/context` — 在聊天模式中显示当前线程的对话上下文（issue #136），支持 `| head/tail` 管道（issue #144）和重定向（issue #210）

**检查命令（自动退出）:**
以下命令在聊天模式中作为首个动作执行后会自动退出聊天模式（issue #148）：
- `/debug client` — 显示客户端调试状态
- `/debug events` — 显示最近事件
- `/debug session` — 显示会话调试信息
- `/sessions` — 列出所有会话
- `/context` — 显示LLM上下文（无参数或带模板名）
- `/template` — 显示提示模板

**其他命令:**
- 通过 `handle_slash_command()` 分发到 `command::dispatch()`，支持所有主循环中的 `/` 命令
- `/help` — 显示所有可用命令
- `/tasks` — 查看或管理定时任务
- `/update` — 透明自重启到磁盘最新版本（issue #217）
- `/update auto` — 切换运行时自动更新开关（不持久化）
- `/test picker [N]` — 隐藏测试命令（不在 `/help` 中显示），使用20个虚拟条目测试picker组件；`N` 为初始选中索引（commit 5df1e1b）

### Ctrl-C 中断 (issue #123, #241)
聊天等待LLM响应或工具执行时，用户可按Ctrl-C中断：
- 使用 `tokio::select!` 竞赛RPC调用和 `wait_for_ctrl_c()` 阻塞任务
- `wait_for_ctrl_c()` 在 `spawn_blocking` 中运行，使用 `poll` 以100ms超时监控stdin
- 中断后发送 `ChatInterrupt` 消息到守护进程记录中断事件
- 支持中断Agent工具调用循环（issue #241），清除状态并显示 "(interrupted)"

### 守护进程JSON响应解析
守护进程命令响应使用JSON格式（issue #134），包含 `display` 字段用于显示和可选的结构化数据字段：
- `parse_cmd_response(content: &str) -> Option<serde_json::Value>` - 解析JSON响应
- `cmd_display_str(json: &serde_json::Value) -> String` - 提取 `display` 字段作为显示文本
- `thread_ids` 数组字段 - 用于缓存线程ID供 `/resume N` 使用
- `thread_id` 字段 - 恢复的线程ID
- `deleted_thread_id` 字段 - 已删除的线程ID

### 线程ID映射稳定性 (issue #150)

**问题:**
在删除对话线程后，缓存的 `cached_thread_ids` 与实际线程列表不同步，导致序号指向错误的线程。

**解决方案:**
- `/thread list` 命令执行后自动刷新 `cached_thread_ids` 缓存（commit b2d6a42）
- 连续删除操作之间的 `cached_thread_ids` 保持稳定，不自动刷新
- 用户需要显式运行 `/thread list` 来更新缓存并查看最新序号
- 这种设计使得批量删除操作（如 `1,2-4,5`）的序号保持一致

**实现细节:**
- `/thread del` 成功后不刷新缓存，保持删除前的映射
- `/thread list` 成功后刷新缓存，同步最新线程列表
- `/resume N` 使用缓存的 `cached_thread_ids[N-1]` 获取thread_id
- 如果索引超出范围，提示用户运行 `/thread list` 更新

## Probe 系统

omnish-client 实现了 Probe  trait 机制，用于收集会话相关的系统信息。Probe 是一种可插拔的数据收集器，可以定期获取客户端和 shell 进程的状态信息。

### `Probe` trait
所有 Probe 实现的基础 trait。

**方法:**
- `key(&self) -> &str` - 返回 Probe 的唯一标识键
- `collect(&self) -> Option<String>` - 收集并返回探测数据，返回 None 表示探测失败

### `ProbeSet`
Probe 集合容器，管理多个 Probe 实例。

**方法:**
- `new() -> Self` - 创建新的 Probe 集合
- `add(&mut self, probe: Box<dyn Probe>)` - 添加一个 Probe
- `collect_all(&self) -> HashMap<String, String>` - 执行所有 Probe 并返回结果映射

### 内置 Probe 实现

**静态 Probe (会话开始时收集):**
- `ShellProbe` - 获取当前 shell 路径（环境变量 `SHELL`）
- `PidProbe` - 获取子进程 PID
- `TtyProbe` - 获取终端设备路径（环境变量 `TTY`）
- `CwdProbe` - 获取客户端启动时的工作目录
- `HostnameProbe` - 获取主机名
- `PlatformProbe` - 获取客户端平台（如 "linux"、"macos"）（commit d83b63b，#402）
- `OsVersionProbe` - 获取客户端操作系统版本（commit d83b63b，#402）

**动态 Probe (定期轮询收集):**
- `ShellCwdProbe(pid: u32)` - 获取 shell 进程当前工作目录
  - **Linux**: 读取 `/proc/{pid}/cwd` 符号链接
  - **macOS**: 使用 `lsof -p PID -a -d cwd` 获取
  - **其他平台**: 返回 None
  - 用于实时跟踪 shell 的实际工作目录
- `ChildProcessProbe(pid: u32)` - 获取 shell 的最新子进程信息
  - **Linux**: 读取 `/proc/{pid}/task/{pid}/children` 获取子进程 PID 列表，取最后一个，读取其 `/proc/{pid}/comm` 获取进程名
  - **macOS**: 返回空字符串（基础支持，完整实现需要系统框架）
  - **其他平台**: 返回空字符串
  - 返回格式: `"name:pid"` 的字符串（如 `"vim:12345"`）
  - 如果没有子进程则返回空字符串
  - 主要用于 tmux 窗口标题更新

### 默认 Probe 集合

**会话探测 (`default_session_probes`)**: 静态 Probe 集合，在会话开始时收集一次
- `ShellProbe` - 使用的 shell（环境变量 `SHELL`）
- `PidProbe` - shell 子进程 PID
- `TtyProbe` - 终端设备路径（环境变量 `TTY`）
- `CwdProbe` - 客户端启动时的工作目录
- `HostnameProbe` - 主机名（通过 `gethostname()` 系统调用获取）
- `PlatformProbe` - 客户端平台（commit d83b63b）
- `OsVersionProbe` - 客户端操作系统版本（commit d83b63b）

**注意 (commit d83b63b, #402):** `system-reminder` 中的平台/OS 信息现在来自客户端 Probe 上报的 `session_attrs`，而非守护进程自身运行环境，确保远程连接等场景下信息准确。

**轮询探测 (`default_polling_probes`)**: 动态 Probe 集合，用于定期轮询
- `HostnameProbe` - 主机名（定期轮询以检测集群环境中的变化）
- `ShellCwdProbe` - 当前 shell 进程工作目录
- `ChildProcessProbe` - 当前子进程信息（进程名:PID 格式）

## 关键函数说明

### 主事件循环 (`main.rs`)
客户端的主I/O事件循环，使用`poll`监控stdin和PTY master。

**主要流程:**
1. **初始化**: 加载配置，创建PTY，连接守护进程，进入原始模式
2. **非TTY检测**: 如果stdin不是终端（如rsync/SSH管道），直接exec底层shell（issue #193）
3. **信号处理**: 设置SIGWINCH处理器同步窗口大小
4. **自动更新检查**: 每60秒检查磁盘二进制mtime变化
5. **前缀匹配计时**: 前缀匹配后等待250ms，区分单前缀（新聊天）和双前缀（恢复对话）
6. **事件循环**:
   - 监控stdin（用户输入）和PTY master（shell输出）
   - 过滤stdin中的DSR响应（`\x1b[row;colR`），更新光标位置
   - 处理用户输入字节，通过`InputInterceptor`检测命令前缀
   - 前缀匹配超时后进入 `run_chat_loop` 多轮聊天循环
   - 双前缀检测到后进入 `run_chat_loop`（initial_msg = `/resume 1`）
   - 处理shell输出，跟踪光标位置，检测全屏程序
   - 发送I/O数据到守护进程（节流）
   - 处理OSC 133事件进行命令跟踪和CWD（当前工作目录）跟踪
   - 使用`ShellInputTracker`跟踪shell命令行输入
   - 检查并发送完成请求
   - 处理完成响应
   - 记录输入延迟事件（超过50ms时，issue #106）
   - 检测bash无readline支持并警告（issue #226）

### `send_or_buffer()`
发送消息到守护进程，失败时缓冲可重试的消息。

**参数:**
- `rpc: &RpcClient` - RPC客户端
- `msg: Message` - 要发送的消息
- `buffer: &MessageBuffer` - 消息缓冲区

**逻辑:**
- 如果发送失败且消息类型可缓冲（`IoData`、`CommandComplete` 或 `SessionUpdate`），则加入缓冲区
- 缓冲区有大小限制（`MAX_BUFFER_SIZE = 10_000`），满时丢弃最旧消息

### SessionUpdate 消息

`SessionUpdate` 消息用于实时更新会话状态信息，携带来自 Polling 探针的数据变化。

**字段:**
- `session_id: String` - 会话ID
- `timestamp_ms: u64` - 消息发送时间戳（毫秒）
- `attrs: HashMap<String, String>` - 变化的属性映射
  - 仅包含自上次探测以来发生变化的属性
  - 常见属性: `hostname`, `shell_cwd`, `child_process`

**特性:**
- 差异更新：仅发送变化的字段，减少网络流量
- 可缓冲消息：发送失败时自动加入重试缓冲区
- 由轮询任务异步生成和发送

### Polling 机制

客户端启动后会在后台运行一个定期探测任务，用于持续收集 shell 进程的状态信息。

**工作机制:**
1. **启动时机**: 与守护进程建立连接后自动启动
2. **探测间隔**: 使用渐进式间隔策略
   - 默认间隔序列: 1, 2, 4, 8, 15, 30, 60 秒
   - 最后维持 60 秒间隔
   - 命令开始时（OSC 133 CommandStart 事件）重置为 1 秒间隔
3. **数据来源**: 使用 `default_polling_probes` 收集以下内容
   - `HostnameProbe` - 主机名（定期轮询，可能在集群环境中变化）
   - `ShellCwdProbe(pid)` - 读取 `/proc/{pid}/cwd` 获取 shell 进程当前工作目录
   - `ChildProcessProbe(pid)` - 获取 shell 的最新子进程信息
4. **差异更新**: 维护上一次探测结果的副本，仅当数值发生变化时才更新
5. **消息发送**: 通过 `SessionUpdate` 消息将变化的数据发送到守护进程
   - `SessionUpdate` 包含: `session_id`, `timestamp_ms`, `attrs` (HashMap)
   - `attrs` 仅包含已变化的字段，减少网络传输

**平台支持:**
- **Linux**: `ShellCwdProbe` 通过 `/proc/{pid}/cwd` 符号链接读取，`ChildProcessProbe` 通过 `/proc/{pid}/task/{pid}/children` 读取
- **macOS**: `ShellCwdProbe` 使用 `lsof` 获取，`ChildProcessProbe` 返回空字符串（基础支持）
- **其他平台**: 两个探针均返回 None/空字符串

**tmux 窗口标题更新:**
- 当 `child_process` 探测值发生变化时，自动更新 tmux 窗口标题
- 提取进程名（不含 PID）作为窗口标题
- 使用 tmux OSC 序列 `\x1b_k{name}\x1b\\` 设置窗口名称

**代码位置**: `main.rs` 中的异步任务块和轮询任务

## 事件日志 (Event Log)

omnish-client 包含客户端事件日志系统，用于调试和监控异步事件流。

### `event_log` 模块
全局事件环形缓冲区，容量200条，使用`LazyLock<Mutex<EventLog>>`实现跨模块访问。

**函数:**
- `push(event: impl Display)` - 记录事件（自动添加时间戳前缀）
- `recent(n: usize) -> Vec<String>` - 获取最近n条事件

**记录的事件类型:**
- OSC 133状态转换: `PromptStart`, `CommandStart(cmd, orig)`, `CommandEnd(exit_code)`, `OutputStart`
- Readline异步事件: `readline request (input key)`, `readline request (completion)`, `readline response`
- 补全流程: `completion request`, `completion response`, `completion accept`
- 聊天交互: `chat mode enter`, `chat mode enter (timeout)`, `chat mode resume (double-prefix, gap Nms)`
- 输入延迟: `input lag Nms (Nbytes)`（处理超过50ms时记录，issue #106）
- isearch模式: `ctrl+r (isearch mode)`
- Readline触发跳过: `readline trigger skipped (seq mismatch: cur=N resp=N)`

### `connect_daemon()`
连接守护进程，支持优雅降级（守护进程不可用时进入直通模式）。

**参数:**
- `daemon_addr: &str` - 守护进程地址
- `session_id: &str` - 会话ID
- `parent_session_id: Option<String>` - 父会话ID
- `child_pid: u32` - 子进程PID
- `buffer: MessageBuffer` - 消息缓冲区

**流程:**
1. 加载认证令牌（`~/.omnish/auth_token`）
2. 尝试连接守护进程
3. 发送`Auth`消息进行认证（包含`PROTOCOL_VERSION`）
4. 检查`AuthOk`响应中的协议版本，不匹配时显示警告
5. 发送`SessionStart`消息
6. 重放缓冲的消息
7. 连接失败时进入直通模式，打印警告

### `handle_command_result()`
处理命令结果，支持重定向到文件。

**参数:**
- `content: &str` - 命令结果内容
- `redirect: Option<&str>` - 重定向文件路径
- `proxy: &PtyProxy` - PTY代理

### `send_daemon_query()`
发送查询到守护进程并显示结果。

**参数:**
- `query: &str` - 查询文本
- `session_id: &str` - 会话ID
- `rpc: &RpcClient` - RPC客户端
- `proxy: &PtyProxy` - PTY代理
- `redirect: Option<&str>` - 重定向文件路径
- `show_thinking: bool` - 是否显示思考状态

### `handle_slash_command()`
在聊天模式中处理 `/` 命令。通过 `command::dispatch()` 分发，特殊处理 `/debug client`（使用闭包获取本地客户端状态，issue #115）。

**参数:**
- `trimmed: &str` - 去空白后的命令文本
- `session_id: &str` - 会话ID
- `rpc: &RpcClient` - RPC客户端
- `proxy: &PtyProxy` - PTY代理
- `client_debug_fn: &dyn Fn() -> String` - 客户端调试状态生成闭包

**返回:** `bool` - true表示命令已处理，false表示未识别的 `/` 命令（传递给LLM）

### `debug_client_state()`
生成客户端调试状态文本，用于 `/debug client` 命令（issue #115, #135, #146）。

**输出内容:**
- 版本信息
- Shell CWD（通过 `/proc/{pid}/cwd` 实时读取，issue #146）
- bash readline状态
- Shell Input Tracker状态：at_prompt, input, sequence_id, pending_rl_report, esc_state, readline_report
- Input Interceptor状态：in_chat, suppressed
- Shell Completer状态：active_requests, sent_seq, pending_seq, active_request_ids, should_request, ghost
- Daemon Connection状态
- OSC 133 Detector状态

**格式改进（issue #135）:**
- 移除了 `=== Client Debug State ===` 头部和 `=== End Debug State ===` 尾部

### 显示函数 (`display.rs`)
纯函数，生成ANSI终端输出字符串。

**函数列表:**
- `render_separator(cols: u16) -> String` - 渲染分隔线（含 "ctrl+o to expand" 提示，右侧2个短划线）
- `render_separator_plain(cols: u16) -> String` - 渲染纯分隔线（无提示，仅 `─` 重复）
- `render_chat_prompt() -> String` - 渲染聊天模式内的输入提示（`> `），用于多轮聊天循环
- `render_dismiss() -> String` - 清除聊天界面
- `render_input_echo(user_input: &[u8]) -> String` - 渲染输入回显
- `render_response(content: &str) -> String` - 渲染LLM响应（Markdown → ANSI）
- `render_error(msg: &str) -> String` - 渲染错误消息（红色 `[omnish] ...`）
- `render_ghost_text(ghost: &str) -> String` - 渲染幽灵文本建议（dim灰色，save/restore光标）
- `render_chat_history(last_exchange: Option<&(String, String)>, earlier_count: u32) -> String` - 渲染聊天历史（用于恢复对话时显示上下文）
- `render_tool_header(icon: &StatusIcon, display_name: &str, param_desc: &str, max_cols: usize) -> String` - 渲染工具状态头行（inline模式，param_desc截断到可用宽度）
- `render_tool_header_full(icon: &StatusIcon, display_name: &str, param_desc: &str) -> String` - 渲染工具状态头行（browse模式，param_desc不截断）
- `render_tool_output(lines: &[String]) -> Vec<String>` - 渲染工具输出行（`⎿` gutter格式，dim样式）
- `truncate_cols(s: &str, max_cols: usize) -> String` - CJK感知截断（全角字符占2列，超出用 `…`）
- `display_width(s: &str) -> usize` - 计算字符串显示宽度（剥离ANSI序列，CJK全角算2列）

**工具状态显示格式:**
```
● ToolName(param desc truncated...)    ← render_tool_header（inline，running/done/error）
  ⎿  result line 1                     ← render_tool_output 第一行
     result line 2                     ← render_tool_output 后续行
```
- 状态图标：`●`（白色=Running，绿色=Success，红色=Error）
- `display_name` 粗体，`param_desc` dim括号内，truncate到终端宽度

### 命令分发 (`command.rs`)
解析聊天消息中的命令，使用统一的命令注册表管理所有聊天命令和完成建议。

**命令注册表:**
- `COMMANDS`: 静态命令数组，包含所有支持的聊天命令
- `CommandEntry`: 命令条目，包含命令路径、类型（本地或守护进程）和帮助文本
- `CommandKind::Local`: 客户端本地处理的命令
- `CommandKind::Daemon`: 转发到守护进程的命令（格式：`__cmd:{key}`）
- `CHAT_ONLY_COMMANDS`: 聊天模式专用命令列表（仅 `/resume`），不在注册表中但包含在自动完成中

**函数:**
- `dispatch(msg: &str) -> ChatAction` - 分发聊天消息，查找最长匹配命令路径
- `parse_redirect(input: &str) -> (&str, Option<&str>)` - 解析重定向后缀
- `parse_limit(input: &str) -> (&str, Option<OutputLimit>)` - 解析 `| head` / `| tail` 管道后缀
- `parse_limit_pub(input: &str) -> (&str, Option<OutputLimit>)` - `parse_limit` 的公开包装（用于 `main.rs` 中聊天模式的 `/context`）
- `apply_limit(text: &str, limit: &OutputLimit) -> String` - 对输出文本应用行数限制
- `completable_commands() -> Vec<String>` - 返回所有可完成命令路径，用于幽灵文本建议（包含聊天专用命令）

**支持命令:**
- `/help` - 显示所有可用命令
- `/context [template]` - 获取LLM上下文（转发到守护进程，commit 5a0f0f9）；在聊天模式中显示当前线程的对话上下文
- `/template <name>` - 显示LLM提示模板（转发到守护进程，commit 5a0f0f9，显示实际工具定义）
- `/debug` - 显示调试子命令用法
- `/debug events [num]` - 显示最近的客户端事件（默认20条）
- `/debug client` - 显示客户端调试状态（通过闭包在客户端本地生成，issue #115），支持重定向和管道限制（issue #239）；新增显示 Landlock 锁定状态
- `/debug session` - 显示当前会话调试信息（转发到守护进程）
- `/debug commands [N]` - 显示最近 N 条 shell 命令历史（默认30条，转发到守护进程，commit 27d19a2）
- `/debug command <seq>` - 显示指定序号命令的完整详情和输出（转发到守护进程，commit 35542da）
- `/sessions` - 列出所有会话（转发到守护进程）
- `/thread list` - 列出所有对话线程（转发到守护进程，映射到 `__cmd:conversations`）
- `/thread del` - 删除对话线程（转发到守护进程，映射到 `__cmd:conversations del`）
- `/tasks [disable <name>]` - 查看或管理定时任务（转发到守护进程）
- `/update` - 透明自重启到磁盘最新版本（issue #217）
- `/update auto` - 切换运行时自动更新开关（不持久化）
- `/lock on` - 使用 Landlock 文件系统沙箱重启 shell（限制写入 /tmp、/dev/null、cwd、git repo根目录）
- `/lock off` - 不使用沙箱重启 shell（移除 Landlock 限制）
- `> file.txt` - 重定向输出到文件（后缀支持）
- `| head [-n] [N]` / `| tail [-n] [N]` - 限制输出行数（默认10行），支持 `-nN` 紧凑语法

**聊天模式专用命令（`CHAT_ONLY_COMMANDS`）:**
- `/resume [N]` - 恢复对话（无参数时使用picker选择，带编号时使用缓存索引）
- `/lock on|off` - Landlock 沙箱开关（重启 shell）

### Agent工具调用循环 (commit 5f439c8)

客户端支持Agent模式的工具调用循环，在LLM响应需要调用工具时自动执行工具并将结果反馈给LLM。

**流程:**
1. 用户发送查询到LLM
2. LLM响应包含工具调用请求（`ChatToolCall`消息）
3. 客户端收集批次中的所有 `ChatToolCall` 消息
4. 通过 `ClientPluginManager` 并行执行所有工具（issue #248）
5. 中间结果通过 `rpc.call()` 发送回守护进程
6. 最后一个结果通过 `rpc.call_stream()` 发送，获取新的响应流
7. LLM基于工具结果生成最终响应或发起新的工具调用
8. 循环继续，直到LLM不再请求工具调用

**消息类型:**
- `ChatToolCall` - 工具调用请求消息
  - `plugin_name`: 插件名称（"builtin"或外部插件目录名）
  - `tool_name`: 工具名称
  - `input`: JSON格式的工具输入
  - `sandboxed`: 是否应用Landlock沙箱
- `ChatToolStatus` - 流式工具执行状态消息
  - 通过LineStatus追加显示工具名称和状态
- `ChatToolResult` - 工具执行结果
  - `content`: 结果文本
  - `is_error`: 是否为错误

**工具定义:**
- `CommandQueryTool` - 查询命令历史和上下文的工具（daemon实现）
- `Read` - 文件读取工具（issue #214）
- `Edit` - 精确字符串替换工具（issue #216）
- `Bash` - 命令执行工具，CWD设置为shell当前目录
- 支持外部插件工具（plugin系统）
- 特权模式（privileged）工具可以写入CWD（issue #219）

**并行工具状态渲染（commit 81a9475, 8ae0126）:**
- 工具开始执行时（第一次 `ChatToolStatus`，无 `result_compact`）：记录 `tool_section_start` 和 `tool_section_hist_idx`，追加显示工具头行
- 工具完成时（第二次 `ChatToolStatus`，有 `result_compact`）：更新 `scroll_history` 中对应条目，调用 `redraw_tool_section()` 整体重绘工具区段
- 中间结果（`rpc.call()` 返回的 `ChatToolStatus`）：同样更新条目并触发 `redraw_tool_section()`
- 效果：多工具并行时，每个工具完成后状态图标原地从 `●`(running) 变为 `●`(success/error)，输出出现在各自头行下方

**用户体验:**
- 工具执行时显示实时状态（`●` 图标 + 工具名 + 参数描述）
- 多工具并行执行，所有工具集中在一个区段内同步更新
- 工具完成后继续显示LLM的最终响应（通过Markdown渲染）
- 用户无需手动触发工具调用，全自动化
- 支持Ctrl-C中断工具执行循环（issue #241）

## 使用示例

### 启动客户端
```bash
# 直接运行
cargo run -p omnish-client

# 或编译后运行
cargo build --release
./target/release/omnish-client
```

### 交互使用
1. **正常shell使用**: 输入命令如`ls -la`直接执行
2. **进入聊天模式**: 输入配置的前缀（默认`:`）
   - 等待250ms后进入新聊天
   - 显示`> `提示符等待输入（使用LineEditor）
   - 双前缀（如`::`）快速恢复上次使用的线程（`last_thread_id`）；若无记录则恢复最新线程
   - 默认模式下命令行有内容时 `:` 直接转发给 shell；`developer_mode = true` 时允许有内容也触发聊天
3. **多轮对话**:
   - 直接输入问题即可开始对话（自动懒创建线程）
   - 支持多行输入（Shift+Enter / Ctrl+J）
   - 支持光标移动和编辑（方向键、Home/End、Alt+方向键）
   - 支持粘贴检测和大文本折叠
   - 输入多个问题进行多轮对话
   - `/resume` 使用picker选择器恢复对话（显示所有会话线程）
   - `/thread list` 列出所有对话
   - `/resume N` 恢复第N个对话（使用缓存索引）
   - `/thread del` 使用多选picker选择要删除的对话
   - `/thread del N` 删除第N个对话
   - `/thread del 1,2-4,5` 删除多个对话（支持范围语法）
   - `Ctrl-C` 中断等待中的LLM响应或工具执行
   - 上下箭头键浏览聊天历史
   - LLM响应以Markdown格式渲染
   - 长响应使用ScrollView，Ctrl+O浏览完整内容
4. **使用聊天命令**: 在聊天模式下，支持以下命令：
   - `/context` - 查看当前线程的对话上下文，支持 `| head 5` 或 `| tail 10`
   - `/debug client` - 查看客户端调试状态（包含shell CWD、输入跟踪器、补全器、Landlock锁定状态等）
   - `/debug events` - 查看最近事件日志
   - `/debug commands [N]` - 查看最近 N 条 shell 命令历史
   - `/debug command <seq>` - 查看指定序号命令的完整详情和输出
   - `/template <name>` - 显示LLM提示模板（包含实际工具定义）
   - `/sessions` - 列出所有活动会话
   - `/lock on` - 使用 Landlock 沙箱重启 shell
   - `/lock off` - 不使用沙箱重启 shell
   - `/update` - 更新到磁盘最新版本
   - `/update auto` - 切换自动更新
   - `> file.txt` - 重定向输出到文件（如`/context > context.txt`）

   **检查命令自动退出**:
   - 以上检查命令（`/debug`, `/context`, `/template`, `/sessions`）作为首个动作执行后会自动退出聊天模式
   - 使检查命令的工作流更符合直觉（查看信息后立即返回shell）

   **上下文输出特点**:
   - 显示命令执行的完整路径（CWD）
   - 失败命令显示`[FAILED: exit_code]`标签
   - 命令按会话分组，当前会话显示在最后
   - 在聊天模式中，`/context` 显示当前线程的对话历史
   - 包含最近命令列表（用于Agent工具调用）
5. **退出聊天模式**:
   - `ESC` — 立即退出（快速检测）
   - `Ctrl-D` — 输入为空时退出
   - `Backspace` — 首次进入且未执行任何命令时，空输入退格退出（防止误触）
   - 检查命令自动退出（作为首个动作时）
6. **Picker选择器交互**: 在使用 `/resume` 或 `/thread del` 无参数时
   - 使用方向键 ↑↓ 移动光标
   - 多选模式下按空格键切换选中状态
   - 超过10项时自动滚动
   - 按Enter确认选择
   - 按ESC取消
   - 光标在交互期间自动隐藏
7. **接受完成建议**: 在shell提示符下，LLM会提供命令完成建议
   - 显示为灰色幽灵文本
   - 按Tab接受建议
   - 光标不在行末时自动抑制补全建议（cursor_at_end检查）
   - 配置中`completion_enabled`为false时完全禁用补全
   - isearch模式（Ctrl+R）中自动丢弃完成响应
   - bash无readline支持时自动禁用并警告
8. **Agent工具调用**: LLM可以自动调用工具获取信息
   - 工具执行时显示状态（通过LineStatus追加显示）
   - 多工具并行执行
   - 工具结果自动反馈给LLM
   - 支持文件读取（Read）、编辑（Edit）、命令执行（Bash）等工具
   - 客户端本地执行，支持Landlock沙箱
   - 用户无需手动干预

### 配置文件示例
```toml
# ~/.omnish/client.toml
[shell]
command = "/bin/bash"
command_prefix = ":"
intercept_gap_ms = 1000

daemon_addr = "~/.omnish/omnish.sock"
auto_update = true
```

### 环境变量
- `OMNISH_SOCKET`: 守护进程socket路径（覆盖配置）
- `OMNISH_SESSION_ID`: 父会话ID（用于嵌套omnish检测）
- `SHELL`: 使用的shell命令（覆盖配置）

## 依赖关系

### 内部依赖
- `omnish-common`: 配置加载、版本号
- `omnish-pty`: PTY管理
- `omnish-transport`: RPC通信
- `omnish-protocol`: 消息协议（包含 `PROTOCOL_VERSION`）
- `omnish-tracker`: 命令跟踪
- `omnish-llm`: 模板名称和模板内容（用于 `/template` 和 `/context` 命令补全）
- `omnish-plugin`: 插件沙箱（`apply_sandbox` 函数）

### 外部依赖
- `tokio`: 异步运行时
- `nix`: 系统调用（原始模式、信号处理）
- `libc`: 低级系统接口
- `unicode-width`: Unicode字符宽度计算
- `uuid`: 会话ID生成
- `vt100`: 终端解析（测试用）
- `serde_json`: 守护进程JSON响应解析
- `pulldown-cmark`: Markdown解析
- `regex-lite`: ANSI序列剥离（测试用）

## 架构设计

### I/O处理
客户端使用同步`poll`进行I/O多路复用，而不是异步I/O，因为：
1. **简单性**: 原始模式下的终端I/O更适合同步处理
2. **控制流**: 输入拦截需要逐字节处理
3. **兼容性**: 避免异步运行时与shell子进程的复杂交互

### 输入拦截策略
使用`TimeGapGuard`作为默认拦截策略：
- 记录最后输入时间
- 仅在超过`intercept_gap_ms`间隔后才尝试拦截
- 防止在命令中间误触发聊天模式

**双前缀检测:**
- 前缀完全匹配后进入250ms计时状态
- 计时期间若再次匹配前缀，返回 `ResumeChat` 恢复对话
- 计时超时后调用 `expire_prefix()` 返回 `Chat("")` 进入新聊天
- 计时期间若有非前缀输入，取消计时，恢复输入到shell

### 聊天模式架构
聊天模式分为两层：
1. **入口层（主循环）**: `InputInterceptor` 检测前缀匹配后进入计时状态，超时返回 `Chat("")` 或双前缀返回 `ResumeChat`，触发进入 `run_chat_loop`
2. **聊天层（`run_chat_loop`）**: 使用ChatLayout管理布局，LineEditor处理输入，ScrollView显示响应，LineStatus显示工具状态

**ChatLayout区域管理:**
- `scroll_view` 区域：显示LLM响应（Markdown渲染 + ScrollView）
- `editor` 区域：显示输入编辑器（LineEditor渲染）
- `status` 区域：显示工具执行状态（LineStatus内容）
- 区域高度变化时自动协调重绘，编辑器使用相对光标移动避免闪烁

这种分离使得：
- 拦截器保持简单（仅负责前缀检测和双前缀识别）
- 聊天输入处理可以独立优化（如LineEditor光标编辑、粘贴块支持）
- 退出行为可以按阶段控制（如backspace只在未发送消息时允许退出）
- 各Widget组件独立渲染，通过ChatLayout协调

### OSC 133协议和CWD跟踪
通过shell hook和OSC 133终端控制序列实现命令跟踪和CWD（当前工作目录）跟踪：

**Shell Hook机制:**
- 安装Bash shell hook，通过`PROMPT_COMMAND`和`DEBUG` trap集成
- 发送OSC 133序列：`B;command_text;cwd:/path;orig:original_input`（命令开始，包含`$BASH_COMMAND`、工作目录、`history 1`原始输入）、`D;exit_code`（命令结束）、`A`（提示开始）、`C`（输出开始）
- `RL;content;point` - readline状态报告（`$READLINE_LINE`和`$READLINE_POINT`）
- 使用复合赋值`__omnish_last_ec=$? __omnish_in_precmd=1`立即捕获退出码，避免被`PROMPT_COMMAND`中的其他命令覆盖
- 对命令和PWD中的分号进行转义，确保OSC 133解析正确
- `NoReadline` 事件检测bash无readline支持（bind -x不可用，issue #226）

**CWD跟踪:**
- 实时跟踪命令执行时的当前工作目录
- 优先使用OSC 133序列中的cwd信息，回退到会话创建时的cwd
- 在context输出中显示命令执行的完整路径
- 工具执行时通过 `get_shell_cwd()` 获取shell当前CWD，注入到工具输入

**可靠命令记录:**
- 使用`$BASH_COMMAND`通过OSC 133;B payload发送命令文本
- 避免箭头键和历史导航产生的垃圾命令文本（如`"[A"`）
- `ShellInputTracker`作为防御措施，过滤ESC序列

### 消息缓冲
- 可重试消息（`IoData`, `CommandComplete`, `SessionUpdate`）在连接失败时缓冲
- 缓冲区大小限制（`MAX_BUFFER_SIZE = 10_000`）防止内存泄漏
- 重新连接后重放缓冲的消息

### 终端渲染
- 所有显示函数返回纯字符串，不直接执行I/O
- 使用ANSI转义序列进行精确光标控制
- 支持UTF-8多字节字符和全角字符
- 正确处理终端滚动和光标恢复
- InlineNotice通过DSR查询自动选择bottom/top渲染模式
- ChatLayout通过区域管理避免Widget间的渲染冲突

### tmux 窗口标题管理
客户端支持根据当前执行的命令自动更新 tmux 窗口标题，提供直观的会话状态指示。

**更新时机:**
1. **命令开始时**: 当检测到 OSC 133 CommandStart 事件时，将窗口标题设置为正在执行的命令名（提取命令的第一个单词）
2. **命令结束时**: 当检测到 OSC 133 PromptStart 或 CommandEnd 事件时，窗口标题恢复为 "omnish"
3. **子进程变化时**: 通过 polling 机制检测到子进程变化时，更新窗口标题为新的子进程名

**实现原理:**
- 检测环境变量 `TMUX` 判断是否在 tmux 环境中运行
- 使用 tmux OSC 序列设置窗口名称: `\x1b_k{name}\x1b\\`
- 通过 `command_basename()` 函数从完整命令中提取可执行文件名
- 当命令中包含路径时（如 `/usr/bin/vim`），会显示完整路径

**代码位置**:
- `tmux_title()` 函数: 构建 tmux 窗口标题序列
- `command_basename()` 函数: 从命令中提取可执行文件名

### 错误处理
- 守护进程连接失败时进入直通模式
- 配置加载失败时使用默认值
- PTY创建失败时立即退出
- 原始模式错误传播到主函数
- 非TTY stdin时直接exec底层shell（issue #193）

## 测试策略

### 单元测试
- `interceptor.rs`: 输入拦截逻辑测试（包含即时聊天模式进入、UTF-8退格、ESC序列转发、双前缀恢复、expire_prefix超时等）
- `completion.rs`: 完成建议处理测试
- `display.rs`: 终端渲染测试（使用vt100解析器验证）
- `command.rs`: 命令解析测试（包含管道限制和重定向解析、`/thread list`/`/thread del`/`/update` 命令分发）
- `shell_input.rs`: Shell输入跟踪测试
- `shell_hook.rs`: Shell hook功能测试
- `main.rs`: `last_utf8_char_len` 工具函数测试、CursorColTracker测试（行列跟踪、CSI解析）、DsrDetector测试
- `markdown.rs`: Markdown渲染测试（标题、粗体、代码块、列表、引用、链接、表格等）
- `widgets/line_editor.rs`: LineEditor测试
  - 基本编辑：插入、删除、内容查询
  - 光标移动：左右、上下、行首行尾、按词移动、跨行
  - CJK字符：显示宽度计算
  - 换行和多行编辑
  - 粘贴块：插入、两步删除
  - 渲染：单行、多行、幽灵文本、空内容
- `widgets/line_status.rs`: LineStatus测试
  - 基本操作：show、clear、append
  - 截断和max_lines限制
  - vt100终端模拟测试：擦除完整性、替换残留检查
- `widgets/inline_notice.rs`: InlineNotice测试
  - Bottom模式和Top模式渲染
  - 光标保存/恢复
  - 截断处理
  - vt100终端模拟测试：全屏、非全屏、顶部、连续通知等场景
- `widgets/scroll_view.rs`: ScrollView测试
  - Compact/Expanded模式切换
  - 滚动：上下移动、边界钳位
  - 滚动条：有/无、位置计算
  - compact_lines输出（ChatLayout集成用）
  - vt100终端模拟测试：tail视图、擦除、展开/收缩
- `widgets/chat_layout.rs`: ChatLayout测试
  - 区域管理：添加、偏移计算
  - 更新：同高度覆写、高度变化重绘、隐藏/恢复
  - cursor_to定位
  - vt100终端模拟测试：更新序列、区域增长、相对编辑器重绘
- `widgets/picker.rs`: Picker组件渲染测试
  - 项目渲染：普通、选中、多选模式
  - 提示行渲染
  - 完整组件渲染和清理
  - 滚动视口测试：超出项目数时的视口渲染、滚动后的内容

### 集成测试
- 主事件循环模拟测试
- 全屏程序检测测试
- 光标列跟踪测试
- 消息缓冲测试
- 拦截器集成测试（双前缀、超时、额外输入取消等场景）
- Picker选择器集成测试（`tools/integration_tests/test_picker_selection.sh`）
  - 测试 `/resume` 命令中的picker交互
  - 验证方向键导航和Enter确认
  - 验证选择结果正确恢复对话
- `/test picker` 集成测试命令（commit 5df1e1b）
  - 在聊天模式内运行 `/test picker [N]` 启动20项虚拟picker
  - 用于验证picker在实际终端中的显示和交互行为
  - 通过 `/test` picker命令在集成测试框架中调用
- 多索引删除测试（`tools/integration_tests` 中的线程清理）
  - 使用 `/thread del 1-N` 批量删除测试线程

## 性能考虑

### 内存使用
- 输入缓冲区大小有限
- 消息缓冲区有大小限制
- 定期清理完成建议状态
- 完成请求超时自动清理（`IN_FLIGHT_TIMEOUT_MS = 5000`）
- 最大并发请求数限制（`MAX_CONCURRENT_REQUESTS = 5`）

### CPU使用
- `poll`超时100ms避免忙等待
- 输出节流减少守护进程负载
- 完成请求防抖（500ms）
- 所有输入活动重置防抖计时器，避免快速逐字符输入触发请求（issue #100）
- ChatLayout增量更新：同高度区域只覆写变化行，避免全屏重绘

### I/O效率
- 批量处理输入字节
- 输出数据节流发送（`OutputThrottle`）：每条命令默认最多发送 **4MB** 数据（`DEFAULT_MAX_BYTES`），超出后 `should_send()` 返回 false，直到下一个提示符重置（commit 4e437cc, 28aed34, ed4d6ad，#370）；同时限制最多 1000 次请求（`DEFAULT_MAX_REQUESTS`）
- 使用原始模式减少系统调用
- 编辑器重绘使用相对光标移动代替layout.update()（issue #278）

## 更新历史

### 2026-03-23（56个commit自cb68db4起）

**Web Search 格式化器 (commit 9aed75c, #405):**
- 新增 `web_search_formatter` 插件脚本：剥离HTML标签，compact视图显示前5条结果的 `[Title](URL)`，full视图附加描述
- 新增 `/test multi_level_picker` 隐藏测试命令：3级级联picker演示（类别→条目→操作），测试快捷键和多级picker链式调用

**系统提示平台/OS信息来自客户端 (commit d83b63b, #402):**
- `system-reminder` 中的平台和 OS 版本信息改由客户端 Probe（`PlatformProbe`、`OsVersionProbe`）上报到 `session_attrs`，守护进程从 `session_attrs` 读取，不再使用守护进程自身运行环境信息

**developer_mode 聊天触发策略 (commit 6d2794a, #393):**
- 默认仅空命令行触发聊天；`developer_mode = true` 允许有内容时也触发

**`/debug commands` 和 `/debug command` 命令:**
- `/debug commands [N]` — 显示最近 N 条 shell 命令历史（commit 27d19a2）
- `/debug command <seq>` — 显示指定序号命令的完整详情和输出（commit 35542da）

**线程恢复 UX 全面改进 (commit d497b68~82382eb, bebbcc3, 75f71bc, #372, #374):**
- cwd/host 不匹配时弹出带快捷键的 picker 提示
- 锁定线程在 picker 中显示 dim + 🔒，被锁时自动回退到其他线程选择
- `::` auto-resume 取消时退出聊天模式
- 实际执行 `cd` 切换目录并立即更新守护进程 cwd

**工具输出超宽渲染修复 (commit 225b451, #386):**
- `lines_printed` 改用终端实际行数计算，修复 `redraw_tool_section()` 光标偏移错误
- `result_compact` 截断为最多3个终端行

**`/lock on/off` Landlock 沙箱命令 (commit c73013e, #378):**
- 重启 shell 开启/关闭 Landlock 文件系统沙箱
- `apply_sandbox` 重构为 `apply_landlock` + `common_writable_paths`

**可配置沙箱放行规则 (commit f4a4c77, #379):**
- `daemon.toml [sandbox_permit]` 规则引擎，按工具和输入字段匹配有选择绕过沙箱

**`::` resume 优先恢复上次线程 (commit bd6898f, #361):**
- 跟踪 `last_thread_id`，`::` 触发时直接恢复而非总取最新线程

**自动关闭空闲聊天会话 (commit 65b6b15, #360):**
- 客户端30分钟无操作自动退出聊天，守护进程后台清理孤立线程绑定

**防止两个会话进入同一线程 (commit 7ab2968, #357):**
- `ActiveThreads` 映射保证独占，被占用时显示错误

**进入聊天模式时立即更新 CWD (commit 61c1dc4, #354):**
- `ChatSession::run()` 开始时立即发送 `SessionUpdate` 消除轮询延迟

**协议版本不匹配立即断连 (commit d8c340a, #369):**
- 版本不匹配时立即 bail 使重连循环能持续重试

**线程绑定退出时释放 (commit 43004b3, #367):**
- 退出聊天模式发送 `ChatEnd` 消息，守护进程释放线程绑定

**协议重构：typed messages (commit f820330):**
- `ChatStart.thread_id` 替代 `__cmd:resume_tid`，`ChatEnd` 替代 `__cmd:release_thread`，协议升级到 v8

**Picker 滚动提示移至 hint 行 (commit f333e28, #371):**
- `(▼ N more)` 移至 hint 行，分隔线保持全宽

**OutputThrottle 每命令 4MB 上限 (commit 4e437cc, 28aed34, ed4d6ad, #370):**
- 硬性上限防止 dstat 等持续输出程序无限积累；同时新增每命令最多 1000 次请求限制

### 2026-03-18（当前，约60个commit自feeb741起）

**并行工具状态渲染重写 (commit 81a9475, 8ae0126, #342):**
- `ChatSession` 新增 `lines_printed`、`tool_section_start`、`tool_section_hist_idx` 字段
- 新增 `redraw_tool_section()` 方法：上移光标到工具区段起始行，擦除后整体重绘所有 `ToolStatus` 条目
- 工具完成时（第二次 `ChatToolStatus`）原地更新 `scroll_history` 中的条目，再调用重绘
- 中间工具结果（`rpc.call()` 响应中的 `ChatToolStatus`）也触发更新（commit d9b9a42, #344）
- 多工具并行时所有工具状态图标在同一区段内原地更新，视觉效果清晰

**新增 `ScrollEntry::ToolStatus` 变体 (commit d227799):**
- 将工具执行状态作为结构化条目存入 `scroll_history`，替代之前的纯文本方式
- 使用 `ChatToolStatus` 结构体存储 display_name、param_desc、status_icon、result_compact、result_full
- Browse mode（Ctrl+O）中使用 `result_full`，inline显示使用 `result_compact`

**统一工具输出格式 (commit 762e512, 4a6687d):**
- `render_tool_output()` 使用 `⎿` gutter格式，所有输出行带dim样式
- `render_tool_header()` / `render_tool_header_full()` 统一状态图标和参数描述格式

**每线程模型选择 (commit 2a2e8d0):**
- 新增 `/model` 命令，显示所有已配置LLM backend的picker选择器
- 选择持久化通过守护进程 `ThreadMeta` 机制
- 新增 `ChatSession.pending_model` 字段（新线程首条消息时携带）
- 新增 `ChatSession.resumed_model` 字段（resume时来自响应JSON的 `model` 字段）
- Picker新增 `pick_one_at()` 函数支持预选初始项
- 模型名自动去除 `-YYYYMMDD` 日期后缀（`strip_date_suffix()` 函数）

**Picker scroll_offset 溢出修复 (commit 81d0a6b):**
- `max_scroll` 改用 `items.len().saturating_sub(vis)` 计算，防止 `initial >= items.len()` 时溢出

**Resume分隔线Ctrl+O提示修复 (commit 76cc3da):**
- Resume显示历史后的分隔线改用 `render_separator()`，添加 "ctrl+o to expand" 提示

**聊天模式入口Ghost Hint (commit 60fb568):**
- 进入聊天模式时在 `> ` 后显示dim ghost提示
- 新聊天显示 "type to start, /resume to continue"
- Resume后有非默认模型则显示 "model for conversation: {model_name}"

**线程创建延迟 (commit 9dfeb9c, bef24ac):**
- 进入聊天模式时不再立即发送 `ChatStart`
- 线程在首条用户消息前懒创建，避免空线程

**Browse mode改进 (commit d8f503f, 09d71ff 等):**
- Ctrl-F/Ctrl-B 分页滚动
- CJK感知截断（`truncate_cols()` 支持全角字符）
- 长行折行显示代替截断裁剪

**ScrollView提取 (commit 791716d):**
- `ChatSession` 使用自然scrollback显示历史（scroll_view提取自inline chat）

**集成测试框架扩展 (commit 5df1e1b, #343):**
- 新增 `/test picker [N]` 隐藏命令，用于picker组件集成测试
- 新增 `/test` picker命令在集成测试框架中选择测试用例
