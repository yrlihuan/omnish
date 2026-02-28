# omnish-llm 模块

**功能:** LLM后端抽象和实现

## 模块概述

omnish-llm 提供LLM后端抽象，支持多种LLM提供商，包括Anthropic、OpenAI兼容API。模块通过统一的接口封装不同LLM提供商的API调用，提供一致的LLM交互体验。

## 重要数据结构

### `LlmBackend` trait
LLM后端接口，定义所有LLM后端必须实现的方法：
- `complete()`: 发送补全请求并获取响应
- `name()`: 返回后端名称标识符
- `max_content_chars()`: 返回该后端模型的上下文最大字符数（可选，默认None）
- `max_content_chars_for_use_case()`: 根据用途返回上下文字符数限制（可选，默认返回max_content_chars()）

### `LlmRequest`
LLM请求结构体，包含发送给LLM的完整请求信息：
- `context`: 终端会话上下文
- `query`: 用户查询（可选）
- `trigger`: 触发类型（手动、自动错误检测、自动模式检测）
- `session_ids`: 相关会话ID列表
- `use_case`: 请求用途（用于选择合适的模型）
- `max_content_chars`: 模型上下文最大字符数限制（可选，用于限制上下文大小）

### `LlmResponse`
LLM响应结构体，包含LLM返回的结果：
- `content`: LLM生成的文本内容
- `model`: 使用的模型名称

### `TriggerType` 枚举
触发类型枚举，表示LLM请求的触发方式：
- `Manual`: 手动触发（用户显式请求）
- `AutoError`: 自动错误检测触发
- `AutoPattern`: 自动模式检测触发

### `AnthropicBackend`
Anthropic API后端实现：
- 支持Claude模型系列
- 实现`LlmBackend` trait
- 使用Anthropic Messages API (v1/messages)

### `OpenAiCompatBackend`
OpenAI兼容API后端实现：
- 支持OpenAI、Azure OpenAI、本地兼容API
- 实现`LlmBackend` trait
- 使用OpenAI兼容的Chat Completions API

### `LlmBackendConfig`
LLM后端配置结构体（来自omnish-common）：
- `backend_type`: 后端类型（"anthropic" 或 "openai-compat"）
- `model`: 模型名称
- `api_key_cmd`: 获取API密钥的命令
- `base_url`: API基础URL（仅openai-compat需要）
- `max_content_chars`: 该模型的上下文最大字符数（可选，用于限制上下文大小）

## 关键函数说明

### `LlmBackend::complete()`
发送LLM补全请求并获取响应。

**参数:** `req: &LlmRequest` - LLM请求结构体
**返回:** `Result<LlmResponse>` - LLM响应或错误
**用途:** 主要的LLM交互接口，处理API调用、错误处理和响应解析

### `create_backend()`
根据配置创建LLM后端实例。

**参数:**
- `_name: &str` - 后端名称（当前未使用）
- `config: &LlmBackendConfig` - 后端配置

**返回:** `Result<Arc<dyn LlmBackend>>` - 装箱的LLM后端实例
**用途:** 工厂函数，根据配置类型创建对应的后端实现

### `create_default_backend()`
从完整LLM配置创建默认后端。

**参数:** `llm_config: &LlmConfig` - 完整的LLM配置
**返回:** `Result<Arc<dyn LlmBackend>>` - 默认后端实例
**用途:** 简化后端创建，自动使用配置中的默认后端

### `resolve_api_key()`
通过命令解析API密钥。

**参数:** `api_key_cmd: &Option<String>` - 获取API密钥的命令
**返回:** `Result<String>` - 解析出的API密钥
**用途:** 安全地获取API密钥，支持通过命令动态获取

### `build_user_content()`
构建发送给LLM的用户内容。

**参数:**
- `context: &str` - 终端会话上下文
- `query: Option<&str>` - 用户查询（可选）

**返回:** `String` - 格式化后的用户内容
**用途:** 根据是否有查询构建不同的提示模板

### `build_completion_content()`
构建shell命令补全的提示内容。

**参数:**
- `context: &str` - 终端会话上下文
- `input: &str` - 当前输入的命令
- `cursor_pos: usize` - 光标位置

**返回:** `String` - 格式化后的补全提示
**用途:** 为shell命令补全生成专门的提示，要求LLM返回JSON格式的补全建议

### `prompt_template()`
获取提示模板。

**参数:** `has_query: bool` - 是否有用户查询
**返回:** `&'static str` - 提示模板字符串
**用途:** 返回包含占位符的提示模板

## 使用示例

### 基本使用
```rust
use omnish_llm::{create_default_backend, LlmBackend};
use omnish_common::config::LlmConfig;

// 从配置创建默认后端
let backend = create_default_backend(&llm_config).await?;

// 构建LLM请求
let request = LlmRequest {
    context: "终端会话上下文...".to_string(),
    query: Some("用户查询".to_string()),
    trigger: TriggerType::Manual,
    session_ids: vec![],
};

// 发送请求并获取响应
let response = backend.complete(&request).await?;
println!("LLM响应: {}", response.content);
```

### 创建特定后端
```rust
use omnish_llm::{create_backend, LlmBackend};
use omnish_common::config::LlmBackendConfig;

// 配置Anthropic后端
let config = LlmBackendConfig {
    backend_type: "anthropic".to_string(),
    model: "claude-3-5-sonnet-20241022".to_string(),
    api_key_cmd: Some("echo $ANTHROPIC_API_KEY".to_string()),
    base_url: None,
};

// 创建后端
let backend = create_backend("anthropic", &config)?;

// 使用后端...
```

### 构建提示内容
```rust
use omnish_llm::template;

// 构建带查询的提示
let user_content = template::build_user_content(
    "$ ls\nfile1.txt\nfile2.txt",
    Some("有哪些文件？")
);

// 构建命令补全提示
let completion_content = template::build_completion_content(
    "$ cd /home/user/project\n$ git status",
    "git comm",
    8
);
```

## 依赖关系
- `omnish-common`: 配置类型定义
- `reqwest`: HTTP客户端，用于API调用
- `serde_json`: JSON序列化和反序列化
- `anyhow`: 错误处理
- `async-trait`: 异步trait支持
- `std::process::Command`: 执行命令获取API密钥
- `std::sync::Arc`: 线程安全的引用计数

## 配置示例
```toml
# omnish.toml 中的LLM配置
[llm]
default = "anthropic"

[llm.backends.anthropic]
backend_type = "anthropic"
model = "claude-3-5-sonnet-20241022"
api_key_cmd = "echo $ANTHROPIC_API_KEY"
max_content_chars = 200000

[llm.backends.openai]
backend_type = "openai-compat"
model = "gpt-4"
api_key_cmd = "echo $OPENAI_API_KEY"
base_url = "https://api.openai.com/v1"
max_content_chars = 128000
```

## 错误处理
模块使用`anyhow::Result`进行错误处理，包括：
- API调用失败（HTTP错误、网络问题）
- 响应格式错误（缺少必需字段）
- 配置错误（缺少必需参数）
- 命令执行失败（获取API密钥失败）

## 测试
模块包含完整的单元测试，覆盖：
- API密钥解析
- 后端创建
- 提示模板构建
- 错误处理场景