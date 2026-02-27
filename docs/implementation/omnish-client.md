# omnish-client 模块

**功能:** 终端客户端，提供交互式shell包装和LLM集成界面

## 模块概述

omnish-client 是终端用户直接交互的客户端程序，作为PTY代理运行shell，拦截用户输入以提供LLM集成功能。主要功能包括：

1. **PTY管理**: 创建伪终端并运行用户指定的shell
2. **输入拦截**: 检测命令前缀（如`:`）进入聊天模式
3. **交互式界面**: 提供美观的终端界面显示聊天提示、输入回显和LLM响应
4. **守护进程通信**: 与omnish-daemon建立连接，发送查询和接收响应
5. **智能完成**: 提供LLM驱动的shell命令完成建议
6. **会话管理**: 跟踪shell会话状态和命令历史
7. **命令跟踪**: 通过OSC 133协议实时跟踪命令执行、CWD（当前工作目录）和退出码

## 重要数据结构

### `InputInterceptor`
输入拦截器，负责检测命令前缀和管理聊天模式状态。

**字段:**
- `prefix: Vec<u8>` - 命令前缀字节序列（如`b":"`）
- `buffer: VecDeque<u8>` - 当前输入缓冲区
- `in_chat: bool` - 是否处于聊天模式
- `suppressed: bool` - 是否抑制拦截（如在vim等全屏程序中）
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

### `InterceptAction` 枚举
输入拦截器返回的动作类型。

**变体:**
- `Buffering(Vec<u8>)` - 正在缓冲输入，不发送到PTY
- `Forward(Vec<u8>)` - 转发字节到PTY
- `Chat(String)` - 聊天消息完成（用户按Enter）
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
- `CsiParam(Vec<u8>)` - CSI序列参数收集中
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
- `in_flight: bool` - 是否有请求在处理中
- `ghost_input: String` - 产生当前建议的输入

**方法:**
- `on_input_changed(input: &str, sequence_id: u64)` - 输入变化通知
- `should_request(current_input: &str) -> bool` - 是否应该发送请求
- `mark_sent(sequence_id: u64)` - 标记请求已发送
- `on_response(response: &CompletionResponse, current_input: &str) -> Option<&str>` - 处理响应
- `accept() -> Option<String>` - 接受当前建议
- `clear()` - 清除建议
- `ghost() -> Option<&str>` - 获取当前建议
- `build_request(session_id: &str, input: &str, sequence_id: u64) -> Message` - 构建完成请求

### `ShellInputTracker`
Shell命令行输入跟踪器，通过观察转发的字节和OSC 133状态转换跟踪当前shell命令行输入。

**生命周期:**
1. OSC 133;A/D (PromptStart/CommandEnd) → `on_prompt()`: `at_prompt = true`
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

**方法:**
- `new() -> Self` - 创建新跟踪器
- `on_prompt()` - OSC 133;A或133;D检测到时调用
- `feed_forwarded(bytes: &[u8])` - 馈送转发到PTY的字节
- `inject(text: &str)` - 追加文本到输入（例如Tab接受后写入PTY）
- `input() -> &str` - 当前输入文本
- `sequence_id() -> u64` - 当前序列ID
- `at_prompt() -> bool` - 用户是否在提示符处
- `take_change() -> Option<(&str, u64)>` - 检查输入是否变化并返回当前状态

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
- `Command { result: String, redirect: Option<String> }` - 本地命令执行
- `LlmQuery(String)` - LLM查询
- `DaemonQuery { query: String, redirect: Option<String> }` - 需要守护进程数据的查询

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
- `ShellCwdProbe(pid: u32)` - 通过 `/proc/{pid}/cwd` 获取 shell 进程当前工作目录
  - 读取 shell 进程的符号链接 `/proc/<pid>/cwd` 获取当前工作目录
  - 用于实时跟踪 shell 的实际工作目录
