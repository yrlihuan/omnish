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
- `DaemonDebug { query: String, redirect: Option<String> }` - 守护进程调试查询

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
   - 处理OSC 133事件进行命令跟踪
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
解析聊天消息中的命令。

**函数:**
- `dispatch(msg: &str) -> ChatAction` - 分发聊天消息
- `parse_redirect(input: &str) -> (&str, Option<&str>)` - 解析重定向后缀
- `handle_debug(args: &[&str], redirect: Option<String>) -> ChatAction` - 处理调试命令

**支持命令:**
- `/debug context` - 获取守护进程上下文
- `/debug template` - 显示LLM提示模板
- `> file.txt` - 重定向输出到文件

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
3. **接受完成建议**: 在shell提示符下，LLM会提供命令完成建议
   - 显示为灰色幽灵文本
   - 按Tab接受建议
4. **取消操作**: 按ESC取消聊天模式

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

### 消息缓冲
- 可重试消息（`IoData`, `CommandComplete`）在连接失败时缓冲
- 缓冲区大小限制防止内存泄漏
- 重新连接后重放缓冲的消息

### 终端渲染
- 所有显示函数返回纯字符串，不直接执行I/O
- 使用ANSI转义序列进行精确光标控制
- 支持UTF-8多字节字符和全角字符
- 正确处理终端滚动和光标恢复

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