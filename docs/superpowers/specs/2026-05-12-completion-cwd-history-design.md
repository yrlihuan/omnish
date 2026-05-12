# 补全请求加入当前 CWD 的命令历史

- Issue: #610
- Date: 2026-05-12

## Problem

当前 auto-completion 的 LLM context 包含跨会话的 `<history>`（命令行列表）和 `<recent>`（带输出的最近命令），按时间排序，但**不区分 cwd**。结果是：

- LLM 推断「用户接下来要做什么」时，只能依赖近期跨目录的全局活动，忽略了「同一目录下用户常做什么」这个最强信号。
- 用户刚切到一个老目录时，最相关的历史命令可能被几小时前其它目录的活动淹没。

## Goal

在补全请求的 LLM context 末尾追加一个新区块 `<cwd_history>`，列出**当前 cwd 下**最近的若干条命令（可选按用户输入前缀过滤），帮助 LLM 抓住「同目录工作模式」和「命令序列模式」。

非目标：

- 不改变现有 `<history>` / `<recent>` 的产生逻辑。
- 不引入新的存储或索引，复用 `SessionManager.all_commands` 的内存数据。
- 不做模糊匹配 / 跨目录召回 / 频次重排。

## Design

### 行为规则

触发条件：`cwd_history_limit > 0` 且当前为真实补全请求（非 KV cache warmup）。

匹配规则（在所有会话的 `CommandRecord` 集合上）：

1. `shorten_home(cmd.cwd) == shorten_home(current_cwd)` —— 严格相等。两端都先走 `omnish_context::shorten_home`，避免 `/home/huan` 与 `~/` 的不一致。
   - `current_cwd` 优先取 `req.cwd`（客户端在请求时刻报告，最准确）；为 `None` 时回退到 `live_cwd`（来自 `meta.attrs.shell_cwd`）；两者都没有时跳过该块。
2. `cmd.command_line.starts_with(prefix)` —— 其中 prefix 的语义是「光标前的用户输入」`req.input[..req.cursor_pos]`。当前实现里客户端固定 `cursor_pos = input.len()`（见 completion.rs:486），所以可以直接用 `&req.input`；spec 层面记录前缀语义，避免未来 cursor_pos 改变时把代码改坏。`prefix` 为空时该条件恒真（退化为「该 cwd 最近 N 条」）。

排序与上限：

- 按 `started_at` 升序排（旧→新），保留 `a b c` 这类执行序列的可推断性。
- 取末尾 `cwd_history_limit` 条（即满足条件的最新 N 条，按时间升序输出）。

去重：**不去重**。重复执行的命令保留原样，序列信息优先于去噪。

空结果：完全跳过该块，不输出空标签。

### 块格式

```
<cwd_history>
git status
git diff src/foo.rs
git add src/foo.rs
git commit -m "fix typo"
</cwd_history>
```

- 仅 `command_line`，每行一条。
- 无输出、无时间戳、无 cwd 标签（同 cwd 不必重复）、无失败标记（避免诱导 LLM 回避刚失败但用户正在重试的命令）。

### 数据流

#### 1. 配置（omnish-common）

`CompletionContextConfig` 新增字段：

```rust
#[serde(default = "default_cwd_history_limit", deserialize_with = "string_or_int::deserialize")]
pub cwd_history_limit: usize,
```

`default_cwd_history_limit()` 返回 `10`。`0` 表示禁用该特性。

#### 2. 类型扩展（omnish-context）

`CompletionSections` 新增字段：

```rust
pub struct CompletionSections {
    pub stable_prefix: String,  // <history> + frozen <recent>
    pub remainder: String,      // tail <recent> + <system-reminder>
    pub cwd_history: String,    // NEW: <cwd_history>...</cwd_history>，可能为空
}
```

`CompletionFormatter` 新增一个辅助方法 `format_cwd_history(commands)` 渲染上述块（不挂在 `format_sections` 上以避免它依赖与 byte-stability 无关的输入）。

#### 3. 选择与渲染（omnish-daemon::session_mgr）

`build_completion_sections` 签名扩展：

```rust
pub async fn build_completion_sections(
    &self,
    current_session_id: &str,
    max_context_chars: Option<usize>,
    cwd_query: Option<CwdQuery<'_>>,  // NEW
) -> Result<CompletionSections>

pub struct CwdQuery<'a> {
    pub cwd: &'a str,      // 已 shorten_home 的当前 cwd
    pub prefix: &'a str,   // 命令前缀，可为空
}
```

