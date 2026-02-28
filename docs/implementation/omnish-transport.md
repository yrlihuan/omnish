# omnish-transport 模块

**功能:** RPC传输层，处理Unix socket和TCP连接，提供客户端和守护进程之间的可靠通信

## 模块概述

omnish-transport 提供客户端和守护进程之间的RPC通信层，支持Unix socket和TCP协议。该模块负责：
- 地址解析和连接管理
- 请求-响应消息传输
- 自动重连机制
- 并发请求处理
- 连接状态监控

模块包含两个主要组件：
- `RpcClient`: 客户端连接管理器，支持自动重连
- `RpcServer`: 服务器监听器，处理多个客户端连接

## 重要数据结构

### `TransportAddr` 枚举
传输地址类型，用于统一处理不同协议：
```rust
pub enum TransportAddr {
    Unix(String),  // Unix socket路径
    Tcp(String),   // TCP地址（主机:端口）
}
```

### `RpcClient`
RPC客户端结构，负责：
- 连接到守护进程
- 发送消息并等待响应
- 管理连接状态和自动重连
- 处理并发请求

**内部结构:**
- `inner`: 包含发送通道、连接状态和后台任务
- `next_id`: 原子计数器，用于生成请求ID
- 支持Unix socket和TCP连接

### `RpcServer`
RPC服务器结构，负责：
- 监听Unix socket或TCP端口
- 接受客户端连接
- 为每个连接生成独立处理任务
- 调用用户提供的消息处理器

**内部结构:**
- `listener`: 监听器（Unix或TCP）
- 为每个连接生成独立的异步任务

### `Frame`
消息帧结构（来自omnish-protocol）：
```rust
pub struct Frame {
    pub request_id: u64,    // 请求标识符
    pub payload: Message,   // 消息负载
}
```

## 关键函数说明

### `parse_addr()`
解析地址字符串为TransportAddr。

**参数:** `addr: &str`
**返回:** `TransportAddr`
**用途:** 解析配置中的地址，支持多种格式：
- Unix socket: `/tmp/omnish.sock`, `./omnish.sock`, `omnish.sock`
- TCP地址: `127.0.0.1:9876`, `localhost:9876`, `[::1]:9876`
- 显式协议: `tcp://127.0.0.1:9500`

### `RpcClient::connect()`
连接到RPC服务器（自动选择协议）。

**参数:** `addr: &str`
**返回:** `Result<RpcClient>`
**用途:** 根据地址类型建立连接

### `RpcClient::connect_unix()` / `RpcClient::connect_tcp()`
建立特定协议的连接。

**参数:** `addr: &str`
**返回:** `Result<RpcClient>`
**用途:** 建立Unix socket或TCP连接

### `RpcClient::connect_with_reconnect()`
建立支持自动重连的连接。

**参数:**
- `addr: &str`: 服务器地址
- `on_reconnect: Fn(&RpcClient) -> Future<Output = Result<()>>`: 重连回调函数

**返回:** `Result<RpcClient>`
**用途:** 建立连接并设置自动重连机制，支持指数退避重试

### `RpcClient::call()`
发送消息到服务器并等待响应。

**参数:** `msg: Message`
**返回:** `Result<Message>`
**用途:** 发送请求并等待响应，使用请求ID匹配响应

### `RpcClient::is_connected()`
检查客户端是否连接。

**返回:** `bool`
**用途:** 监控连接状态

### `RpcServer::bind()`
绑定到地址并开始监听（自动选择协议）。

**参数:** `addr: &str`
**返回:** `Result<RpcServer>`
**用途:** 启动服务器监听器

### `RpcServer::bind_unix()` / `RpcServer::bind_tcp()`
绑定到特定协议的地址。

**参数:** `addr: &str`
**返回:** `Result<RpcServer>`
**用途:** 启动Unix socket或TCP监听器

### `RpcServer::serve()`
开始处理客户端连接。

**参数:** `handler: Fn(Message) -> Future<Output = Message>`
**返回:** `Result<()>`
**用途:** 循环接受连接并为每个连接生成处理任务

### `RpcServer::local_tcp_addr()`
获取TCP监听器的本地地址。

**返回:** `Option<SocketAddr>`
**用途:** 获取TCP服务器的绑定地址

## 使用示例

### 客户端连接示例
```rust
use omnish_transport::{RpcClient, parse_addr};

// 自动选择协议连接
let client = RpcClient::connect("/tmp/omnish.sock").await?;

// 显式Unix socket连接
let client = RpcClient::connect_unix("/tmp/omnish.sock").await?;

// 显式TCP连接
let client = RpcClient::connect_tcp("127.0.0.1:9876").await?;

// 带自动重连的连接
let client = RpcClient::connect_with_reconnect("/tmp/omnish.sock", |rpc| {
    Box::pin(async move {
        // 重连后重新注册会话
        rpc.call(session_start_message).await?;
        Ok(())
    })
}).await?;

// 发送消息并等待响应
let response = client.call(message).await?;

// 检查连接状态
if client.is_connected().await {
    println!("Connected to server");
}
```

