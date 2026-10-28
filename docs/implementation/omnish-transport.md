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
- `push_rx`: 服务器主动推送消息的接收通道（`mpsc::Receiver<Message>`），用于接收 `ConfigClient` 等推送消息
- `suppress_until`: 重连抑制时间戳（epoch millis），用于 `/test disconnect` 延迟重连
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
- 支持服务器主动推送消息到客户端

**内部结构:**
- `listener`: 监听器（Unix或TCP）
- 为每个连接生成独立的异步任务，分配唯一 `conn_id`

### `PushRegistry` / `OnPushConnect` / `OnDisconnect`
服务器推送与断连回调的核心类型：
- `PushRegistry`: `Arc<Mutex<HashMap<u64, mpsc::Sender<Message>>>>`，按连接 ID 维护推送通道
- `OnPushConnect`: 新连接注册到 PushRegistry 后的回调，用于发送初始消息（如当前客户端配置）
- `OnDisconnect`: 每连接任务即将退出时的回调，接收 transport 分配的 `conn_id`，供上层清理以 conn_id 为键的状态（如客户端未发 SessionEnd 就断开时结束其会话）
- 连接断开时自动从 PushRegistry 中移除

### `CONN_ID` task_local
`tokio::task_local!` 定义的连接 ID 上下文。serve() 为每个消息处理任务通过 `CONN_ID.scope(conn_id, ...)` 注入，使处理器（及其 await 的内容）可用 `CONN_ID.try_with(|id| *id)` 取回消息所属连接 ID，处理器签名保持不变。仅在 serve() 派生的处理器内可用。

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
- `on_reconnect_error: Option<impl Fn(&str) + Send + Sync + 'static>`: 每次重连尝试失败时以失败描述调用（可选），客户端把它写进事件日志

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
- `SessionEnd` - 会话结束通知
- `CompletionSummary` - 补全采样数据上报
- 客户端退出时的非关键消息（按键遥测等热路径改用 `try_send()`）
- 任何只需单向通知、不需要响应的消息

### `RpcClient::try_send()`
非阻塞的 fire-and-forget 发送（#656）。

**参数:** `msg: Message`
**返回:** `Result<()>`
**行为:** 绝不等待网络 I/O 或背压：用 `try_lock` 取内部锁、用 `try_send` 放入写队列；连接已断开、内部锁被占用或写队列已满时立即返回 `Err`
**用途:** 热路径上必须永不阻塞调用者的消息，主要是客户端主循环每次按键上报的 `IoData` / `CommandComplete`（经客户端 `report_or_buffer()` 调用，失败时退回缓冲，极端背压下丢弃遥测）。阻塞发送会让主循环停止读取 stdin，内核 tty 输入缓冲溢出后按键被静默丢弃

### `RpcClient::force_disconnect()`
主动拆掉当前连接，效果等同对端关闭（#689）。

**行为:**
- 标记 `connected = false`
- 通过 `Inner.shutdown`（`tokio::sync::Notify`，`notify_one` 会保存许可，读循环处于两次读取之间也能收到）唤醒读循环；读循环读帧时与它 `select!`，收到后走正常退出路径：清空挂起的回复（挂起的 `call()` 立即返回错误）并发出断开信号，启用重连的客户端随即进入退避重连
- abort 写任务：它可能正阻塞在死 socket 已满的发送缓冲上，不 abort 会泄漏到内核放弃为止

**用途:** 调用方已有证据表明连接已死、而操作系统还没察觉时使用，例如客户端心跳等不到 Ack（静默丢包的路径要好几分钟才会产生 socket 错误）

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
- `push_registry: Option<PushRegistry>` - 推送通道注册表（Some时启用服务器主动推送）
- `on_push_connect: Option<OnPushConnect>` - 新连接注册后的回调（用于发送初始推送消息）
- `on_disconnect: Option<OnDisconnect>` - 每连接任务退出前的回调，接收 `conn_id`（用于上层清理以 conn_id 为键的状态，如客户端无 SessionEnd 断开时结束会话）

**返回:** `Result<()>`
**用途:** 循环接受连接并为每个连接生成处理任务

**真正的流式传输支持:**
- 处理器接收一个 `mpsc::Sender<Message>`，可在任意时刻通过 `tx.send(msg).await` 发送消息，实现真正的流式传输
- 服务器在处理器完成（`tx` 被 drop）后统计已发送消息数量：若发送了多条消息（`count > 1`），自动追加一个 `Message::Ack` 作为流结束标记
- 单消息响应不发送额外的 Ack 标记

