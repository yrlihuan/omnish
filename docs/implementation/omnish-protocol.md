# omnish-protocol 模块

**功能:** 定义客户端和守护进程之间的通信协议

## 模块概述

omnish-protocol 定义了客户端和守护进程之间交换的消息类型，使用bincode进行二进制序列化。协议包含一个简单的帧格式，每个帧包含请求ID和消息负载，使用魔术字节"OS"(0x4F 0x53)进行验证。当前协议版本为v16（`PROTOCOL_VERSION = 16`），最低兼容版本为v14（`MIN_COMPATIBLE_VERSION = 14`）。服务器在认证阶段检查对端版本是否 >= MIN_COMPATIBLE_VERSION，配合帧反序列化失败时的跳过机制，实现新旧版本共存。

## 重要数据结构

### `Message` 枚举
主要消息类型：
- `SessionStart`: 新会话开始
- `SessionEnd`: 会话结束
- `SessionUpdate`: 会话属性更新
- `IoData`: 终端I/O数据
- `Event`: 事件通知
- `Request`: LLM请求
- `Response`: LLM响应
- `CommandComplete`: 命令完成通知
- `CompletionRequest`: 自动补全请求
- `CompletionResponse`: 自动补全响应
- `CompletionSummary`: 补全交互分析记录
- `ChatStart`: 聊天会话开始请求（客户端发起）
- `ChatReady`: 聊天会话就绪响应（守护进程返回线程信息）
- `ChatEnd`: 聊天会话结束通知（客户端退出聊天模式时发送）
- `ChatMessage`: 聊天消息（客户端发送查询）
- `ChatResponse`: 聊天响应（守护进程返回LLM回复）
- `ChatInterrupt`: 聊天中断（客户端Ctrl-C取消待处理的聊天请求或中断agent工具调用循环）
- `ChatToolStatus`: 工具执行状态（守护进程在代理循环中流式发送工具执行状态）
- `ChatToolCall`: 客户端侧工具调用转发（守护进程发送给客户端执行）
- `ChatToolResult`: 客户端侧工具执行结果（客户端返回给守护进程）
- `Auth`: 认证消息（客户端发送令牌和协议版本）
- `AuthResult`: 认证结果响应（统一成功/失败，包含daemon版本信息）
- `Ack`: 确认消息
- `ConfigQuery`: 配置查询请求（客户端请求daemon配置项列表）
- `ConfigResponse`: 配置响应（daemon返回配置项和handler信息）
- `ConfigUpdate`: 配置更新请求（客户端发送配置变更）
- `ConfigUpdateResult`: 配置更新结果
- `UpdateCheck`: 更新检查请求（客户端发送平台信息和当前版本）
- `UpdateInfo`: 更新信息响应（daemon返回最新版本和可用性）
- `UpdateRequest`: 更新下载请求（客户端请求下载指定版本）
- `UpdateChunk`: 更新数据块（daemon分块传输更新包）
- `ConfigClient`: 守护进程主动推送配置变更到客户端
- `TestDisconnect`: 测试辅助消息，daemon在指定延迟后断开连接

### `SessionStart`
会话开始消息，包含：
- `session_id`: 会话标识符
- `parent_session_id`: 父会话ID（可选）
- `timestamp_ms`: 时间戳（毫秒）
- `attrs`: 会话属性键值对

### `SessionEnd`
会话结束消息，包含：
- `session_id`: 会话标识符
- `timestamp_ms`: 时间戳（毫秒）
- `exit_code`: 退出代码（可选）

### `SessionUpdate`
会话属性更新消息，用于客户端定期向守护进程发送会话状态更新，包含：
- `session_id`: 会话标识符
- `timestamp_ms`: 时间戳（毫秒）
- `attrs`: 会话属性键值对（`HashMap<String, String>`）

**用途：** 客户端定期向守护进程发送会话属性更新，通过各种探针（Probe）收集的状态信息，使守护进程能够跟踪会话的实时状态。属性值包含来自不同探针的数据。

**标准属性：**
- `host`: 主机名（来自HostnameProbe）
- `shell_cwd`: Shell当前工作目录（来自ShellCwdProbe）
- `child_process`: 当前子进程信息，格式为"name:pid"（来自ChildProcessProbe）
- 其他自定义属性可作为扩展字段存储

**属性来源：** SessionUpdate中的attrs由多个Probe产生，包括HostnameProbe、ShellCwdProbe、ChildProcessProbe等

### `IoData`
I/O数据消息，包含：
- `session_id`: 会话标识符
- `direction`: 输入或输出方向（`IoDirection`枚举）
- `timestamp_ms`: 时间戳（毫秒）
- `data`: 原始字节数据

