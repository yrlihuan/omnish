# omnish-transport 模块

**功能:** RPC传输层，处理Unix socket和TCP连接，提供客户端和守护进程之间的可靠通信

## 模块概述

omnish-transport 提供客户端和守护进程之间的RPC通信层，支持Unix socket和TCP协议。该模块负责：
- 地址解析和连接管理
- 请求-响应消息传输（单消息和多消息流式传输）
- 自动重连机制
- 并发请求处理
- 连接状态监控
- 认证和访问控制
- 协议版本协商
- TLS加密（TCP连接）

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
- 发送消息并等待单个响应或接收多个响应流
- 管理连接状态和自动重连
- 处理并发请求

**内部结构:**
- `inner`: 包含发送通道、连接状态和后台任务
- `next_id`: 原子计数器，用于生成请求ID
- `ReplyTx`: 枚举类型，支持oneshot（单响应）和mpsc（多响应流）两种模式
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

### TLS支持

omnish-transport 支持TCP连接的TLS加密，使用自签名证书。

**tls模块函数:**
- `default_tls_dir() -> PathBuf`: 返回默认TLS目录（`~/.omnish/tls/`）
- `load_or_create_cert(tls_dir: &Path) -> Result<(Vec<CertificateDer>, PrivateKeyDer)>`: 加载或生成自签名证书（cert.pem + key.pem，权限0600）
- `make_acceptor(tls_dir: &Path) -> Result<TlsAcceptor>`: 创建服务器TLS接受器
- `make_connector(cert_path: &Path) -> Result<TlsConnector>`: 创建客户端TLS连接器（信任指定的自签名证书）

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
**用途:** 建立连接并设置自动重连机制，支持指数退避重试。内部委托给`connect_with_reconnect_notify`，通知回调设为`None`。

### `RpcClient::connect_with_reconnect_notify()`
建立支持自动重连的连接，并在重连成功后发送通知。

**参数:**
- `addr: &str`: 服务器地址
- `tls_connector: Option<TlsConnector>`: TLS连接器（可选）
- `on_reconnect: Fn(&RpcClient) -> Future<Output = Result<()>>`: 重连回调函数
- `on_reconnect_notify: Option<impl Fn() + Send + Sync + 'static>`: 重连成功通知回调（可选）

**返回:** `Result<RpcClient>`
**用途:** 在`connect_with_reconnect`基础上增加重连成功通知机制。当重连成功并完成`on_reconnect`回调后，调用`on_reconnect_notify`通知调用方。典型用途是在UI层显示重连成功提示（如InlineNotice小部件）。

### `RpcClient::call()`
发送消息到服务器并等待单个响应。

**参数:** `msg: Message`
**返回:** `Result<Message>`
**用途:** 发送请求并等待单个响应，使用请求ID匹配响应

### `RpcClient::call_stream()`
发送消息到服务器并接收多个响应（用于流式传输）。

**参数:** `msg: Message`
**返回:** `Result<mpsc::Receiver<Message>>`
**用途:** 发送请求并接收多个响应消息流，支持agent循环等需要连续消息的场景。服务器发送Ack消息作为流结束标记。

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

**参数:**
- `handler: Fn(Message) -> Future<Output = Vec<Message>>` - 消息处理回调，返回响应消息列表
- `auth_token: Option<String>` - 认证令牌（Some时启用认证）
- `tls_acceptor: Option<TlsAcceptor>` - TLS接受器（Some时启用TLS，仅TCP）

**返回:** `Result<()>`
**用途:** 循环接受连接并为每个连接生成处理任务

**多消息响应支持:**
- 处理器可以返回多个消息（如agent循环中的工具状态更新和最终响应）
- 当返回多个消息时，服务器会自动发送Ack作为流结束标记
- 单消息响应不发送额外的Ack标记

**安全机制:**
- **Unix socket**: 绑定时设置权限0600，接受连接时验证peer UID必须与服务器进程相同
- **TCP + TLS**: 使用`tls_acceptor`对TCP连接进行TLS握手，握手失败则拒绝连接
- **认证**: 启用`auth_token`时，客户端必须在连接后5秒内发送`Auth`消息（携带`protocol_version`），令牌匹配返回`AuthOk`（携带服务器协议版本），不匹配返回`AuthFailed`并关闭连接

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

