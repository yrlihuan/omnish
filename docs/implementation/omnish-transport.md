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
- 协议版本校验
- EMFILE（文件描述符耗尽）诊断
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

### `PermanentFailure` 错误类型
用于从`on_reconnect`回调中返回，表示永久性失败（如认证被拒绝、协议版本不匹配）。重连循环在连续收到多次永久性失败后放弃重连。
```rust
pub struct PermanentFailure(pub String);
```

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
**用途:** 在`connect_with_reconnect`基础上增加重连成功通知机制。内部委托给`connect_with_reconnect_full`，`on_disconnect`设为`None`。

### `RpcClient::connect_with_reconnect_full()`
建立支持自动重连的连接，提供完整的断开/重连回调支持。

**参数:**
- `addr: &str`: 服务器地址
- `tls_connector: Option<TlsConnector>`: TLS连接器（可选）
- `on_reconnect: Fn(&RpcClient) -> Future<Output = Result<()>>`: 重连回调函数
- `on_reconnect_notify: Option<impl Fn() + Send + Sync + 'static>`: 重连成功通知回调（可选）
- `on_disconnect: Option<impl Fn() + Send + Sync + 'static>`: 断开连接通知回调（可选）

**返回:** `Result<RpcClient>`
**用途:** 在`connect_with_reconnect_notify`基础上增加断开连接即时通知。当连接断开时，先调用`on_disconnect`通知调用方，然后开始重连循环。典型用途是在UI层显示连接断开提示和重连成功提示。

### `RpcClient::call()`
发送消息到服务器并等待单个响应。

**参数:** `msg: Message`
**返回:** `Result<Message>`
**用途:** 发送请求并等待单个响应，使用请求ID匹配响应

### `RpcClient::send()`
以 fire-and-forget 模式发送消息，不等待响应。

**参数:** `msg: Message`
**返回:** `Result<()>`
**用途:** 发送不需要响应的消息（如 CompletionSummary、SessionEnd 等），避免阻塞调用者的事件循环。与 `call()` 的区别在于不注册 reply 通道，消息发送后立即返回，不等待服务器响应。

**使用场景：**
- `SessionEnd` — 会话结束通知
- `CompletionSummary` — 补全采样数据上报
- `send_or_buffer()` 中的非关键消息
- 任何只需单向通知、不需要响应的消息

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
- `handler: Fn(Message, mpsc::Sender<Message>) -> Future<Output = ()>` - 消息处理回调，通过 `mpsc::Sender<Message>` 发送响应消息
- `auth_token: Option<String>` - 认证令牌（Some时启用认证）
- `tls_acceptor: Option<TlsAcceptor>` - TLS接受器（Some时启用TLS，仅TCP）

**返回:** `Result<()>`
**用途:** 循环接受连接并为每个连接生成处理任务

**真正的流式传输支持:**
- 处理器接收一个 `mpsc::Sender<Message>`，可在任意时刻通过 `tx.send(msg).await` 发送消息，实现真正的流式传输
- 服务器在处理器完成（`tx` 被 drop）后统计已发送消息数量：若发送了多条消息（`count > 1`），自动追加一个 `Message::Ack` 作为流结束标记
- 单消息响应不发送额外的 Ack 标记

**安全机制:**
- **Unix socket**: 绑定时设置权限0600，接受连接时验证peer UID必须与服务器进程相同
- **TCP + TLS**: 使用`tls_acceptor`对TCP连接进行TLS握手，握手失败则拒绝连接
- **认证**: 启用`auth_token`时，客户端必须在连接后5秒内发送`Auth`消息（携带`protocol_version`），令牌匹配且协议版本一致返回`AuthResult { ok: true }`（携带服务器协议版本和守护进程版本），令牌不匹配或协议版本不一致返回`AuthResult { ok: false }`并关闭连接

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