### `IoDirection` 枚举
I/O方向：
- `Input`: 输入数据（用户到shell）
- `Output`: 输出数据（shell到用户）

### `Event`
事件通知消息，包含：
- `session_id`: 会话标识符
- `timestamp_ms`: 时间戳（毫秒）
- `event_type`: 事件类型（`EventType`枚举）

### `EventType` 枚举
事件类型：
- `NonZeroExit(i32)`: 非零退出代码
- `PatternMatch(String)`: 模式匹配事件
- `CommandBoundary { command: String }`: 命令边界事件

### `Request`
LLM请求消息，包含：
- `request_id`: 请求标识符
- `session_id`: 会话标识符
- `query`: 查询字符串
- `scope`: 请求范围（`RequestScope`枚举）

### `RequestScope` 枚举
请求范围：
- `CurrentSession`: 当前会话
- `AllSessions`: 所有会话
- `Sessions(Vec<String>)`: 指定会话列表

### `Response`
LLM响应消息，包含：
- `request_id`: 请求标识符
- `content`: 响应内容
- `is_streaming`: 是否为流式响应
- `is_final`: 是否为最终响应

### `CommandComplete`
命令完成通知，包含：
- `session_id`: 会话标识符
- `record`: 命令记录（来自omnish-store模块）

### `CompletionRequest`
自动补全请求，包含：
- `session_id`: 会话标识符
- `input`: 输入文本
- `cursor_pos`: 光标位置
- `sequence_id`: 序列ID
- `cwd`: 当前工作目录（可选）

### `CompletionResponse`
自动补全响应，包含：
- `sequence_id`: 序列ID
- `suggestions`: 补全建议列表

### `CompletionSummary`
补全交互分析记录，包含：
- `session_id`: 会话标识符
- `sequence_id`: 序列ID
- `prompt`: 触发补全时的输入
- `completion`: 补全建议文本
- `accepted`: 用户是否接受了补全
- `latency_ms`: 补全响应延迟（毫秒）
- `dwell_time_ms`: 用户停留时间（毫秒）
- `cwd`: 当前工作目录（可选）
- `extra`: 额外元数据键值对（`HashMap<String, String>`，可选，`#[serde(default)]`；使用`String`值而非`serde_json::Value`以兼容bincode序列化）

### `CompletionSuggestion`
补全建议，包含：
- `text`: 建议文本
- `confidence`: 置信度分数

### `ChatTurn`
聊天消息回合，用于构建LLM上下文：
- `role`: 角色（"user" 或 "assistant"）
- `content`: 消息内容

### `ChatStart`
聊天会话开始请求（客户端发起），包含：
- `request_id`: 请求标识符
- `session_id`: 会话标识符
- `new_thread`: 是否创建新线程
- `thread_id`: 指定要恢复的线程ID（可选，`#[serde(default)]`；设置后守护进程将恢复该线程而非创建新线程）

### `ChatReady`
聊天会话就绪响应（守护进程返回），包含：
- `request_id`: 请求标识符
- `thread_id`: 线程标识符
- `last_exchange`: 上一轮对话（可选，`(String, String)` 用户问题和助手回复）
- `earlier_count`: 更早的对话回合数
- `model_name`: 聊天LLM后端使用的模型名称（可选，`#[serde(default)]`，例如 `"claude-sonnet-4-5-20250929"`，用于在ghost text提示中显示当前模型）
- `history`: 结构化对话历史记录（可选，`#[serde(default)]`，`Vec<String>`，每项为JSON编码的字符串；使用`String`而非`serde_json::Value`以兼容bincode序列化）
- `thread_host`: 线程上次使用时的主机名（可选，`#[serde(default)]`，用于检测主机不匹配）
- `thread_cwd`: 线程上次使用时的工作目录（可选，`#[serde(default)]`，用于检测工作目录不匹配）
- `thread_summary`: 对话线程的摘要说明（可选，`#[serde(default)]`，在恢复会话提示中展示）
- `error`: 线程无法进入时的错误键（可选，`#[serde(default)]`，例如 `"thread_locked"`）
- `error_display`: 人类可读的错误说明（可选，`#[serde(default)]`）

### `ChatEnd`
聊天会话结束通知（客户端退出聊天模式时发送），包含：
- `session_id`: 会话标识符
- `thread_id`: 线程标识符

**用途：** 客户端退出聊天模式时发送此消息，通知守护进程释放线程锁。这替代了原先通过`__cmd release`字符串命令传递的机制，使用类型化协议消息，使协议更加清晰。