- `cwd_query = None`：跳过 cwd_history 计算（warmup 路径）。
- `cwd_query = Some(q)`：在已经聚合好的 `all_commands` 上按 `q.cwd` + `q.prefix` 做一次过滤 → 排序 → 截断 → 调用 `format_cwd_history` 渲染。

实现细节：

- `current_cwd` 由调用方决定：`handle_completion_request` 优先用 `req.cwd`，回退到 session 的 `live_cwd`。把决定后的 cwd 直接以 `CwdQuery.cwd` 传入，`build_completion_sections` 内部不再处理回退逻辑。
- 为支持调用方拿到 `live_cwd`，给 `SessionManager` 加一个简单 getter：`pub async fn get_live_cwd(&self, session_id: &str) -> Option<String>`，读 `meta.attrs.shell_cwd` 并 `shorten_home`。
- `cmd.cwd` 在过滤时即时 `shorten_home`。
- 当前 cwd 不可知时调用方传 `None`，跳过该块。

#### 4. 透传与组装（omnish-daemon::server）

`handle_completion_request`：

```rust
let cwd = req.cwd.as_deref()
    .map(omnish_context::shorten_home)
    .or_else(|| mgr.get_live_cwd(&req.session_id));
let cwd_query = cwd.as_deref().map(|c| CwdQuery {
    cwd: c,
    prefix: req.input.as_str(),  // cursor_pos 当前固定为 input.len()
});
let sections = mgr.build_completion_sections(
    &req.session_id, max_context_chars, cwd_query,
).await?;
```

`maybe_warmup_completion`（KV cache 预热路径，server.rs:2308 附近）：

```rust
let sections = mgr.build_completion_sections(
    session_id, max_chars, None,
).await?;
```

`build_completion_extra_messages` 调整：blocks 序列从 `[stable_prefix, remainder, query]` 变为 `[stable_prefix, remainder, cwd_history, query]`，每个 block 非空才插入。`cache_pos` 仍只挂在 `stable_prefix` 所在 block（block 0）。

#### 5. 采样

`prompt_for_sample` 与 `context_for_sample` 把 `cwd_history` 也拼进去，使补全采样记录的 prompt 长度与实际发送一致。

### 缓存影响

- `stable_prefix` 与 `remainder` 不动 → KV cache 命中率与现状一致。
- `cwd_history` 是独立 block，每个键击都可能变化 → 永不缓存，符合 Anthropic 的 cache_control 模型。
- block 末尾紧接 `query`（即 `build_completion_parts` 产出的 user_input 描述），LLM 看到的最后一段就是「（最相关的）cwd 历史 + 当前输入」，符合「context 末尾 = 最相关」的注意力直觉。

### 测试

omnish-context：

- `CompletionFormatter::format_cwd_history`：空 commands → 空字符串；含 commands → 包裹在 `<cwd_history>` 内、保持顺序。

omnish-daemon::session_mgr：

- `build_completion_sections` with `Some(CwdQuery { cwd, prefix: "" })`：返回 `cwd` 下最近 N 条命令（无 prefix 过滤），不包含其他 cwd 的命令。
- `build_completion_sections` with `Some(CwdQuery { cwd, prefix: "git" })`：仅返回 `cwd` 下 `command_line.starts_with("git")` 的命令。
- `build_completion_sections` with `None`：cwd_history 字段为空。
- `cwd_history_limit = 0`：cwd_history 字段恒为空。
- 多会话同 cwd：跨会话命令合并后按时间排。

omnish-daemon::server：

- `build_completion_extra_messages` 在 `cwd_history` 为空时只产出 3 个 block；非空时 4 个 block，顺序正确。

### 影响范围

| 文件 | 改动 |
|---|---|
| `crates/omnish-common/src/config.rs` | `CompletionContextConfig` 加字段 + default |
| `crates/omnish-daemon/src/config_schema.toml` | 新增 schema item + i18n key |
| `crates/omnish-context/src/recent.rs` | `CompletionSections` 加字段；`CompletionFormatter::format_cwd_history` |
| `crates/omnish-daemon/src/session_mgr.rs` | `build_completion_sections` 签名 + 实现；新增 `get_live_cwd` getter |
| `crates/omnish-daemon/src/server.rs` | `handle_completion_request` 传 prefix；warmup 传 `None`；`build_completion_extra_messages` 多一个 block；采样字段拼 cwd_history |
| 客户端 i18n 字符串 | 新配置项的中英文标签 |

### 配置默认值

```toml
[context.completion]
cwd_history_limit = 10
```

升级时旧配置文件无此字段会走 default，行为自动启用。用户可显式设为 `0` 关掉。