### 服务器示例
```rust
use omnish_transport::RpcServer;
use omnish_protocol::message::{Message, Response, Request};

// 绑定到Unix socket
let mut server = RpcServer::bind_unix("/tmp/omnish.sock").await?;

// 绑定到TCP端口
let mut server = RpcServer::bind_tcp("127.0.0.1:9876").await?;

// 自动选择协议绑定
let mut server = RpcServer::bind("/tmp/omnish.sock").await?;

// 获取TCP地址（如果是TCP服务器）
if let Some(addr) = server.local_tcp_addr() {
    println!("Server listening on {}", addr);
}

// 启动服务器处理连接
server.serve(|msg| {
    Box::pin(async move {
        match msg {
            Message::Request(req) => {
                // 处理请求并返回响应
                Message::Response(Response {
                    request_id: req.request_id.clone(),
                    content: format!("Echo: {}", req.query),
                    is_streaming: false,
                    is_final: true,
                })
            }
            _ => Message::Ack,
        }
    })
}).await?;
```

### 地址解析示例
```rust
use omnish_transport::parse_addr;

let addr1 = parse_addr("/tmp/omnish.sock");      // Unix socket
let addr2 = parse_addr("127.0.0.1:9876");        // TCP地址
let addr3 = parse_addr("tcp://localhost:9500");  // 显式TCP协议
let addr4 = parse_addr("./local.sock");          // 相对路径Unix socket
```

## 内部工作机制

### 客户端内部结构
1. **连接管理**: 使用`AsyncRead`和`AsyncWrite`trait抽象不同传输协议
2. **读写分离**: 连接被拆分为独立的读取器和写入器
3. **后台任务**:
   - `write_loop`: 处理发送队列，序列化并发送消息
   - `read_loop`: 接收响应，根据请求ID分发到对应的oneshot通道
4. **请求ID管理**: 使用原子计数器生成唯一请求ID
5. **重连机制**: 使用指数退避算法自动重连，支持重连回调

### 服务器内部结构
1. **连接接受**: 循环接受新连接
2. **任务生成**: 为每个连接生成独立的异步任务
3. **消息处理**: 读取消息帧，调用用户处理器，发送响应
4. **并发支持**: 每个连接独立处理，互不干扰

### 消息传输协议
1. **帧格式**: `[长度: u32][序列化帧数据]`
2. **请求-响应匹配**: 使用`request_id`关联请求和响应
3. **错误处理**: 连接断开时清理挂起的请求

### 连接断开处理
当守护进程意外断开连接或网络故障导致连接中断时，必须防止客户端的`call()`方法永久挂起。这是通过显式清空待处理的请求映射来实现的。

**核心机制:**
在`read_loop`和`write_loop`函数退出时，调用`pending.lock().await.clear()`清空挂起的请求映射。此映射存储了所有等待响应的请求ID与oneshot发送端的对应关系。

**必要性:**
- 当守护进程死亡时，`read_loop`停止并退出，但`write_loop`仍保有`pending` Arc的引用，使其内部的oneshot发送端保持活动状态
- 客户端中调用`call()`的协程通过`reply_rx.await`等待响应，而oneshot接收端持有对发送端的引用
- 如果不显式清空mapping，oneshot发送端将保持活动状态，导致`reply_rx.await`永久阻塞，客户端出现"僵尸"状态

**工作流程:**
1. 连接断开（e.g., 守护进程崩溃或网络故障）
2. `read_loop`或`write_loop`检测到I/O错误并退出
3. 循环退出前调用`pending.lock().await.clear()`
4. 所有oneshot发送端被销毁
5. 所有正在`reply_rx.await`的协程收到`RecvError`并解除阻塞
6. `call()`返回错误而非永久挂起

这个设计确保客户端能够快速检测到与守护进程的连接失败，从而进行重连或返回错误。

## 依赖关系
- **omnish-protocol**: 消息类型定义和序列化
- **tokio**: 异步运行时、网络I/O和同步原语
- **anyhow**: 错误处理
- **tracing**: 日志记录
- **std::sync**: 原子操作和同步原语

## 测试覆盖
模块包含全面的测试用例：
- 基本请求-响应测试
- 并发请求处理测试
- 自动重连机制测试
- 连接状态监控测试
- Unix socket和TCP协议测试
- 多客户端并发测试