### `ChatMessage`
聊天消息（客户端发送查询），包含：
- `request_id`: 请求标识符
- `session_id`: 会话标识符
- `thread_id`: 线程标识符
- `query`: 用户查询内容
- `model`: 指定使用的模型（可选，用于per-thread模型选择，通过`/model`命令设置）

### `ChatResponse`
聊天响应（守护进程返回LLM回复），包含：
- `request_id`: 请求标识符
- `thread_id`: 线程标识符
- `content`: LLM回复内容

### `ChatInterrupt`
聊天中断消息（客户端Ctrl-C取消待处理的聊天请求或中断agent工具调用循环），包含：
- `request_id`: 请求标识符
- `session_id`: 会话标识符
- `thread_id`: 线程标识符
- `query`: 被中断的查询内容

### `StatusIcon` 枚举
工具执行状态图标，用于`ChatToolStatus`消息中的状态展示：
- `Running`: 工具正在执行中
- `Success`: 工具执行成功
- `Error`: 工具执行出错

### `ChatToolStatus`
工具执行状态消息（守护进程在代理循环中流式发送工具执行状态），包含：
- `request_id`: 请求标识符
- `thread_id`: 线程标识符
- `tool_name`: 正在执行的工具名称
- `status`: 状态字符串（例如："执行中"、"查询命令历史..."等人类可读的描述）
- `tool_call_id`: 工具调用ID（可选，用于将状态更新与具体的工具调用关联）
- `status_icon`: 状态图标（可选，`StatusIcon`枚举，用于展示工具执行阶段）
- `display_name`: 工具的友好显示名称（可选，例如 `"Command Query"`）
- `param_desc`: 工具参数的简短描述（可选，例如 `"pattern=git"`）
- `result_compact`: 执行结果的紧凑摘要行列表（可选，`Vec<String>`，用于在UI中内联显示）
- `result_full`: 执行结果的完整文本行列表（可选，`Vec<String>`，用于展开显示完整输出）

**用途：** 在聊天会话的代理循环中，当LLM决定使用工具（如命令查询、文件操作等）时，守护进程通过此消息类型向客户端实时推送工具的执行状态。这使得客户端能够向用户显示当前正在执行的操作，提供更好的用户体验和可见性。

**使用场景：**
- 代理开始执行某个工具时发送状态更新（`status_icon: Running`）
- 工具执行完成后发送最终状态（`status_icon: Success` 或 `status_icon: Error`），同时携带`result_compact`和`result_full`
- 客户端可根据`tool_call_id`将多次状态更新关联到同一工具调用，并原地更新展示

### `ChatToolCall`
客户端侧工具调用转发消息（守护进程发送给客户端执行），包含：
- `request_id`: 请求标识符
- `thread_id`: 线程标识符
- `tool_name`: 工具名称
- `tool_call_id`: 工具调用ID（用于关联结果）
- `input`: 工具输入参数（JSON字符串，使用String而非`serde_json::Value`以兼容bincode序列化）
- `plugin_name`: 插件目录名（"builtin"或外部插件名）
- `sandboxed`: 是否对插件进程应用Landlock沙箱

**用途：** 当LLM在agent循环中请求调用客户端侧工具（如文件读写、编辑等）时，守护进程将工具调用转发给客户端执行。客户端通过`ChatToolResult`返回执行结果。

### `ChatToolResult`
客户端侧工具执行结果消息（客户端返回给守护进程），包含：
- `request_id`: 请求标识符
- `thread_id`: 线程标识符
- `tool_call_id`: 对应的工具调用ID
- `content`: 执行结果内容
- `is_error`: 是否为错误结果
- `needs_summarization`: 工具是否请求LLM摘要（`bool`，`#[serde(default)]`，默认`false`；设置为`true`时守护进程将对工具结果进行LLM摘要后再反馈到对话）

### `Auth`
认证消息，客户端连接后发送的第一帧：
- `token`: 认证令牌字符串
- `protocol_version`: 协议版本号（`#[serde(default)]`，旧版本客户端不发送时默认为0）

### `AuthResult`
认证结果响应（统一了原先的`AuthOk`和`AuthFailed`），包含：
- `ok`: 认证是否成功
- `protocol_version`: 服务器协议版本号
- `daemon_version`: daemon版本字符串（`#[serde(default)]`）

**用途：** 统一的认证结果消息。成功时客户端可检测协议版本不匹配；失败时也会返回daemon版本信息。协议版本不匹配时保持连接，允许客户端通过协议进行更新。