- `ChildProcessProbe(pid: u32)` - 通过 `/proc/{pid}/task/{pid}/children` 获取子进程信息
  - 读取 `/proc/<pid>/task/<pid>/children` 获取子进程 PID 列表
  - 取最后一个子进程，读取其 `/proc/<pid>/comm` 获取进程名
  - 返回格式为 `"name:pid"` 的字符串（如 `"vim:12345"`）
  - 如果没有子进程则返回空字符串

### 默认 Probe 集合

**会话探测 (`default_session_probes`)**: 静态 Probe 集合，在会话开始时收集一次
- 包含: ShellProbe, PidProbe, TtyProbe, CwdProbe, HostnameProbe

**轮询探测 (`default_polling_probes`)**: 动态 Probe 集合，用于定期轮询
- 包含: ShellCwdProbe, ChildProcessProbe

## 关键函数说明

### 主事件循环 (`main.rs`)
客户端的主I/O事件循环，使用`poll`监控stdin和PTY master。

**主要流程:**
1. **初始化**: 加载配置，创建PTY，连接守护进程，进入原始模式
2. **信号处理**: 设置SIGWINCH处理器同步窗口大小
3. **事件循环**:
   - 监控stdin（用户输入）和PTY master（shell输出）
   - 处理用户输入字节，通过`InputInterceptor`检测命令前缀
   - 处理shell输出，跟踪光标位置，检测全屏程序
   - 发送I/O数据到守护进程（节流）
   - 处理OSC 133事件进行命令跟踪和CWD（当前工作目录）跟踪
   - 使用`ShellInputTracker`跟踪shell命令行输入
   - 检查并发送完成请求
   - 处理完成响应

### `send_or_buffer()`
发送消息到守护进程，失败时缓冲可重试的消息。

**参数:**
- `rpc: &RpcClient` - RPC客户端
- `msg: Message` - 要发送的消息
- `buffer: &MessageBuffer` - 消息缓冲区

**逻辑:**
- 如果发送失败且消息类型可缓冲（`IoData`或`CommandComplete`），则加入缓冲区
- 缓冲区有大小限制（`MAX_BUFFER_SIZE = 10_000`），满时丢弃最旧消息

### Polling 机制

客户端启动后会在后台运行一个定期探测任务，用于持续收集 shell 进程的状态信息。

**工作机制:**
1. **启动时机**: 与守护进程建立连接后自动启动
2. **探测间隔**: 每 5 秒执行一次探测
3. **数据来源**: 使用 `default_polling_probes` 收集 ShellCwdProbe 和 ChildProcessProbe
4. **差异更新**: 维护上一次探测结果的副本，仅当数值发生变化时才更新
5. **消息发送**: 通过 `SessionUpdate` 消息将变化的数据发送到守护进程

**tmux 窗口标题更新:**
- 当 `child_process` 探测值发生变化时，自动更新 tmux 窗口标题
- 提取进程名（不含 PID）作为窗口标题
- 使用 tmux OSC 序列 `\x1b_k{name}\x1b\\` 设置窗口名称

**代码位置**: `main.rs` 中的异步任务块 (约第 120-157 行)

### `connect_daemon()`
连接守护进程，支持优雅降级（守护进程不可用时进入直通模式）。

**参数:**
- `daemon_addr: &str` - 守护进程地址
- `session_id: &str` - 会话ID
- `parent_session_id: Option<String>` - 父会话ID
- `child_pid: u32` - 子进程PID
- `buffer: MessageBuffer` - 消息缓冲区

**流程:**
1. 尝试连接守护进程
2. 发送`SessionStart`消息
3. 重放缓冲的消息
4. 连接失败时进入直通模式，打印警告

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

### 显示函数 (`display.rs`)
纯函数，生成ANSI终端输出字符串。

