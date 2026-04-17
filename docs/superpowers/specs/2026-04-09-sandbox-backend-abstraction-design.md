# 沙箱后端抽象层设计

日期：2026-04-09

## 目标

将现有平台特定的沙箱实现（Linux Landlock、macOS sandbox-exec）抽象为统一接口，并新增 bubblewrap（bwrap）作为 Linux 上的可选后端。调用方只描述「限制什么」，不关心「怎么实现」。

## 非目标

- 网络隔离（`--unshare-net`、socat 代理、域名白名单）
- PID 命名空间隔离
- seccomp 系统调用过滤
- 违规检测与报告

以上功能留待后续迭代。

## 数据结构

### SandboxBackendType

```rust
pub enum SandboxBackendType {
    Bwrap,
    Landlock,
    #[cfg(target_os = "macos")]
    MacosSeatbelt,
}
```

### SandboxPolicy

```rust
pub struct SandboxPolicy {
    /// 可写路径列表，文件系统其余部分只读
    pub writable_paths: Vec<PathBuf>,
    /// 禁止读取的路径列表
    pub deny_read: Vec<PathBuf>,
    /// 是否允许网络访问（预留字段，当前始终 true）
    pub allow_network: bool,
}
```

## 核心 API

所有函数位于 `omnish-plugin` crate。

### 可用性检测

```rust
/// 检测指定后端是否可用
pub fn is_available(backend: SandboxBackendType) -> bool;

/// 自动选择后端：preferred → fallback → None
/// 不可用时记录 warn 日志
/// 回退链：
///   Linux:  bwrap → landlock → None
///   macOS:  MacosSeatbelt → None
pub fn detect_backend(preferred: SandboxBackendType) -> Option<SandboxBackendType>;
```

### 命令构建

```rust
/// 构建带沙箱限制的 Command
/// 调用方唯一入口，不关心后端实现细节
pub fn sandbox_command(
    backend: SandboxBackendType,
    policy: &SandboxPolicy,
    executable: &Path,
    args: &[&str],
) -> Result<Command, String>;
```

### Policy 便利函数

```rust
/// 构建插件执行场景的 policy
/// 可写：data_dir + common_writable_paths + cwd + git_repo_root
pub fn plugin_policy(data_dir: &Path, cwd: Option<&Path>) -> SandboxPolicy;

/// 构建 shell lock 场景的 policy
/// 可写：common_writable_paths + cwd + git_repo_root（无 data_dir）
pub fn lock_policy(cwd: Option<&Path>) -> SandboxPolicy;
```

### common_writable_paths 组成

```
/tmp
/dev/null, /dev/ptmx, /dev/pts, /dev/tty, /dev/shm
~/.ssh, ~/.cargo, ~/.config, ~/.local, ~/.claude, ~/.omnish
~/.cache, ~/.npm, ~/.rustup, ~/.gnupg, ~/.docker, ~/.kube
~/.nvm, ~/.pyenv
```

加上：
- `cwd`（当前工作目录）
- `git rev-parse --show-toplevel` 探测的仓库根目录
- `data_dir`（仅 plugin_policy，路径为 `~/.omnish/data/{plugin_name}/`）

## 各后端实现

### Bwrap

构建 `Command::new("bwrap")`，参数：

```
--new-session
--die-with-parent
--ro-bind / /                          # 只读根文件系统
--dev /dev                             # 挂载 devtmpfs
--bind <writable_path> <writable_path> # 每个可写路径
--tmpfs <deny_read_dir>                # 目录级读取拒绝
--ro-bind /dev/null <deny_read_file>   # 文件级读取拒绝
-- <executable> <args...>
```

可用性检测：`which bwrap` 成功。

### Landlock

构建 `Command::new(executable).args(args)`，通过 `pre_exec` 闭包应用 Landlock 规则集：

- 读取权限：整个文件系统 `/`
- 写入权限：仅 `policy.writable_paths`
- deny_read：Landlock ABI v3（内核 6.2+）支持 `LANDLOCK_ACCESS_FS_TRUNCATE` 等，但读取拒绝需要 v1 即可通过不授予读权限实现；低版本内核对 deny_read 记录警告并忽略

现有 `apply_landlock()` 逻辑保持不变，仅重构为从 `SandboxPolicy` 读取参数。

可用性检测：`is_landlock_supported()`（内核 >= 5.13）。

### macOS Seatbelt

构建 `Command::new("sandbox-exec").args(["-p", &profile, executable, args...])`：

- 从 `SandboxPolicy` 生成 `.sb` profile 字符串
- 现有 `build_sandbox_profile()` 重构为接收 `SandboxPolicy` 参数

可用性检测：macOS 平台始终可用。

## 配置变更

### omnish-common SandboxConfig

