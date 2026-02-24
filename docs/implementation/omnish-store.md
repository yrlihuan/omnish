# omnish-store 模块

**功能:** 数据存储，命令记录和流存储

## 模块概述

omnish-store 提供数据持久化功能，包括命令记录、会话管理和原始流存储。该模块负责将终端会话的命令历史、元数据和原始I/O流数据保存到文件系统中，以便后续分析和检索。

## 重要数据结构

### `CommandRecord`
命令记录结构，包含命令执行的完整信息：

```rust
pub struct CommandRecord {
    pub command_id: String,        // 命令ID
    pub session_id: String,        // 会话ID
    pub command_line: Option<String>, // 命令行内容
    pub cwd: Option<String>,       // 当前工作目录
    pub started_at: u64,           // 开始时间戳（毫秒）
    pub ended_at: Option<u64>,     // 结束时间戳（毫秒）
    pub output_summary: String,    // 输出摘要
    pub stream_offset: u64,        // 流数据偏移量
    pub stream_length: u64,        // 流数据长度
    pub exit_code: Option<i32>,    // 退出码
}
```

### `SessionMeta`
会话元数据结构，包含会话的基本信息：

```rust
pub struct SessionMeta {
    pub session_id: String,                // 会话ID
    pub parent_session_id: Option<String>, // 父会话ID
    pub started_at: String,                // 开始时间（字符串格式）
    pub ended_at: Option<String>,          // 结束时间（字符串格式）
    pub attrs: HashMap<String, String>,    // 会话属性
}
```

### `StreamWriter`
流写入器，负责将原始I/O数据写入二进制文件：

```rust
pub struct StreamWriter {
    writer: BufWriter<File>,  // 缓冲写入器
    pos: u64,                 // 当前写入位置
}
```

### `StreamEntry`
流条目结构，表示单个I/O事件：

```rust
pub struct StreamEntry {
    pub timestamp_ms: u64,    // 时间戳（毫秒）
    pub direction: u8,        // 方向（0=输入，1=输出）
    pub data: Vec<u8>,        // 原始数据
}
```

## 关键函数说明

### `CommandRecord::save_all()`
批量保存命令记录到文件。

**参数:** `records: &[CommandRecord]`, `dir: &Path`
**返回:** `Result<()>`
**用途:** 将命令记录数组序列化为JSON并保存到`commands.json`文件

### `CommandRecord::load_all()`
从文件加载所有命令记录。

**参数:** `dir: &Path`
**返回:** `Result<Vec<CommandRecord>>`
**用途:** 从`commands.json`文件读取并反序列化命令记录

### `SessionMeta::save()`
保存会话元数据到文件。

**参数:** `&self`, `dir: &Path`
**返回:** `Result<()>`
**用途:** 将会话元数据序列化为JSON并保存到`meta.json`文件

### `SessionMeta::load()`
从文件加载会话元数据。

**参数:** `dir: &Path`
**返回:** `Result<SessionMeta>`
**用途:** 从`meta.json`文件读取并反序列化会话元数据

### `StreamWriter::create()`
创建新的流写入器。

**参数:** `path: &Path`
**返回:** `Result<StreamWriter>`
**用途:** 创建新的流文件并初始化写入器

### `StreamWriter::open_append()`
以追加模式打开现有流文件。

**参数:** `path: &Path`
**返回:** `Result<StreamWriter>`
**用途:** 打开现有流文件并定位到文件末尾继续写入

### `StreamWriter::write_entry()`
写入流条目到文件。

**参数:** `timestamp_ms: u64`, `direction: u8`, `data: &[u8]`
**返回:** `Result<()>`
**用途:** 将I/O事件按二进制格式写入流文件

**二进制格式:** `timestamp_ms(8字节) + direction(1字节) + data_len(4字节) + data(N字节)`

### `StreamWriter::position()`
获取当前写入位置。

**返回:** `u64`
**用途:** 返回当前流文件中的写入偏移量

### `read_range()`
从指定偏移量读取流条目。

**参数:** `path: &Path`, `offset: u64`, `length: u64`
**返回:** `Result<Vec<StreamEntry>>`
**用途:** 从流文件的指定位置读取指定长度的数据并解析为流条目

### `read_entries()`
读取流文件中的所有条目。

**参数:** `path: &Path`
**返回:** `Result<Vec<StreamEntry>>`
**用途:** 读取整个流文件并解析所有流条目

## 使用示例

### 保存命令记录
```rust
use omnish_store::CommandRecord;
use std::path::Path;

let records = vec![
    CommandRecord {
        command_id: "cmd1".to_string(),
        session_id: "sess1".to_string(),
        command_line: Some("ls -la".to_string()),
        cwd: Some("/home/user".to_string()),
        started_at: 1000,
        ended_at: Some(2000),
        output_summary: "列出目录内容".to_string(),
        stream_offset: 0,
        stream_length: 100,
        exit_code: Some(0),
    }
];

CommandRecord::save_all(&records, Path::new("/path/to/store"))?;
```

### 管理会话元数据
```rust
use omnish_store::SessionMeta;
use std::collections::HashMap;
use std::path::Path;

let mut attrs = HashMap::new();
attrs.insert("shell".to_string(), "bash".to_string());
attrs.insert("term".to_string(), "xterm-256color".to_string());

let session = SessionMeta {
    session_id: "sess1".to_string(),
    parent_session_id: None,
    started_at: "2024-01-01T10:00:00Z".to_string(),
    ended_at: None,
    attrs,
};

session.save(Path::new("/path/to/session"))?;
```

### 写入流数据
```rust
use omnish_store::StreamWriter;
use std::path::Path;

let mut writer = StreamWriter::create(Path::new("/path/to/stream.bin"))?;
writer.write_entry(1000, 0, b"ls -la")?;  // 输入
writer.write_entry(2000, 1, b"total 100")?; // 输出

// 后续追加写入
let mut writer = StreamWriter::open_append(Path::new("/path/to/stream.bin"))?;
writer.write_entry(3000, 0, b"cd /tmp")?;
```

### 读取流数据
```rust
use omnish_store::{read_range, read_entries};
use std::path::Path;

// 读取所有条目
let all_entries = read_entries(Path::new("/path/to/stream.bin"))?;

// 读取指定范围的条目
let range_entries = read_range(Path::new("/path/to/stream.bin"), 0, 100)?;
```

## 依赖关系
- **serde**: 序列化和反序列化支持
- **serde_json**: JSON序列化实现
- **anyhow**: 错误处理
- **std::fs, std::io**: 文件系统操作和I/O处理

## 文件结构
omnish-store模块使用以下文件结构存储数据：
```
store_directory/
├── commands.json      # 命令记录（JSON格式）
├── meta.json         # 会话元数据（JSON格式）
└── stream.bin        # 原始流数据（二进制格式）
```

## 设计特点
1. **高效存储**: 流数据使用紧凑的二进制格式，减少存储空间
2. **增量写入**: 支持追加模式，避免重复写入已有数据
3. **精确检索**: 通过偏移量和长度精确读取特定范围的流数据
4. **结构化元数据**: 命令和会话信息使用JSON格式，便于人类阅读和工具处理
5. **错误处理**: 使用anyhow提供统一的错误处理机制