**函数列表:**
- `render_separator(cols: u16) -> String` - 渲染分隔线
- `render_prompt(cols: u16) -> String` - 渲染聊天提示（分隔线 + ❯）
- `render_dismiss() -> String` - 清除聊天界面
- `render_input_echo(user_input: &[u8]) -> String` - 渲染输入回显
- `render_thinking() -> String` - 渲染思考状态
- `render_response(content: &str) -> String` - 渲染LLM响应
- `render_error(msg: &str) -> String` - 渲染错误消息
- `render_ghost_text(ghost: &str) -> String` - 渲染幽灵文本建议

### 命令分发 (`command.rs`)
解析聊天消息中的命令，使用统一的命令注册表管理所有聊天命令和完成建议。

**命令注册表:**
- `COMMANDS`: 静态命令数组，包含所有支持的聊天命令
- `CommandEntry`: 命令条目，包含命令路径、类型（本地或守护进程）和帮助文本
- `CommandKind::Local`: 客户端本地处理的命令
- `CommandKind::Daemon`: 转发到守护进程的命令（格式：`__cmd:{key}`）

**函数:**
- `dispatch(msg: &str) -> ChatAction` - 分发聊天消息，查找最长匹配命令路径
- `parse_redirect(input: &str) -> (&str, Option<&str>)` - 解析重定向后缀
- `completable_commands() -> Vec<String>` - 返回所有可完成命令路径，用于幽灵文本建议

**支持命令:**
- `/debug` - 显示调试子命令用法
- `/debug context` - 获取守护进程上下文（转发到守护进程）
- `/debug template` - 显示LLM提示模板（客户端本地处理）
- `/sessions` - 列出所有会话（转发到守护进程）
- `> file.txt` - 重定向输出到文件（后缀支持）

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
   - 输入LLM查询，如`why did my command fail?`
   - 按Enter发送查询
3. **使用聊天命令**: 在聊天模式下，支持以下命令：
   - `/debug context` - 查看当前上下文（显示最近的命令历史，包括CWD和退出码）
   - `/debug template` - 显示LLM提示模板
   - `/sessions` - 列出所有活动会话
   - `> file.txt` - 重定向输出到文件（如`/debug context > context.txt`）

   **上下文输出特点**:
   - 显示命令执行的完整路径（CWD）
   - 失败命令显示`[FAILED: exit_code]`标签
   - 命令按会话分组，当前会话显示在最后
4. **接受完成建议**: 在shell提示符下，LLM会提供命令完成建议
   - 显示为灰色幽灵文本
   - 按Tab接受建议
5. **取消操作**: 按ESC取消聊天模式

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

### 外部依赖
- `tokio`: 异步运行时
- `nix`: 系统调用（原始模式、信号处理）
- `libc`: 低级系统接口
- `unicode-width`: Unicode字符宽度计算
- `uuid`: 会话ID生成
- `vt100`: 终端解析（测试用）

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

### OSC 133协议和CWD跟踪
通过shell hook和OSC 133终端控制序列实现命令跟踪和CWD（当前工作目录）跟踪：

**Shell Hook机制:**
- 安装Bash shell hook，通过`PROMPT_COMMAND`和`DEBUG` trap集成
- 发送OSC 133序列：`B;command_text;cwd:/path`（命令开始）、`D;exit_code`（命令结束）、`A`（提示开始）、`C`（输出开始）
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
- 可重试消息（`IoData`, `CommandComplete`）在连接失败时缓冲
- 缓冲区大小限制防止内存泄漏
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
- `interceptor.rs`: 输入拦截逻辑测试
- `completion.rs`: 完成建议处理测试
- `display.rs`: 终端渲染测试（使用vt100解析器验证）
- `command.rs`: 命令解析测试
- `shell_input.rs`: Shell输入跟踪测试
- `shell_hook.rs`: Shell hook功能测试

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

### CPU使用
- `poll`超时100ms避免忙等待
- 输出节流减少守护进程负载
- 完成请求防抖（500ms）

### I/O效率
- 批量处理输入字节
- 输出数据节流发送
- 使用原始模式减少系统调用