```rust
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct SandboxConfig {
    /// 沙箱后端："bwrap" | "landlock" | "macos"
    /// 默认值：Linux 为 "bwrap"，macOS 为 "macos"
    #[serde(default = "default_backend")]
    pub backend: String,

    /// 每个工具的豁免规则（现有字段，不变）
    #[serde(default)]
    pub plugins: HashMap<String, SandboxPluginConfig>,
}
```

### omnish-client ClientConfig

```rust
/// 可选覆盖 daemon 的 sandbox backend 设置
#[serde(default)]
pub sandbox_backend: Option<String>,
```

客户端优先使用自己的 `sandbox_backend`，未设置时通过 ConfigQuery 从 daemon 获取。

## 回退逻辑

```
detect_backend(preferred):
  if is_available(preferred) → return preferred
  warn!("preferred backend {:?} not available, trying fallback", preferred)

  fallback = match preferred:
    Bwrap → Some(Landlock)
    Landlock → Some(Bwrap)
    MacosSeatbelt → None

  if fallback.is_some() && is_available(fallback):
    warn!("using fallback backend {:?}", fallback)
    return fallback

  warn!("no sandbox backend available, running without sandbox")
  return None
```

## 调用方变更

### client_plugin.rs (execute_tool)

变更前：平台条件编译 + 各自实现 Landlock / sandbox-exec。

变更后：

```rust
let policy = plugin_policy(&data_dir, cwd.as_deref());
let mut cmd = if sandboxed {
    match detect_backend(configured_backend) {
        Some(backend) => sandbox_command(backend, &policy, &executable, &tool_args)?,
        None => {
            warn!("no sandbox available");
            let mut c = Command::new(&executable);
            c.args(&tool_args);
            c
        }
    }
} else {
    let mut c = Command::new(&executable);
    c.args(&tool_args);
    c
};
cmd.stdin(Stdio::piped())
   .stdout(Stdio::piped())
   .stderr(Stdio::piped());
```

### main.rs (handle_lock)

变更前：Landlock pre_exec 传给 `respawn()`。

变更后：

```rust
let policy = lock_policy(cwd.as_deref());
match detect_backend(configured_backend) {
    Some(SandboxBackendType::Bwrap) => {
        // respawn 用 bwrap -- shell 包装
    }
    Some(SandboxBackendType::Landlock) => {
        // 保持现有 pre_exec 逻辑
    }
    Some(SandboxBackendType::MacosSeatbelt) => {
        // sandbox-exec -- shell
    }
    None => {
        warn!("no sandbox available for lock");
    }
}
```

## 模块结构

`omnish-plugin` 拆分出 `sandbox` 子模块：

```
omnish-plugin/src/
├── lib.rs              # pub mod sandbox; pub use sandbox::*;
├── sandbox/
│   ├── mod.rs          # SandboxBackendType, SandboxPolicy, sandbox_command,
│   │                   # is_available, detect_backend, plugin_policy, lock_policy
│   ├── bwrap.rs        # bwrap 后端实现
│   ├── landlock.rs     # landlock 后端实现（从 lib.rs 迁入）
│   └── seatbelt.rs     # macOS sandbox-exec 后端实现（从 lib.rs 迁入）
```

`lib.rs` 只做 re-export：

```rust
pub mod sandbox;
pub use sandbox::{
    SandboxBackendType, SandboxPolicy,
    sandbox_command, detect_backend, is_available,
    plugin_policy, lock_policy,
};
```

## 文件变更范围

| 文件 | 变更内容 |
|------|----------|
| `omnish-common/src/config.rs` | SandboxConfig 加 backend 字段，ClientConfig 加 sandbox_backend |
| `omnish-plugin/src/lib.rs` | 迁出 Landlock/macOS 代码，改为 re-export sandbox 模块 |
| `omnish-plugin/src/sandbox/mod.rs` | 新增统一接口：SandboxBackendType、SandboxPolicy、sandbox_command、detect_backend 等 |
| `omnish-plugin/src/sandbox/bwrap.rs` | 新增 bwrap 后端实现 |
| `omnish-plugin/src/sandbox/landlock.rs` | 从 lib.rs 迁入现有 Landlock 代码 |
| `omnish-plugin/src/sandbox/seatbelt.rs` | 从 lib.rs 迁入现有 macOS sandbox-exec 代码 |
| `omnish-plugin/Cargo.toml` | 新增 `which` 依赖（bwrap 可用性检测） |
| `omnish-client/src/client_plugin.rs` | 简化为调用 sandbox_command |
| `omnish-client/src/main.rs` | handle_lock 适配新接口 |

不变的文件：
- `omnish-daemon/src/sandbox_rules.rs` - permit_rules 逻辑不变
- `omnish-protocol/src/message.rs` - ChatToolCall.sandboxed 不变
- `omnish-client/src/chat_session.rs` - /test lock 解析不变
