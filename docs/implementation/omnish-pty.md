# omnish-pty 模块

**功能:** PTY（伪终端）处理，原始模式设置

## 模块概述

omnish-pty 提供PTY代理功能，创建伪终端并管理原始模式，确保透明转发所有I/O。该模块是omnish项目的核心组件之一，负责与shell进程的底层通信。

## 重要数据结构

### `PtyProxy`
PTY代理，负责：
- 创建伪终端（使用`openpty`）
- 转发I/O数据
- 管理子进程生命周期
- 设置终端窗口大小

**字段:**
- `master_fd: OwnedFd` - 主端文件描述符
- `child_pid: Pid` - 子进程PID

### `RawModeGuard`
原始模式守卫，RAII风格：
- 进入时设置原始模式（禁用回显、规范模式等）
- 退出时自动恢复原始终端设置
- 确保异常安全

**字段:**
- `fd: RawFd` - 文件描述符
- `original: Termios` - 原始终端设置

## 关键函数说明

### `PtyProxy::spawn()`
创建PTY并启动子进程。

**参数:** `cmd: &str`, `args: &[&str]`
**返回:** `Result<PtyProxy>`
**用途:** 启动shell进程，使用默认环境变量
**实现细节:** 调用`spawn_with_env`并传入空环境变量映射

### `PtyProxy::spawn_with_env()`
创建PTY并启动子进程，可指定环境变量。

**参数:** `cmd: &str`, `args: &[&str]`, `env: HashMap<String, String>`
**返回:** `Result<PtyProxy>`
**用途:** 启动shell进程并设置自定义环境变量
**实现细节:**
1. 使用`openpty`创建伪终端
2. `fork`创建子进程
3. 在子进程中设置控制终端、重定向标准I/O
4. 设置环境变量并执行命令

### `PtyProxy::read()`
从PTY读取数据。

**参数:** `buf: &mut [u8]`
**返回:** `Result<usize>`
**用途:** 读取子进程输出
**实现细节:** 使用`nix::unistd::read`从主端读取

### `PtyProxy::write_all()`
向PTY写入数据。

**参数:** `data: &[u8]`
**返回:** `Result<()>`
**用途:** 发送输入到子进程
**实现细节:** 循环写入确保所有数据都被发送

### `PtyProxy::set_window_size()`
设置终端窗口大小。

**参数:** `rows: u16`, `cols: u16`
**返回:** `Result<()>`
**用途:** 通知子进程终端尺寸变化
**实现细节:** 使用`ioctl`的`TIOCSWINSZ`命令

### `PtyProxy::wait()`
等待子进程退出。

**参数:** 无
**返回:** `Result<i32>`
**用途:** 获取子进程退出状态
**实现细节:** 使用`waitpid`等待进程终止

### `RawModeGuard::enter()`
创建原始模式守卫。

**参数:** `fd: RawFd`
**返回:** `Result<RawModeGuard>`
**用途:** 安全设置原始模式
**实现细节:**
1. 保存当前终端设置
2. 使用`cfmakeraw`配置原始模式
3. 应用新设置
4. 返回守卫对象，析构时自动恢复

## 使用示例

```rust
use omnish_pty::PtyProxy;
use omnish_pty::raw_mode::RawModeGuard;

// 创建PTY代理并启动bash
let mut proxy = PtyProxy::spawn("bash", &[])?;

// 设置原始模式
let _guard = RawModeGuard::enter(proxy.master_raw_fd())?;

// 发送命令
proxy.write_all(b"ls -la\n")?;

// 读取输出
let mut buf = [0u8; 4096];
let n = proxy.read(&mut buf)?;
let output = &buf[..n];

// 设置窗口大小
proxy.set_window_size(24, 80)?;

// 等待进程退出
let exit_code = proxy.wait()?;
```

## 依赖关系
- **nix**: Unix系统调用封装，提供`openpty`、`fork`、`execvp`等函数
- **libc**: C库绑定，用于`ioctl`调用
- **anyhow**: 错误处理，提供上下文信息
- **std::collections::HashMap**: 环境变量存储
- **std::ffi::CString**: C字符串转换

## 设计模式

### RAII模式
`RawModeGuard`使用RAII（Resource Acquisition Is Initialization）模式确保资源安全：
- 构造时获取资源（设置原始模式）
- 析构时释放资源（恢复原始设置）
- 异常安全：即使发生panic，析构函数也会被调用

### 代理模式
`PtyProxy`作为PTY的代理：
- 封装复杂的PTY操作
- 提供简洁的API
- 管理子进程生命周期

## 注意事项

1. **文件描述符管理**: PTY创建后需要正确关闭文件描述符，避免泄漏
2. **信号处理**: 子进程需要正确处理信号，特别是SIGINT和SIGTERM
3. **终端设置**: 原始模式会禁用许多终端功能，需确保使用场景正确
4. **环境变量**: 子进程继承父进程环境，可通过`spawn_with_env`自定义
5. **错误处理**: 所有操作都可能失败，需要适当处理错误
6. **TIOCSCTTY ioctl**: 在不同平台上TIOCSCTTY常数的定义可能不同，需使用`as _`进行类型转换以保证兼容性

## 平台支持

模块支持多个Unix系统：

### Linux
全功能支持，包括PTY创建、raw模式设置、窗口大小调整等。

### macOS
支持PTY创建和基本操作，某些高级功能可能有限制。TIOCSCTTY ioctl已正确适配macOS平台。

### 其他Unix平台
基础功能支持，某些特定功能（如某些ioctl操作）可能需要有条件编译或平台特定处理。