// 带自动重连和重连通知的连接
let client = RpcClient::connect_with_reconnect_notify(
    "/tmp/omnish.sock",
    None, // tls_connector
    |rpc| {
        Box::pin(async move {
            rpc.call(session_start_message).await?;
            Ok(())
        })
    },
    Some(|| {
        // 重连成功后的通知，例如触发UI提示
        println!("Reconnected to server");
    }),
).await?;

// 发送消息并等待单个响应
let response = client.call(message).await?;

// 发送消息并接收多个响应（流式）
let mut stream = client.call_stream(message).await?;
while let Some(msg) = stream.recv().await {
    println!("Received: {:?}", msg);
}

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

// 启动服务器处理连接（单消息响应）
server.serve(|msg| {
    Box::pin(async move {
        vec![match msg {
            Message::Request(req) => {
                // 处理请求并返回单个响应
                Message::Response(Response {
                    request_id: req.request_id.clone(),
                    content: format!("Echo: {}", req.query),
                    is_streaming: false,
                    is_final: true,
                })
            }
            _ => Message::Ack,
        }]
    })
}).await?;

// 启动服务器处理连接（多消息响应，如agent循环）
server.serve(|msg| {
    Box::pin(async move {
        match msg {
            Message::Request(req) => {
                // 返回多个消息：状态更新和最终响应
                vec![
                    Message::ChatToolStatus(ToolStatus {
                        tool_name: "calculator".to_string(),
                        status: "running".to_string(),
                    }),
                    Message::Response(Response {
                        request_id: req.request_id.clone(),
                        content: "Calculation complete".to_string(),
                        is_streaming: false,
                        is_final: true,
                    }),
                ]
            }
            _ => vec![Message::Ack],
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
   - `read_loop`: 接收响应，根据请求ID分发到对应的oneshot或mpsc通道
4. **请求ID管理**: 使用原子计数器生成唯一请求ID
5. **重连机制**: 使用指数退避算法自动重连，支持重连回调和重连成功通知
6. **响应分发**: 使用`ReplyTx`枚举支持单响应（oneshot）和多响应流（mpsc）两种模式

### 服务器内部结构
1. **连接接受**: 循环接受新连接
2. **任务生成**: 为每个连接生成独立的异步任务
3. **消息处理**: 读取消息帧，调用用户处理器，发送响应列表
4. **并发支持**: 每个连接独立处理，互不干扰
5. **多消息响应**: 处理器返回`Vec<Message>`，服务器依次发送所有消息，多消息响应（`Vec.len() > 1`）后自动追加Ack作为流结束标记

### 消息传输协议
1. **帧格式**: `[长度: u32][序列化帧数据]`
2. **请求-响应匹配**: 使用`request_id`关联请求和响应
3. **多消息流式传输**:
   - 服务器处理器可返回多个消息（`Vec<Message>`）
   - 所有消息使用相同的`request_id`发送
   - 多消息响应后，服务器自动发送Ack作为流结束标记
   - 客户端接收到Ack时，从pending映射中移除该请求ID，结束流接收
4. **错误处理**: 连接断开时清理挂起的请求

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

### 多消息流式传输机制

从commit 6700a42开始，omnish-transport支持服务器返回多个响应消息的流式传输，主要用于agent循环等需要发送连续消息的场景。

**关键变更:**
1. **服务器处理器签名变更**: `Fn(Message) -> Future<Output = Message>` → `Fn(Message) -> Future<Output = Vec<Message>>`
2. **新增客户端API**: `RpcClient::call_stream()` 返回 `mpsc::Receiver<Message>`，用于接收多个响应
3. **ReplyTx枚举**: 内部使用枚举支持两种模式：
   - `Once(oneshot::Sender)`: 用于`call()`的单响应模式
   - `Stream(mpsc::Sender)`: 用于`call_stream()`的多响应流模式

**流结束标记（commit b8827d9）:**
- 问题：多消息响应时，客户端无法判断何时结束接收，导致pending映射中的流条目无法清理，造成内存泄漏
- 解决：服务器在发送完所有响应消息后（当`Vec.len() > 1`时），自动追加一个`Message::Ack`作为流结束标记
- 客户端接收到Ack时，从pending映射中移除该请求ID的流条目，关闭mpsc发送端，使`call_stream()`返回的接收器结束

**工作流程:**
1. 客户端调用`call_stream(msg)`，创建mpsc通道，将请求ID和发送端存入pending映射
2. 服务器处理器返回`Vec<Message>`（如`[ToolStatus, Response]`）
3. 服务器依次发送所有消息，每个消息使用相同的`request_id`
4. 如果返回消息数量>1，服务器自动发送`Message::Ack`作为结束标记
5. 客户端read_loop接收到每个消息，通过mpsc发送端转发给调用者
6. 接收到Ack时，客户端移除pending映射条目，关闭发送端
7. 调用者的`stream.recv()`返回`None`，结束循环

**使用场景:**
- agent循环：发送工具执行状态（`ChatToolStatus`）和最终响应（`Response`）
- 长时间操作的进度更新：先发送进度消息，最后发送完成消息
- 批量数据传输：分多次发送大量数据

### 协议版本协商

omnish-transport在认证握手阶段进行协议版本协商，用于检测客户端和服务器之间的协议版本不一致。

**机制:**
- 协议版本号由`omnish_protocol::message::PROTOCOL_VERSION`常量定义
- 客户端在发送`Auth`消息时携带自身的`protocol_version`字段
- 服务器认证成功后返回`AuthOk`消息（替代原来的`Ack`），其中包含服务器的`protocol_version`
- 若客户端和服务器版本不一致，服务器记录warning日志，但不拒绝连接（允许兼容运行）

**相关数据结构:**
```rust
pub struct Auth {
    pub token: String,
    #[serde(default)]
    pub protocol_version: u32,
}

pub struct AuthOk {
    pub protocol_version: u32,
}
```

**设计考虑:**
- `Auth.protocol_version`使用`#[serde(default)]`标注，确保旧版本客户端（不发送版本号）仍能正常连接
- 版本不匹配仅产生warning而非拒绝连接，支持渐进式升级场景
- 客户端可以根据`AuthOk`中返回的服务器版本号决定后续行为

### 重连成功通知机制

`connect_with_reconnect_notify`方法在`connect_with_reconnect`基础上增加了可选的重连成功通知回调`on_reconnect_notify`。

**工作流程:**
1. 连接断开，`reconnect_loop`开始指数退避重试
2. 重连成功后，先执行`on_reconnect`回调（如重新注册会话）
3. 将新的连接内部状态替换到客户端
4. 调用`on_reconnect_notify`回调通知调用方重连已完成

**用途:**
该回调用于通知UI层显示重连成功提示。`on_reconnect`回调负责重建连接状态（如重新发送认证和会话注册），而`on_reconnect_notify`回调用于触发用户可见的通知（如显示InlineNotice小部件）。两者职责分离：前者处理传输层恢复，后者处理用户通知。

## 安全模型

### Unix Socket安全
- 绑定时设置文件权限为0600（仅所有者可读写）
- 接受连接时验证peer UID，拒绝非同一用户的连接
- 提供操作系统级别的进程隔离

### TCP安全
- 支持TLS加密（自签名证书，存储于`~/.omnish/tls/`）
- 证书和密钥文件权限0600
- 客户端通过`make_connector`信任守护进程的自签名证书

### 认证流程
1. 守护进程启动时加载或创建认证令牌（`~/.omnish/auth_token`）
2. 客户端连接后必须在5秒内发送`Auth`消息（携带`token`和`protocol_version`）
3. 令牌匹配: 服务器返回`AuthOk`（携带服务器协议版本），进入正常消息循环；若协议版本不一致，记录warning日志
4. 令牌不匹配: 服务器返回`AuthFailed`并关闭连接
5. 超时: 服务器关闭连接

## 依赖关系
- **omnish-protocol**: 消息类型定义和序列化
- **tokio**: 异步运行时、网络I/O和同步原语
- **anyhow**: 错误处理
- **tracing**: 日志记录
- **nix**: Unix系统调用（peer UID验证）
- **tokio-rustls**: 异步TLS支持
- **rustls**: TLS协议实现
- **rustls-pemfile**: PEM文件解析
- **rcgen**: 自签名证书生成
- **std::sync**: 原子操作和同步原语

## 测试覆盖
模块包含全面的测试用例：
- 基本请求-响应测试
- 并发请求处理测试
- 自动重连机制测试
- 连接状态监控测试
- Unix socket和TCP协议测试
- 多客户端并发测试
- 认证成功/失败/超时测试
- TLS连接测试
