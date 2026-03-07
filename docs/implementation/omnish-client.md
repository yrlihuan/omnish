# omnish-client 模块

**功能:** 终端客户端，提供交互式shell包装和LLM集成界面

## 模块概述

omnish-client 是终端用户直接交互的客户端程序，作为PTY代理运行shell，拦截用户输入以提供LLM集成功能。主要功能包括：

1. **PTY管理**: 创建伪终端并运行用户指定的shell
2. **输入拦截**: 检测命令前缀（如`:`）进入聊天模式
3. **多轮聊天**: 支持多轮对话循环，包含线程管理（/new, /chat, /ask, /resume, /conversations, /threads）
4. **交互式界面**: 提供美观的终端界面显示聊天提示、输入回显和LLM响应
5. **守护进程通信**: 与omnish-daemon建立连接，发送查询和接收响应
6. **智能完成**: 提供LLM驱动的shell命令完成建议
7. **会话管理**: 跟踪shell会话状态和命令历史
8. **命令跟踪**: 通过OSC 133协议实时跟踪命令执行、CWD（当前工作目录）和退出码

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

**前缀匹配即时进入聊天模式 (issue #116):**
- 前缀完全匹配后立即返回 `InterceptAction::Chat(String::new())`，不再在拦截器中收集后续输入
- 后续输入由 `run_chat_loop` 中的 `read_chat_input` 函数处理
- 退格处理正确支持UTF-8多字节字符（issue #141）

### `InterceptAction` 枚举
输入拦截器返回的动作类型。

**变体:**
- `Buffering(Vec<u8>)` - 正在缓冲输入，不发送到PTY
- `Forward(Vec<u8>)` - 转发字节到PTY
- `Chat(String)` - 聊天消息完成（前缀匹配后立即触发，字符串为空）
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
光标列跟踪器，跟踪终端输出中的光标位置。

**字段:**
- `col: u16` - 当前列位置
- `state: ColTrackState` - 解析状态
- `utf8_buf: [u8; 4]` - UTF-8字符缓冲区
- `utf8_len: u8` - 已收集字节数
- `utf8_need: u8` - 需要字节数

**状态枚举 `ColTrackState`:**
- `Normal` - 正常文本
- `Esc` - ESC序列开始
- `Csi` - CSI序列中
- `Osc` - OSC序列中

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

## 多轮聊天模式 (Multi-turn Chat)

### 概述
当用户输入命令前缀（如`:`）后，客户端进入多轮聊天循环（`run_chat_loop`），支持与LLM进行多轮对话，以及执行聊天内命令。退出方式包括ESC、Ctrl-D（空输入时）、backspace退出（仅首次进入且未发送消息时，issue #120, #124, #127）。

### `run_chat_loop()` 函数
多轮聊天主循环，接管用户输入直到退出。

**参数:**
- `rpc: &RpcClient` - RPC客户端
- `session_id: &str` - 会话ID
- `proxy: &PtyProxy` - PTY代理
- `initial_msg: Option<String>` - 初始消息（如果在前缀匹配前已有输入）
- `client_debug_fn: &dyn Fn() -> String` - 客户端调试状态生成闭包

**内部状态:**
- `current_thread_id: Option<String>` - 当前会话线程ID，懒创建（issue #130）
- `cached_thread_ids: Vec<String>` - 从 `/conversations` 缓存的线程ID列表，用于 `/resume N` 的稳定索引（issue #133）

**流程:**
1. 显示聊天提示符（`> `）
2. 通过 `read_chat_input()` 读取用户输入
3. 处理聊天内命令（`/new`, `/chat`, `/ask`, `/resume`, `/conversations`, `/threads`, `/context`, 等）
4. 非命令输入作为LLM查询发送（懒创建线程）
5. 显示LLM响应，循环继续

### `read_chat_input()` 函数
在原始模式下读取一行聊天输入。

**参数:**
- `completer: &mut GhostCompleter` - 幽灵文本完成器（用于 `/` 命令补全）
- `allow_backspace_exit: bool` - 是否允许空输入时退格退出

**退出方式:**
- `ESC` — 返回None，退出聊天
- `Ctrl-D` — 仅在输入为空时返回None退出（issue #124）
- `Backspace` — 仅在输入为空且 `allow_backspace_exit=true` 时退出（issue #120, #127）

**UTF-8多字节处理 (issue #141):**
- 退格时使用 `last_utf8_char_len()` 计算最后一个UTF-8字符的字节长度
- 根据字符的视觉宽度（`unicode_width`）计算光标回退距离
- 正确处理中文等多字节字符的删除和显示

### 聊天内命令

**线程管理命令:**
- `/new`, `/chat`, `/ask` — 创建新对话线程（发送 `ChatStart` 消息）
- `/resume [N]` — 恢复对话。无参数时恢复最近的线程；带编号时使用 `cached_thread_ids` 缓存的索引（issue #133），自动获取并显示最后一轮对话（issue #137）
- `/conversations`, `/threads` — 列出所有对话（`/threads` 是别名，issue #138），同时缓存线程ID供 `/resume N` 使用
- `/threads del [N]`, `/conversations del [N]` — 删除对话（issue #142），不带编号时先列出对话再交互式选择

**上下文命令:**
- `/context` — 在聊天模式中显示当前线程的对话上下文（issue #136），支持 `| head/tail` 管道（issue #144）

**其他命令:**
- 通过 `handle_slash_command()` 分发到 `command::dispatch()`，支持所有主循环中的 `/` 命令

### Ctrl-C 中断 (issue #123)
聊天等待LLM响应时，用户可按Ctrl-C中断：
- 使用 `tokio::select!` 竞赛RPC调用和 `wait_for_ctrl_c()` 阻塞任务
- `wait_for_ctrl_c()` 在 `spawn_blocking` 中运行，使用 `poll` 以100ms超时监控stdin
- 中断后发送 `ChatInterrupt` 消息到守护进程记录中断事件

### 守护进程JSON响应解析
守护进程命令响应使用JSON格式（issue #134），包含 `display` 字段用于显示和可选的结构化数据字段：
- `parse_cmd_response(content: &str) -> Option<serde_json::Value>` - 解析JSON响应
- `cmd_display_str(json: &serde_json::Value) -> String` - 提取 `display` 字段作为显示文本
- `thread_ids` 数组字段 - 用于缓存线程ID供 `/resume N` 使用
- `thread_id` 字段 - 恢复的线程ID
- `deleted_thread_id` 字段 - 已删除的线程ID

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

**动态 Probe (定期轮询收集):**
- `ShellCwdProbe(pid: u32)` - 获取 shell 进程当前工作目录
  - **Linux**: 读取 `/proc/{pid}/cwd` 符号链接
  - **macOS**: 返回 None（获取其他进程 CWD 在 macOS 上需要 lsof 等复杂方案）
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

**轮询探测 (`default_polling_probes`)**: 动态 Probe 集合，用于定期轮询
- `HostnameProbe` - 主机名（定期轮询以检测集群环境中的变化）
- `ShellCwdProbe` - 当前 shell 进程工作目录
- `ChildProcessProbe` - 当前子进程信息（进程名:PID 格式）

## 关键函数说明

### 主事件循环 (`main.rs`)
客户端的主I/O事件循环，使用`poll`监控stdin和PTY master。

**主要流程:**
1. **初始化**: 加载配置，创建PTY，连接守护进程，进入原始模式
2. **信号处理**: 设置SIGWINCH处理器同步窗口大小
3. **事件循环**:
   - 监控stdin（用户输入）和PTY master（shell输出）
   - 处理用户输入字节，通过`InputInterceptor`检测命令前缀
   - 前缀匹配后进入 `run_chat_loop` 多轮聊天循环
   - 处理shell输出，跟踪光标位置，检测全屏程序
   - 发送I/O数据到守护进程（节流）
   - 处理OSC 133事件进行命令跟踪和CWD（当前工作目录）跟踪
   - 使用`ShellInputTracker`跟踪shell命令行输入
   - 检查并发送完成请求
   - 处理完成响应
   - 记录输入延迟事件（超过50ms时，issue #106）

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
- **macOS**: `ShellCwdProbe` 返回 None（系统复杂性），`ChildProcessProbe` 返回空字符串（基础支持）
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
- 聊天交互: `chat mode enter`, `command complete`
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
3. 发送`Auth`消息进行认证
4. 发送`SessionStart`消息
5. 重放缓冲的消息
6. 连接失败时进入直通模式，打印警告

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
- `render_separator(cols: u16) -> String` - 渲染分隔线
- `render_prompt(cols: u16) -> String` - 渲染初始聊天提示（分隔线 + ❯），用于前缀匹配时的一次性UI
- `render_chat_prompt() -> String` - 渲染聊天模式内的输入提示（`> `），用于多轮聊天循环
- `render_dismiss() -> String` - 清除聊天界面
- `render_input_echo(user_input: &[u8]) -> String` - 渲染输入回显
- `render_thinking() -> String` - 渲染思考状态
- `render_response(content: &str) -> String` - 渲染LLM响应
- `render_error(msg: &str) -> String` - 渲染错误消息
- `render_ghost_text(ghost: &str) -> String` - 渲染幽灵文本建议
- `render_chat_history(last_exchange: Option<&(String, String)>, earlier_count: u32) -> String` - 渲染聊天历史（用于恢复对话时显示上下文）

### 命令分发 (`command.rs`)
解析聊天消息中的命令，使用统一的命令注册表管理所有聊天命令和完成建议。

**命令注册表:**
- `COMMANDS`: 静态命令数组，包含所有支持的聊天命令
- `CommandEntry`: 命令条目，包含命令路径、类型（本地或守护进程）和帮助文本
- `CommandKind::Local`: 客户端本地处理的命令
- `CommandKind::Daemon`: 转发到守护进程的命令（格式：`__cmd:{key}`）
- `CHAT_ONLY_COMMANDS`: 聊天模式专用命令列表（`/chat`, `/ask`, `/resume`, `/new`），不在注册表中但包含在自动完成中

**函数:**
- `dispatch(msg: &str) -> ChatAction` - 分发聊天消息，查找最长匹配命令路径
- `parse_redirect(input: &str) -> (&str, Option<&str>)` - 解析重定向后缀
- `parse_limit(input: &str) -> (&str, Option<OutputLimit>)` - 解析 `| head` / `| tail` 管道后缀
- `parse_limit_pub(input: &str) -> (&str, Option<OutputLimit>)` - `parse_limit` 的公开包装（用于 `main.rs` 中聊天模式的 `/context`）
- `apply_limit(text: &str, limit: &OutputLimit) -> String` - 对输出文本应用行数限制
- `completable_commands() -> Vec<String>` - 返回所有可完成命令路径，用于幽灵文本建议（包含聊天专用命令）

**支持命令:**
- `/help` - 显示所有可用命令
- `/context [template]` - 获取LLM上下文（转发到守护进程）；在聊天模式中显示当前线程的对话上下文
- `/template <name>` - 显示LLM提示模板（客户端本地处理）
- `/debug` - 显示调试子命令用法
- `/debug events [num]` - 显示最近的客户端事件（默认20条）
- `/debug client` - 显示客户端调试状态（通过闭包在客户端本地生成，issue #115）
- `/debug session` - 显示当前会话调试信息（转发到守护进程）
- `/sessions` - 列出所有会话（转发到守护进程）
- `/conversations` - 列出所有对话（转发到守护进程）
- `/threads` - `/conversations` 的别名（issue #138）
- `/tasks [disable <name>]` - 查看或管理定时任务（转发到守护进程）
- `> file.txt` - 重定向输出到文件（后缀支持）
- `| head [-n] [N]` / `| tail [-n] [N]` - 限制输出行数（默认10行），支持 `-nN` 紧凑语法

**聊天模式专用命令（`CHAT_ONLY_COMMANDS`）:**
- `/chat`, `/ask` - 在聊天中创建新对话线程
- `/new` - 创建新对话线程
- `/resume [N]` - 恢复对话

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
   - 显示分隔线和`❯`提示符
   - 立即进入多轮聊天循环
   - 显示`> `提示符等待输入
3. **多轮对话**:
   - 直接输入问题即可开始对话（自动懒创建线程）
   - 输入多个问题进行多轮对话
   - `/new` 或 `/chat` 开始新对话
   - `/resume` 恢复最近的对话
   - `/conversations` 或 `/threads` 列出所有对话
   - `/resume N` 恢复第N个对话
   - `/threads del N` 删除第N个对话
   - `Ctrl-C` 中断等待中的LLM响应
4. **使用聊天命令**: 在聊天模式下，支持以下命令：
   - `/context` - 查看当前线程的对话上下文，支持 `| head 5` 或 `| tail 10`
   - `/debug client` - 查看客户端调试状态（包含shell CWD、输入跟踪器、补全器等）
   - `/debug events` - 查看最近事件日志
   - `/template <name>` - 显示LLM提示模板
   - `/sessions` - 列出所有活动会话
   - `> file.txt` - 重定向输出到文件（如`/context > context.txt`）

   **上下文输出特点**:
   - 显示命令执行的完整路径（CWD）
   - 失败命令显示`[FAILED: exit_code]`标签
   - 命令按会话分组，当前会话显示在最后
   - 在聊天模式中，`/context` 显示当前线程的对话历史
5. **退出聊天模式**:
   - `ESC` — 立即退出
   - `Ctrl-D` — 输入为空时退出
   - `Backspace` — 首次进入且未发送消息时，空输入退格退出
6. **接受完成建议**: 在shell提示符下，LLM会提供命令完成建议
   - 显示为灰色幽灵文本
   - 按Tab接受建议
   - 光标不在行末时自动抑制补全建议（cursor_at_end检查）
   - 配置中`completion_enabled`为false时完全禁用补全
   - isearch模式（Ctrl+R）中自动丢弃完成响应
7. **取消操作**: 按ESC取消聊天模式

### 配置文件示例
```toml
# ~/.omnish/client.toml
[shell]
command = "/bin/bash"
command_prefix = ":"
intercept_gap_ms = 1000

daemon_addr = "~/.omnish/omnish.sock"
```

### 环境变量
- `OMNISH_SOCKET`: 守护进程socket路径（覆盖配置）
- `OMNISH_SESSION_ID`: 父会话ID（用于嵌套omnish检测）
- `SHELL`: 使用的shell命令（覆盖配置）

## 依赖关系

### 内部依赖
- `omnish-common`: 配置加载
- `omnish-pty`: PTY管理
- `omnish-transport`: RPC通信
- `omnish-protocol`: 消息协议
- `omnish-tracker`: 命令跟踪
- `omnish-llm`: 模板名称和模板内容（用于 `/template` 和 `/context` 命令补全）

### 外部依赖
- `tokio`: 异步运行时
- `nix`: 系统调用（原始模式、信号处理）
- `libc`: 低级系统接口
- `unicode-width`: Unicode字符宽度计算
- `uuid`: 会话ID生成
- `vt100`: 终端解析（测试用）
- `serde_json`: 守护进程JSON响应解析

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

### 聊天模式架构
聊天模式分为两层：
1. **入口层（主循环）**: `InputInterceptor` 检测前缀匹配后立即返回 `Chat("")`，触发进入 `run_chat_loop`
2. **聊天层（`run_chat_loop`）**: 独立的输入循环 `read_chat_input`，处理聊天内的所有键盘交互（包括UTF-8输入、退格、Tab补全、ESC/Ctrl-D/backspace退出）

这种分离使得：
- 拦截器保持简单（仅负责前缀检测）
- 聊天输入处理可以独立优化（如UTF-8多字节字符支持）
- 退出行为可以按阶段控制（如backspace只在未发送消息时允许退出）

### OSC 133协议和CWD跟踪
通过shell hook和OSC 133终端控制序列实现命令跟踪和CWD（当前工作目录）跟踪：

**Shell Hook机制:**
- 安装Bash shell hook，通过`PROMPT_COMMAND`和`DEBUG` trap集成
- 发送OSC 133序列：`B;command_text;cwd:/path;orig:original_input`（命令开始，包含`$BASH_COMMAND`、工作目录、`history 1`原始输入）、`D;exit_code`（命令结束）、`A`（提示开始）、`C`（输出开始）
- `RL;content;point` - readline状态报告（`$READLINE_LINE`和`$READLINE_POINT`）
- 使用复合赋值`__omnish_last_ec=$? __omnish_in_precmd=1`立即捕获退出码，避免被`PROMPT_COMMAND`中的其他命令覆盖
- 对命令和PWD中的分号进行转义，确保OSC 133解析正确

**CWD跟踪:**
- 实时跟踪命令执行时的当前工作目录
- 优先使用OSC 133序列中的cwd信息，回退到会话创建时的cwd
- 在context输出中显示命令执行的完整路径

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

## 测试策略

### 单元测试
- `interceptor.rs`: 输入拦截逻辑测试（包含即时聊天模式进入、UTF-8退格、ESC序列转发等）
- `completion.rs`: 完成建议处理测试
- `display.rs`: 终端渲染测试（使用vt100解析器验证）
- `command.rs`: 命令解析测试（包含管道限制和重定向解析）
- `shell_input.rs`: Shell输入跟踪测试
- `shell_hook.rs`: Shell hook功能测试
- `main.rs`: `last_utf8_char_len` 工具函数测试

### 集成测试
- 主事件循环模拟测试
- 全屏程序检测测试
- 光标列跟踪测试
- 消息缓冲测试

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

### I/O效率
- 批量处理输入字节
- 输出数据节流发送
- 使用原始模式减少系统调用
