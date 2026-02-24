# omnish-protocol 模块

**功能:** 定义客户端和守护进程之间的通信协议

## 模块概述

omnish-protocol 定义了客户端和守护进程之间交换的消息类型，使用bincode进行二进制序列化。协议包含一个简单的帧格式，每个帧包含请求ID和消息负载，使用魔术字节"OS"(0x4F 0x53)进行验证。

## 重要数据结构

### `Message` 枚举
主要消息类型：
- `SessionStart`: 新会话开始
- `SessionEnd`: 会话结束
- `IoData`: 终端I/O数据
- `Event`: 事件通知
- `Request`: LLM请求
- `Response`: LLM响应
- `CommandComplete`: 命令完成通知
- `CompletionRequest`: 自动补全请求
- `CompletionResponse`: 自动补全响应
- `Ack`: 确认消息

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

### `CompletionResponse`
自动补全响应，包含：
- `sequence_id`: 序列ID
- `suggestions`: 补全建议列表

### `CompletionSuggestion`
补全建议，包含：
- `text`: 建议文本
- `confidence`: 置信度分数

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