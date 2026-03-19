# Daemon/Client 通信安全机制设计

Issue: #12

## 概述

为 omnish daemon/client 通信添加分层安全机制：共享密钥认证 + Unix socket 权限控制 + SO_PEERCRED UID 验证 + TCP 模式 TLS 加密。

## 当前问题

- 无认证：任何进程都可以连接 daemon
- 无加密：所有数据（shell 历史、LLM 对话）明文传输
- Socket 权限未设置：默认继承 umask，可能允许其他用户连接
- 无访问控制：任何连接都可以访问所有会话数据

## 设计

### 1. Token 认证

**Token 生成与存储**:
- Daemon 首次启动时生成 32 字节随机 token（`rand::thread_rng().gen::<[u8; 32]>()`）
- Hex 编码后写入 `~/.omnish/auth_token`，权限 `0o600`
- 后续启动时读取已有 token（文件存在且权限正确时复用）
- 用户可手动删除文件来强制轮换 token

**认证流程**:
1. Client 读取 `~/.omnish/auth_token`
2. 连接建立后，第一条消息发送 `Auth { token: String }`
3. Daemon 验证 token，匹配则标记连接为已认证，回复 `Ack`
4. Token 不匹配则回复 `AuthFailed` 并断开
5. 连接后 5 秒内未认证则断开
6. 未认证连接收到任何非 Auth 消息直接断开

**Reconnect 处理**:
- `on_reconnect` 回调中先发 Auth 再发 SessionStart
- 已有的 `connect_with_reconnect` 机制自然支持

### 2. Unix Socket 安全

**Socket 文件权限**:
- `bind_unix()` 创建 socket 后，设置权限 `0o600`
- 确保只有 owner 可读写

**Peer credential 验证**:
- 连接接受后，用 `SO_PEERCRED`（Linux）/ `LOCAL_PEERCRED`（macOS）获取对方 UID
- 验证 UID == daemon 进程 UID
- UID 不匹配直接拒绝连接（token 之外的第二道防线）

### 3. TCP TLS 加密

**自签名证书**:
- Daemon 首次启动时用 `rcgen` crate 生成自签名证书和私钥
- 保存到 `~/.omnish/tls/cert.pem` 和 `~/.omnish/tls/key.pem`（权限 0600）
- 后续启动复用已有证书

**TLS 连接**:
- TCP 模式下，server 端用 `tokio-rustls` 的 `TlsAcceptor` 包装连接
- Client 端用 `TlsConnector`，信任 `~/.omnish/tls/cert.pem`（同用户可访问）
- Unix socket 不启用 TLS（FS 权限 + SO_PEERCRED 已够）

### 4. Protocol 变更

**新增 Message 变体（追加到末尾）**:
```rust
Auth { token: String },
AuthFailed,
```

**Server 端连接状态机**:
```
Connected → (收到 Auth + token 正确) → Authenticated → (正常处理消息)
Connected → (5秒超时 / 非Auth消息 / Auth失败) → Disconnected
```

### 5. 改动范围

| 模块 | 改动 |
|------|------|
| `omnish-protocol` | 添加 `Auth`/`AuthFailed` 消息变体 |
| `omnish-transport/rpc_server.rs` | socket 权限设置、SO_PEERCRED 验证、认证状态机、TLS acceptor |
| `omnish-transport/rpc_client.rs` | connect 方法增加 auth token 参数、TLS connector |
| `omnish-common/config.rs` | auth_token_path、tls cert/key 路径配置 |
| `omnish-daemon/main.rs` | 启动时生成/读取 token 和证书 |
| `omnish-client/main.rs` | 连接时发送 Auth、读取 token |

### 6. 新增依赖

- `rcgen`: 自签名证书生成
- `tokio-rustls`: TLS 集成（项目已有 rustls 依赖）
- `rand`: 随机 token 生成（可能已有）

### 7. 安全层级总结

| 层级 | 机制 | 防御 |
|------|------|------|
| 文件系统 | auth_token 权限 0600 | 其他用户无法读取 token |
| Socket | 权限 0600 + SO_PEERCRED | 其他用户无法连接 |
| 协议 | Auth token 认证 | 未授权进程无法使用 API |
| 传输 | TLS（TCP 模式） | 防止网络窃听和篡改 |