**安全机制:**
- **Unix socket**: 绑定时设置权限0600，接受连接时验证peer UID必须与服务器进程相同
- **TCP + TLS**: 使用`tls_acceptor`对TCP连接进行TLS握手，握手失败则拒绝连接
- **认证**: 启用`auth_token`时，客户端必须在连接后5秒内发送`Auth`消息（携带`protocol_version`），令牌匹配且版本兼容返回`AuthResult { ok: true }`，令牌不匹配或版本不兼容返回`AuthResult { ok: false }`
- **推送**: 启用`push_registry`时，认证后为连接分配唯一 `conn_id` 并注册推送通道；推送消息由**独立任务**写出，而不是在读循环的 `tokio::select!` 中内联写出，这样客户端读得慢时也不会卡住守护进程的读循环（#656）；推送写入失败时发出断开信号，让连接拆除并由客户端重连；连接断开时自动清理
- **TestDisconnect**: 收到 `TestDisconnect` 消息后延迟指定秒数断开连接，用于测试客户端断线恢复
- **帧跳过**: `read_frame_bytes` 先读取原始字节，反序列化失败时跳过该帧（warn 日志）而非断开连接，支持与更新版本对端通信

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
9. **服务器推送非阻塞分发**: `read_loop` 用 `try_send` 分发 `request_id = 0` 的服务器推送；推送通道已满时记录 warn 并丢弃该推送，而不是阻塞读循环（阻塞会让响应也读不到，与对端的阻塞写一起形成死锁，#656）

### 服务器内部结构
1. **连接接受**: 循环接受新连接
2. **任务生成**: 为每个连接生成独立的异步任务
3. **消息处理**: 读取消息帧，创建内部 `mpsc` 通道，将 `tx` 传给处理器，处理器通过 `tx` 异步发送消息
4. **并发支持**: 每个连接独立处理，互不干扰；每个请求另起独立任务运行处理器，网络写入循环与处理器并发执行
5. **流式写入**: 服务器不等待处理器完成，而是边接收边写入--处理器通过 `tx` 发出的消息立即转发给客户端
6. **流结束标记**: 处理器完成后（`tx` drop），统计已发送消息数，若 `count > 1` 则追加 `Message::Ack` 作为流结束标记
7. **EMFILE处理**: 当`accept()`返回EMFILE（errno 24）或ENFILE（errno 23）错误时，调用`dump_fd_stats()`输出fd诊断信息后返回错误，而非静默崩溃
8. **推送写出任务**: 每个启用推送的连接有一个专门的任务消费推送通道并写给客户端，与读循环解耦（#656）

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

omnish-transport在认证握手阶段进行协议版本兼容性检查，使用 `versions_compatible(MIN_COMPATIBLE_VERSION, peer_version)` 判断对端是否在兼容范围内。

**机制:**
- 协议版本号由 `PROTOCOL_VERSION`（当前版本）和 `MIN_COMPATIBLE_VERSION`（最低兼容版本）两个常量定义
- 客户端在发送`Auth`消息时携带自身的`protocol_version`字段
- 服务器检查 `peer_version >= MIN_COMPATIBLE_VERSION`：兼容时 `ok=true`（即使版本不完全相同），不兼容时 `ok=false`
- 令牌不匹配时，`ok=false`，服务器关闭连接
- 帧反序列化失败时优雅跳过（graceful frame skip），允许忽略未知消息类型

**日志行为:**
- 版本兼容但不同：记录 info 日志（`protocol compatible`）
- 版本不兼容：记录 warn 日志（`protocol incompatible`）

**设计考虑:**
- `Auth.protocol_version`使用`#[serde(default)]`标注，确保旧版本客户端（不发送版本号）仍能正常连接
- 协议版本不匹配时返回`ok=false`，但保持连接打开，允许客户端通过该连接获取更新所需信息
- 新消息变体追加到枚举末尾 + frame skip 机制，使得版本在兼容范围内的新旧客户端可以共存
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
`on_reconnect`回调可以返回`PermanentFailure`错误来表示不可恢复的失败（如认证被拒绝、协议版本不匹配）。这与普通错误（如网络超时、SessionStart失败）不同--普通错误会无限重试，而永久失败会累计计数。

**连接层面 vs 回调层面的区分:**
- 连接失败（`connector()` 返回错误）表示daemon可能未运行，重置永久失败计数器
- 回调失败中的普通错误：继续重试，不累计永久失败
- 回调失败中的`PermanentFailure`：累计计数，达阈值放弃

