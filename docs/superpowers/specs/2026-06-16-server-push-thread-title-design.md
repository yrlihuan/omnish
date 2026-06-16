# 通用 daemon -> client 推送消息(ServerPush)设计

日期:2026-06-16
状态:待实现
相关 issue:#644(stdout 统一写出对象,后续工作)

## 1. 目标与范围

为 daemon 主动向 client 推送内容建立一个**通用、可扩展**的消息类型 `ServerPush`,首个用例是 `thread_title_update`(后台 `thread_summary` 任务生成新 `title_word` 后,实时刷新聊天 tmux 窗口标题)。

当前痛点:`thread_summary` 生成的 `title_word` 只在下次 resume 该 thread 时才反映到窗口标题(见 `main.rs` 中 `set_chat_tmux_title` 上方 TODO)。daemon 缺少一条在聊天进行中把标题更新推给 client 的通道。

**范围内:**
- 新增协议消息 `Message::ServerPush { kind, payload }`。
- daemon 在 `thread_summary` 生成新 `title_word` 后广播该消息。
- client 端常驻 dispatcher task 接收并安全地刷新 tmux 标题。

**范围外(YAGNI):**
- 服务端定向投递(按 conn_id 精确推送)。本设计用广播 + 客户端过滤。
- `/thread rename` 向其他客户端广播(rename 已在发起端本地即时更新)。
- stdout 统一写出对象的彻底重构(记录在 issue #644,本设计用折中方案规避竞争)。

## 2. 关键设计决策

1. **消息形态:通用信封 `kind: String + payload: serde_json::Value`。** 将来新增推送类型无需改协议版本,只需约定新的 `kind`。代价是失去编译期类型安全、需手动解析 JSON;对于低频、非核心路径的推送可接受。
2. **投递方式:广播 + 客户端过滤。** 复用现有 `broadcast_notice` 模式(遍历 `PushRegistry` 群发)。payload 携带 `thread_id`,客户端按自己的 `current_thread_id` 过滤。无需在 daemon 侧维护 `thread_id -> conn_id` 映射。
3. **客户端:独立常驻 dispatcher task 作为 `push_rx` 唯一消费者。** 聊天模式会阻塞 shell 主循环(两层架构),主循环期间无法 drain push。常驻 task 让推送在 shell 与 chat 模式下都能被接收。
4. **竞争规避:接收与字节写出分离。** dispatcher **不直接写 stdout**;它只把新标题写入共享槽。真正的 tmux 标题转义字节由唯一的渲染线程在安全点写出,避免多线程把转义序列写交错(详见 §6)。

## 3. 协议层(omnish-protocol)

在 `Message` 枚举**末尾**追加变体(保持 bincode 变体索引稳定):

```rust
/// 通用的 daemon -> client 主动推送。`kind` 标识语义,
/// `payload` 是与 kind 对应的 JSON 载荷。client 按 kind 分发。
/// PROTOCOL_VERSION 26。
ServerPush { kind: String, payload: serde_json::Value },
```

首个 kind 约定:
- `kind = "thread_title_update"`
- `payload = { "thread_id": String, "title": String }`

版本与守卫:
- `PROTOCOL_VERSION` 25 -> 26。
- `MIN_COMPATIBLE_VERSION` 保持 25(append-only,向后兼容:旧 client 收不到/跳过该帧,仅退化为不刷新标题)。
- 更新 `message_variant_guard` 测试:`EXPECTED_VARIANT_COUNT` 37 -> 38,并在穷举 vec 中加入 `ServerPush`。

## 4. 守护进程侧(omnish-daemon)

### 4.1 让 scheduled task 能访问 push_registry

`PushRegistry` 当前由 `DaemonServer::new` 内部创建(`server.rs`),而 `TaskContext.daemon: Arc<DaemonContext>` 不持有它。

改动:
- 在 `DaemonContext`(`task_mgr.rs`)新增字段 `pub push_registry: PushRegistry`。
- 在 `main.rs` **第 238 行构建 `daemon_ctx` 之前**创建 `let push_registry: PushRegistry = Arc::new(Mutex::new(HashMap::new()))`,同时:
  - 注入 `DaemonContext { ..., push_registry: push_registry.clone() }`;
  - 传入 `DaemonServer::new(...)`,让 server 复用同一实例(替换其内部自建)。
- `PushRegistry` 是 `Arc<Mutex<HashMap<...>>>`,clone 廉价。

### 4.2 广播 ServerPush

新增共享辅助函数(放在 `deploy.rs` 的 `broadcast_notice` 旁或新建 `push.rs`):

```rust
pub async fn broadcast_server_push(registry: &PushRegistry, kind: &str, payload: serde_json::Value) {
    let senders: Vec<_> = { registry.lock().await.values().cloned().collect() };
    for tx in senders {
        let _ = tx.send(Message::ServerPush { kind: kind.to_string(), payload: payload.clone() }).await;
    }
}
```

在 `thread_summary.rs` 生成新 `title_word` 成功后(`updated_meta.title_word = Some(word)` 的分支内,`meta_dirty = true` 之后)广播:

```rust
broadcast_server_push(
    &ctx.daemon.push_registry,
    "thread_title_update",
    serde_json::json!({ "thread_id": thread_id, "title": word }),
).await;
```

注意:`generate_thread_summaries` 当前签名只接收 `conv_mgr`/`llm`/`language`。需把 `push_registry`(或 `&TaskContext`)透传进去。优先只透传 `push_registry`,保持函数职责清晰、便于测试。

## 5. 客户端侧(omnish-client)

### 5.1 共享待应用标题槽

```rust
// thread_id -> 最新标题。用 HashMap 避免不同 thread 的更新互相覆盖丢失。
type PendingTitles = Arc<Mutex<HashMap<String, String>>>;  // std::sync::Mutex,锁持有极短
```

在客户端启动建立 `daemon_conn` 后创建,克隆分发给 dispatcher task 与 ChatSession。

### 5.2 常驻 dispatcher task(唯一 push 消费者)

在 polling task 附近(daemon_conn 为 `Some` 时)spawn:

```rust
loop {
    while let Some(msg) = rpc.try_recv_push().await {
        match msg {
            Message::ServerPush { kind, payload } => match kind.as_str() {
                "thread_title_update" => {
                    if let (Some(tid), Some(title)) = (payload["thread_id"].as_str(), payload["title"].as_str()) {
                        pending_titles.lock().unwrap().insert(tid.to_string(), title.to_string());
                    }
                }
                other => event_log::push(format!("unknown ServerPush kind: {other}")),
            },
            // 维持现状:转发给 shell 主循环处理(见 5.3)
            other => { let _ = fallback_tx.send(other).await; }
        }
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}
```

要点:
- 用轮询式 `try_recv_push`(每次重新加锁,锁持有短),不能用 `recv().await` 长期持锁,否则会卡住重连时 `push_rx` 的替换(`rpc_client.rs:451-455`)。
- dispatcher 是 `push_rx` 的**唯一**消费者,避免与主循环争抢导致 ServerPush 被 `_ => {}` 静默丢弃。

### 5.3 ConfigClient / NoticePush 行为不变

- 新增 `fallback`(mpsc)通道:dispatcher 把非 ServerPush 消息转发进去。
- shell 主循环原 `main.rs:1842` 处由 `rpc.try_recv_push()` 改为 drain `fallback_rx`,`ConfigClient`/`NoticePush` 的处理逻辑完全不动。
- 聊天模式下主循环阻塞,这两类消息在通道里缓冲、退出聊天后处理(与现状一致,无回归)。

### 5.4 渲染线程在安全点应用标题

ChatSession 持有 `pending_titles`。新增方法:

```rust
fn apply_pending_title(&self) {
    if let Some(tid) = self.current_thread_id.as_deref() {
        if let Some(title) = self.pending_titles.lock().unwrap().remove(tid) {
            super::set_chat_tmux_title(Some(&title));
        }
    }
}
```

调用点(均在聊天自身的执行流上,不与其它渲染并发):
- 聊天循环每轮迭代绘制 `> ` 提示符之前;
- `read_input_with` 的 poll 超时分支(`chat_session.rs:3935` 附近,每 `timeout_ms` 醒一次)-> 覆盖"用户空闲停在提示符"的主要场景,实现近实时刷新。

过滤天然由渲染线程完成:它直接持有自己的 `current_thread_id`,无需把该字段共享给 dispatcher。非当前 thread 的更新留在 map 中(只占极小内存),或在退出聊天时清理。

## 6. 并发与正确性

- 客户端为多线程 runtime(`worker_threads = 4`),后台 task 与渲染可真并行。
- 单次 `nix::unistd::write` 字节原子,但"多段逻辑转义序列"(`redraw_tool_section`/spinner/prompt 的 `\x1b7..\x1b8`)会被另一线程的写入打断;若 tmux 标题序列 `\x1bk..\x1b\\` 的 ESC 落入在途 CSI 序列中会造成错乱。
- 本设计通过**只让渲染线程写 tmux 标题**(dispatcher 仅更新共享槽)规避该竞争。
- 彻底方案(把 stdout 封装为统一写出对象、以完整逻辑序列为加锁单元)记录在 issue #644;落地后 dispatcher 可直接安全写出,本折中可简化。

## 7. 测试

- **协议**:`message_variant_guard` 更新;`ServerPush` 的 bincode 往返序列化测试。
- **daemon**:`thread_summary` 单测 - 注入一个 mock sender 到 `push_registry`,断言生成 `title_word` 后收到 `ServerPush { kind:"thread_title_update", payload:{thread_id,title} }`。
- **client**:
  - dispatcher 路由单测:`ServerPush` 进 `pending_titles`,其它进 `fallback`。
  - `apply_pending_title` 过滤单测:仅当 `thread_id == current_thread_id` 时写标题并移除条目。
- **手动验证**:tmux 下进入聊天,触发 `thread_summary`(或临时缩短其周期),确认窗口标题在空闲提示符状态下被刷新,且聊天渲染无错乱。

## 8. 实现顺序建议

1. 协议:新增 `ServerPush` + 版本/守卫更新。
2. daemon:`push_registry` 接入 `DaemonContext` + `broadcast_server_push` + `thread_summary` 广播。
3. client:`pending_titles` + dispatcher task + fallback 通道 + 主循环改用 fallback。
4. client:ChatSession 的 `apply_pending_title` 与调用点。
5. 测试与手动验证。