### `ConfigItem`
配置项结构，用于`ConfigResponse`中描述daemon配置：
- `path`: 配置路径（如 `"llm.use_cases.completion"`）
- `label`: 显示标签
- `kind`: 配置项类型（`ConfigItemKind`枚举）
- `prefills`: 预填充数据（`Vec<(String, Vec<(String, String)>)>`，`#[serde(default)]`），用于 Select 项在 form_mode 下选中某选项后自动填充同级表单字段。外层元组为 `(选项名, 字段列表)`，内层元组为 `(兄弟项label, 预填值)`

### `ConfigItemKind` 枚举
配置项的输入类型：
- `Toggle { value: bool }`: 开关类型
- `Select { options: Vec<String>, selected: usize }`: 选择类型
- `TextInput { value: String }`: 文本输入类型
- `Label`: 非交互式标签，用于显示描述或分节说明

### `ConfigChange`
配置变更项，用于`ConfigUpdate`消息：
- `path`: 配置路径
- `value`: 新值（字符串）

### `ConfigHandlerInfo`
Handler子菜单元数据，用于`ConfigResponse`中描述需要回调的子菜单：
- `path`: schema路径（如 `"llm.backends.__new__"`）
- `label`: 显示标签（如 `"Add backend"`）
- `handler`: handler函数名（如 `"add_backend"`）

### `ConfigQuery`
配置查询请求，客户端请求daemon的配置项列表。无字段。

### `ConfigResponse`
配置响应，daemon返回配置项和handler信息：
- `items`: 配置项列表（`Vec<ConfigItem>`）
- `handlers`: handler子菜单列表（`Vec<ConfigHandlerInfo>`）

### `ConfigUpdate`
配置更新请求，客户端发送配置变更：
- `changes`: 变更列表（`Vec<ConfigChange>`）

### `ConfigUpdateResult`
配置更新结果：
- `ok`: 是否成功
- `error`: 错误信息（可选）

### `UpdateCheck`
更新检查请求，客户端定期发送以查询是否有新版本：
- `os`: 操作系统（如 `"linux"`）
- `arch`: 架构（如 `"x86_64"`）
- `current_version`: 客户端当前版本
- `hostname`: 主机名（用于per-host速率限制）

### `UpdateInfo`
更新信息响应，daemon返回版本可用性：
- `latest_version`: 最新版本号（经过版本规范化处理）
- `checksum`: SHA-256校验和
- `available`: 是否有更新可用

### `UpdateRequest`
更新下载请求，客户端请求下载指定版本的更新包：
- `os`: 操作系统
- `arch`: 架构
- `version`: 请求的版本号
- `hostname`: 主机名（5分钟per-host冷却时间）

### `UpdateChunk`
更新数据块，daemon分块传输更新包（tar.gz + install.sh）：
- `seq`: 序列号
- `total_size`: 总大小
- `checksum`: 校验和
- `data`: 数据块字节
- `done`: 是否传输完成
- `error`: 错误信息（可选）

### `Frame`
协议帧结构，包含：
- `request_id`: 请求ID（64位无符号整数）
- `payload`: 消息负载（`Message`枚举）

## 关键函数说明

### `Message::to_bytes()`
序列化消息为字节向量，包含魔术字节和长度前缀。

**参数:** 无
**返回:** `Result<Vec<u8>>`
**用途:** 准备网络传输
**格式:** `[魔术字节(2)][长度(4)][序列化消息]`

### `Message::from_bytes()`
从字节向量反序列化消息，验证魔术字节和长度。

**参数:** `bytes: &[u8]`
**返回:** `Result<Message>`
**用途:** 接收网络数据
**验证:** 检查魔术字节"OS"(0x4F 0x53)和消息长度

### `Frame::to_bytes()`
序列化帧为字节向量。

**参数:** 无
**返回:** `Result<Vec<u8>>`
**用途:** 准备带请求ID的网络传输
**格式:** `[请求ID(8)][消息字节]`

### `Frame::from_bytes()`
从字节向量反序列化帧。

**参数:** `bytes: &[u8]`
**返回:** `Result<Frame>`
**用途:** 接收带请求ID的网络数据

## 使用示例

```rust
use omnish_protocol::{Message, SessionStart, Frame};
use std::collections::HashMap;

// 创建会话开始消息
let session_start = SessionStart {
    session_id: "session-123".to_string(),
    parent_session_id: None,
    timestamp_ms: 1000,
    attrs: HashMap::new(),
};

// 序列化消息
let msg = Message::SessionStart(session_start);
let bytes = msg.to_bytes().unwrap();

// 反序列化消息
let restored = Message::from_bytes(&bytes).unwrap();

// 使用帧包装消息
let frame = Frame {
    request_id: 42,
    payload: msg,
};
let frame_bytes = frame.to_bytes().unwrap();
let restored_frame = Frame::from_bytes(&frame_bytes).unwrap();
```