**用途:**
解决旧版客户端（协议版本不匹配）在连接被拒后无限重试的问题。服务器拒绝后，客户端的`on_reconnect`回调返回`PermanentFailure`，5次后客户端放弃重连。

**超时与失败上报（防止重连循环卡死）:**
- `CONNECT_TIMEOUT`（10 秒，`lib.rs`）：`make_connector` 的 TCP 连接加 TLS 握手整体限时。此前握手没有超时，网络切换时握手包被静默丢弃会让某一次尝试一直挂起，直到 TCP 放弃；macOS 上实测网络恢复后 12 分钟以上仍未重连
- 客户端 `on_reconnect` 回调中的 Auth、SessionStart 和缓存消息重放都经 `call_bounded()` 限时 `CONNECT_TIMEOUT`，新连接在握手后立即失联也不会卡住
- 每次失败（`connect` 或 `register` 阶段）都 `tracing::warn!` 并调用 `on_reconnect_error`，格式 `reconnect attempt N failed (<阶段>): <错误>`，`/debug events` 可见
- `set_keepalive()`：客户端与服务端的 TCP socket 都开启 keepalive（空闲 30 秒开始探测，间隔 10 秒），对端掉线后半开连接能被发现，不再一直保持 ESTABLISHED

### TLS 握手不阻塞 accept

服务端 TCP 监听循环接受连接后，TLS 握手放到独立任务中执行并限时 `CONNECT_TIMEOUT`，超时或失败只记 warning。此前握手在 accept 循环里直接 await，一个握手中途失联的客户端会阻塞所有其他远程客户端的连接。accept 后 `set_nodelay` 失败也只跳过该连接，不再让 `serve` 返回错误。

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

### 双向流控死锁导致按键被吞（已修复，#656）

**现象:** 守护进程连接不稳定之后，某个客户端会持续地丢失大部分按键（每几下只有一下到达 shell），而同一主机上的其他客户端正常。

**问题根因:** 客户端和守护进程都在读/泵循环中内联执行阻塞写。某个方向的发送缓冲满了（对端读得慢）时，阻塞写让这一侧停止读取，进而卡住另一个方向，形成环路。发送缓冲满只会挂起、不会报错，所以连接永远不会被判定为断开，也不会重连，网络恢复后也无法自愈。客户端主循环卡住后不再读取 stdin，内核 tty 输入缓冲溢出，多出的按键被内核静默丢弃。

**修复方案:** 解开环路上每一处"在读/泵循环中内联阻塞写"：
- 客户端主循环的按键遥测改用非阻塞的 `report_or_buffer()`（基于新增的 `RpcClient::try_send()`），主循环永不阻塞
- 客户端 `read_loop` 用 `try_send` 分发服务器推送，通道满时丢弃推送而非阻塞
- 服务器把推送写出移到每连接独立的任务，并在推送写入失败时发出断开信号

### 网络路径静默断开后长时间无法发现（已修复，#689）

**现象:** 路由切换（如 Tailscale 路径改走 VM）后，客户端按 `:` 进入聊天卡死，连 Ctrl-C 都无效，好几分钟后才自行恢复。

**问题根因:** 旧路径静默丢包、不回 RST/FIN，socket 要等 TCP 重传放弃才报错。传输层没有任何存活检测（无心跳超时、无 TCP keepalive、无读超时），所以这期间 `connected` 一直为 true，不会重连，所有 `call()` 既不返回也不报错。客户端进入聊天时第一步就是一个没有超时的 `call()`，而此时还没有代码读取 stdin。守护进程日志显示 10 个会话的重连分散在 6 分钟内，各自等到重传超时才发现，守护进程也从未记录旧连接断开。

**修复方案:**
- 新增 `RpcClient::force_disconnect()`，读循环读帧时与 shutdown 通知 `select!`
- 客户端轮询任务的 `SessionUpdate` 改为带 10 秒超时的心跳 `call()`，超时即 `force_disconnect()`；轮询间隔上限从 60 秒降到 30 秒
- 进入聊天时的 cwd 同步 `call()` 限时 3 秒

**测试:** 用可"黑洞"的 TCP 代理（停止转发但保持 socket 打开）复现：未修复时挂起的 `call()` 不返回、`is_connected()` 保持 true；`force_disconnect()` 后挂起调用 1 秒内失败，路径恢复后自动重连

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