// 带断开通知和重连通知的完整连接
let client = RpcClient::connect_with_reconnect_full(
    "/tmp/omnish.sock",
    None, // tls_connector
    |rpc| {
        Box::pin(async move {
            rpc.call(session_start_message).await?;
            Ok(())
        })
    },
    Some(|| {
        println!("Reconnected to server");
    }),
    Some(|| {
        // 连接断开时的即时通知
        println!("Disconnected from server");
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
server.serve(|msg, tx| {
    Box::pin(async move {
        let reply = match msg {
            Message::Request(req) => {
                // 处理请求并发送单个响应
                Message::Response(Response {
                    request_id: req.request_id.clone(),
                    content: format!("Echo: {}", req.query),
                    is_streaming: false,
                    is_final: true,
                })
            }
            _ => Message::Ack,
        };
        let _ = tx.send(reply).await;
    })
}).await?;

// 启动服务器处理连接（流式多消息响应，如agent循环）
server.serve(|msg, tx| {
    Box::pin(async move {
        if let Message::Request(req) = msg {
            // 立即发送工具状态更新
            let _ = tx.send(Message::ChatToolStatus(ToolStatus {
                tool_name: "calculator".to_string(),
                status: "running".to_string(),
            })).await;
            // 处理完成后发送最终响应
            let _ = tx.send(Message::Response(Response {
                request_id: req.request_id.clone(),
                content: "Calculation complete".to_string(),
                is_streaming: false,
                is_final: true,
            })).await;
            // tx 在此处 drop，服务器自动追加 Ack 作为流结束标记
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
   - `read_loop`: 接收响应，根据请求 ID 分发到对应的 oneshot 或 mpsc 通道；帧解析失败时记录 warning 并跳过
4. **请求ID管理**: 使用原子计数器生成唯一请求ID
5. **重连机制**: 使用指数退避算法自动重连，支持重连回调、重连成功通知和断开连接通知；连续永久性失败（如认证拒绝）达到阈值后放弃重连
6. **响应分发**: 使用`ReplyTx`枚举支持单响应（oneshot）和多响应流（mpsc）两种模式
7. **锁作用域优化**: `call()`、`send()`、`call_stream()` 方法在持有内部锁期间仅检查连接状态并克隆 `tx`，随后立即释放锁，再执行可能阻塞的 `tx.send()` 操作，避免锁持有期间阻塞
8. **流通道背压**: `read_loop` 中对 Stream 类型的消息分发先释放 pending 锁再发送，通道满时使用背压（`await`）等待消费者处理，而非丢弃消息

### 服务器内部结构
1. **连接接受**: 循环接受新连接
2. **任务生成**: 为每个连接生成独立的异步任务
3. **消息处理**: 读取消息帧，创建内部 `mpsc` 通道，将 `tx` 传给处理器，处理器通过 `tx` 异步发送消息
4. **并发支持**: 每个连接独立处理，互不干扰；每个请求另起独立任务运行处理器，网络写入循环与处理器并发执行
5. **流式写入**: 服务器不等待处理器完成，而是边接收边写入——处理器通过 `tx` 发出的消息立即转发给客户端
6. **流结束标记**: 处理器完成后（`tx` drop），统计已发送消息数，若 `count > 1` 则追加 `Message::Ack` 作为流结束标记
7. **EMFILE处理**: 当`accept()`返回EMFILE（errno 24）或ENFILE（errno 23）错误时，调用`dump_fd_stats()`输出fd诊断信息后返回错误，而非静默崩溃

### 消息传输协议
1. **帧格式**: `[长度: u32][序列化帧数据]`
2. **请求-响应匹配**: 使用`request_id`关联请求和响应
3. **多消息流式传输**:
   - 服务器通过 `mpsc::Sender<Message>` 实时发送消息
   - 所有消息使用相同的 `request_id` 发送
   - 多消息响应后，服务器自动发送 Ack 作为流结束标记
   - 客户端接收到 Ack 时，从 pending 映射中移除该请求 ID，结束流接收
4. **帧解析错误处理**: 帧反序列化失败时记录 warning 日志（含帧长度和错误信息）并跳过该帧，不断开连接
5. **错误处理**: 连接断开时清理挂起的请求

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

omnish-transport 支持服务器向客户端推送多个响应消息的真正流式传输，主要用于 agent 循环等需要发送连续消息的场景。

**处理器签名演进:**
1. 初始版本：`Fn(Message) -> Future<Output = Message>`（单消息）
2. 中间版本（commit 6700a42）：`Fn(Message) -> Future<Output = Vec<Message>>`（批量多消息）
3. **当前版本（commit 09ed9ea）**：`Fn(Message, mpsc::Sender<Message>) -> Future<Output = ()>`（真正流式）

当前版本的关键改进在于：处理器不再需要等待全部工作完成后才返回结果列表，而是在产生每条消息时立即通过 `tx.send()` 发送，客户端能实时收到每条消息，无需等待整个 agent 循环结束。

**客户端 API:**
- `RpcClient::call_stream()` 返回 `mpsc::Receiver<Message>`，用于接收多个响应
- **ReplyTx枚举**: 内部使用枚举支持两种模式：
  - `Once(oneshot::Sender)`: 用于`call()`的单响应模式
  - `Stream(mpsc::Sender)`: 用于`call_stream()`的多响应流模式

**流通道容量与背压:**
- `call_stream()` 创建的 mpsc 通道容量为 128（从原来的 16 增大，避免频繁触发背压）
- `read_loop` 在分发 Stream 消息时，先释放 pending 锁，再通过 `try_send` 尝试非阻塞发送
- 通道满时（`TrySendError::Full`），记录 debug 日志后使用 `tx.send().await` 等待消费者处理（背压），而非丢弃消息
- 通道关闭时（`TrySendError::Closed`），从 pending 映射中移除该请求 ID
- 此设计避免了旧版本中 `try_send` 失败时直接丢弃消息和移除流条目的问题，该问题曾导致下载不完整

**流结束标记:**
- 服务器在处理器完成（`tx` drop）后，若 `count > 1` 则自动追加一个 `Message::Ack` 作为流结束标记
- 客户端接收到 Ack 时，从 pending 映射中移除该请求 ID 的流条目，关闭 mpsc 发送端，使 `call_stream()` 返回的接收器结束

**工作流程:**
1. 客户端调用 `call_stream(msg)`，创建 mpsc 通道（容量128），将请求 ID 和发送端存入 pending 映射
2. 服务器为每个请求创建内部 `mpsc::channel`，将 `tx` 传给处理器并独立 spawn
3. 服务器写入循环通过 `rx.recv()` 接收消息，有消息立即写入连接（真正流式）
4. 处理器通过 `tx.send()` 依次发送消息（如 `ToolStatus`、`Response`）
5. 处理器完成，`tx` drop，写入循环结束，若 `count > 1` 追加 `Ack`
6. 客户端 read_loop 接收到每个消息，释放 pending 锁后通过 mpsc 发送端转发给调用者（通道满时背压等待）
7. 接收到 Ack 时，客户端移除 pending 映射条目，关闭发送端
8. 调用者的 `stream.recv()` 返回 `None`，结束循环

**使用场景:**
- agent 循环：工具执行时立即发送 `ChatToolStatus`，完成后发送 `Response`
- 长时间操作的进度更新：先发送进度消息，最后发送完成消息
- 批量数据传输：分多次发送大量数据

### 协议版本校验

omnish-transport在认证握手阶段进行协议版本校验，协议版本不一致的客户端将被拒绝连接。

**机制:**
- 协议版本号由`omnish_protocol::message::PROTOCOL_VERSION`常量定义
- 客户端在发送`Auth`消息时携带自身的`protocol_version`字段
- 服务器认证成功后返回`AuthResult`消息，其中包含`ok`（是否通过）、服务器`protocol_version`和`daemon_version`
- 令牌匹配且协议版本一致时，`ok=true`，进入正常消息循环
- 令牌匹配但协议版本不一致时，`ok=false`，服务器记录warning日志；连接保持打开以允许客户端获取更新信息
- 令牌不匹配时，`ok=false`，服务器关闭连接

**相关数据结构:**
```rust
pub struct Auth {
    pub token: String,
    #[serde(default)]
    pub protocol_version: u32,
}

pub struct AuthResult {
    pub ok: bool,
    pub protocol_version: u32,
    #[serde(default)]
    pub daemon_version: String,
}
```

**设计考虑:**
- `Auth.protocol_version`使用`#[serde(default)]`标注，确保旧版本客户端（不发送版本号）仍能正常连接
- 协议版本不匹配时返回`ok=false`，但保持连接打开，允许客户端通过该连接获取更新所需信息（如守护进程版本号），支持跨版本升级场景
- `AuthResult.daemon_version`携带守护进程版本号，客户端可据此触发更新检查

### 重连机制与永久失败终止

`reconnect_loop`方法管理断开连接后的重连尝试，支持指数退避、断开通知、重连通知和永久失败终止。

**工作流程:**
1. 连接断开，调用`on_disconnect`回调（如果提供）通知调用方
2. 标记连接状态为断开
3. 开始指数退避重试（初始1秒，最大30秒）
4. 重连成功后，执行`on_reconnect`回调（如重新注册会话和认证）
5. 回调成功：将新连接替换到客户端，调用`on_reconnect_notify`通知调用方
6. 回调返回`PermanentFailure`错误：递增连续永久失败计数器
7. 连续永久失败达到5次：记录warning日志并放弃重连，退出循环
8. 连接失败（daemon可能未运行）：重置永久失败计数器，继续重试

**`PermanentFailure`错误类型:**
`on_reconnect`回调可以返回`PermanentFailure`错误来表示不可恢复的失败（如认证被拒绝、协议版本不匹配）。这与普通错误（如网络超时、SessionStart失败）不同——普通错误会无限重试，而永久失败会累计计数。

**连接层面 vs 回调层面的区分:**
- 连接失败（`connector()` 返回错误）表示daemon可能未运行，重置永久失败计数器
- 回调失败中的普通错误：继续重试，不累计永久失败
- 回调失败中的`PermanentFailure`：累计计数，达阈值放弃

**用途:**
解决旧版客户端（协议版本不匹配）在连接被拒后无限重试的问题。服务器拒绝后，客户端的`on_reconnect`回调返回`PermanentFailure`，5次后客户端放弃重连。

### EMFILE（文件描述符耗尽）处理

当服务器的`accept()`调用因文件描述符耗尽而失败时，transport层会输出详细的fd诊断信息以便排查问题。

**触发条件:**
- EMFILE（errno 24）：进程级文件描述符限制
- ENFILE（errno 23）：系统级文件描述符限制

**诊断内容（`dump_fd_stats()`）:**
- 读取`/proc/self/fd`目录，统计当前进程所有打开的文件描述符
- 按类型分类统计：socket、pipe、anon_inode、device、file、other
- 每种类型输出数量和最多5个样本路径
- 通过`getrlimit(RLIMIT_NOFILE)`获取soft/hard限制
- 所有信息通过`tracing::error!`输出

**处理流程:**
1. `accept()`返回错误
2. 检查是否为EMFILE/ENFILE（`is_fd_exhausted()`）
3. 如果是：调用`dump_fd_stats()`输出诊断信息，然后返回错误
4. 如果不是：直接返回错误

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
3. 令牌匹配且版本一致: 服务器返回`AuthResult { ok: true }`（携带服务器协议版本和守护进程版本），进入正常消息循环
4. 令牌匹配但版本不一致: 服务器返回`AuthResult { ok: false }`，记录warning日志，保持连接打开（允许客户端获取更新信息）
5. 令牌不匹配: 服务器返回`AuthResult { ok: false }`并关闭连接
6. 超时: 服务器关闭连接

## 依赖关系
- **omnish-protocol**: 消息类型定义和序列化
- **omnish-common**: 版本号常量（`VERSION`）
- **tokio**: 异步运行时、网络I/O和同步原语
- **anyhow**: 错误处理
- **tracing**: 日志记录
- **nix**: Unix系统调用（peer UID验证、getrlimit）
- **tokio-rustls**: 异步TLS支持
- **rustls**: TLS协议实现
- **rustls-pemfile**: PEM文件解析
- **rcgen**: 自签名证书生成
- **std::sync**: 原子操作和同步原语

## 已知问题与修复

### ChatReady 反序列化失败导致 15 秒超时（已修复）

**问题根因:** `ChatReady.history` 字段原类型为 `Option<Vec<serde_json::Value>>`，而 bincode 不支持反序列化 `serde_json::Value`（该类型调用 `deserialize_any`）。帧反序列化失败后被静默丢弃，客户端等待响应直到 15 秒超时。

**修复方案:**
- 将 `ChatReady.history` 改为 `Vec<String>`，每个元素为 JSON 编码的字符串，客户端接收后再解码
- 同样的修复应用于 `CompletionSummary.extra` 和 `CompletionRecord.extra`（`HashMap<String,Value>` → `HashMap<String,String>`）
- transport 层增加了帧解析错误日志：解析失败时记录 warning（含帧长度和错误信息），不再静默丢弃

**影响范围:** 此修复涉及 omnish-protocol 中的数据结构变更，transport 层的改进（错误日志）确保类似问题未来能被及时发现。

### 流通道满时丢弃消息导致下载不完整（已修复）

**问题根因:** `read_loop`中对Stream类型消息使用`try_send`发送，通道满时直接移除该请求ID的流条目并丢弃消息，导致更新下载等场景下数据不完整。

**修复方案:**
- 将通道容量从16增大到128，减少通道满的概率
- `read_loop`中Stream消息分发改为先释放pending锁，再尝试发送
- 通道满时使用`tx.send().await`背压等待，而非丢弃消息
- 通道关闭时才移除pending条目

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
- bincode 往返测试（`ChatReady` 含历史记录、`CompletionSummary` 含非空 extra 映射）