## 依赖关系
- `serde`: 序列化框架
- `serde_json`: JSON序列化（用于`CompletionSummary.extra`字段）
- `bincode`: 二进制序列化
- `anyhow`: 错误处理
- `omnish-store`: 命令记录类型（用于`CommandComplete`消息）

## 协议格式

### 消息格式
```
+--------+--------+----------------+
| 魔术字节 |  长度   |  序列化消息     |
| (2字节) | (4字节) | (变长)         |
+--------+--------+----------------+
```

### 帧格式
```
+------------+-------------------+
|  请求ID    |     消息字节       |
|  (8字节)   |     (变长)        |
+------------+-------------------+
```

魔术字节固定为`[0x4F, 0x53]`（"OS"代表OmniSh），用于验证消息完整性。

## 协议版本管理

协议版本通过`PROTOCOL_VERSION`（当前值16）和`MIN_COMPATIBLE_VERSION`（当前值14）两个常量定义。服务器接受对端版本 >= MIN_COMPATIBLE_VERSION 的连接。`versions_compatible(my_min, peer_version)` 函数封装此判断。

**追加规则：** 新 Message 变体**必须**追加到枚举末尾。bincode 使用 u32 变体索引序列化枚举，在中间插入会移位已有索引，导致旧版客户端解析失败。`variant_indices_are_stable` 测试锁定关键消息的变体索引。

**编译时守卫测试：** `message_variant_guard`测试检测Message枚举变体数量变化，变体数不一致时测试失败并提醒开发者考虑更新`PROTOCOL_VERSION`。

**版本历史：**
- v4: 新增`ChatToolCall`、`ChatToolResult`消息，支持客户端侧工具执行转发；新增`AuthOk`消息替代认证成功时的`Ack`；`Auth`和`ChatInterrupt`新增字段
- v5: `ChatToolStatus`扩展结构化展示字段：新增`tool_call_id`（工具调用关联）、`status_icon`（`StatusIcon`枚举）、`display_name`、`param_desc`、`result_compact`、`result_full`
- v6: `ChatReady`新增`model_name`字段，用于在ghost text提示中显示当前模型名称
- v7: `ChatMessage`新增`model`字段，支持per-thread模型选择（通过`/model`命令）
- v8: 新增`ChatEnd`消息，替代原先的`__cmd release`字符串命令，用于客户端退出聊天模式时通知守护进程释放线程锁；`ChatStart`新增`thread_id`字段，支持恢复指定线程；`ChatReady`新增`history`、`thread_host`、`thread_cwd`、`thread_summary`、`error`、`error_display`字段，支持恢复会话时的主机/工作目录不匹配检测和线程摘要展示；`CompletionSummary.extra`从`HashMap<String, Value>`改为`HashMap<String, String>`以修复bincode序列化问题
- v9: 新增`ConfigQuery`、`ConfigResponse`、`ConfigUpdate`、`ConfigUpdateResult`消息，支持通过协议远程查询和修改daemon配置；新增`ConfigItem`、`ConfigItemKind`、`ConfigChange`、`ConfigHandlerInfo`数据结构
- v10: 新增`UpdateCheck`、`UpdateInfo`、`UpdateRequest`、`UpdateChunk`消息，支持通过协议进行客户端更新检查和包下载；引入hostname字段实现per-host速率限制；版本号规范化处理
- v11: `AuthOk`和`AuthFailed`合并为统一的`AuthResult`消息，新增`ok`和`daemon_version`字段；协议版本不匹配时保持连接（不再断开），允许客户端通过协议进行更新
- v12: `ConfigItem`新增`prefills`字段，支持 Select 项在 form_mode 下选中预设选项后自动填充同级表单字段（用于 Add Backend 表单的 Provider 预设选择器）
- v13: `ChatToolResult`新增`needs_summarization`字段（`#[serde(default)]`），工具可请求守护进程对其结果进行LLM摘要后再反馈到对话
- v14: 引入`MIN_COMPATIBLE_VERSION`常量和`versions_compatible()`函数，实现兼容版本范围检测；新增`ConfigClient`消息，支持守护进程主动推送配置变更到客户端；新增`ConfigItemKind::Label`非交互式标签变体
- v15: 新增`TestDisconnect`消息，用于`/test disconnect`命令测试客户端断线恢复
- v16: 帧反序列化失败时优雅跳过（graceful frame skip），允许与更新版本的对端通信时忽略未知消息

