# LLM Cache Hint — 缓存策略上移

- Issue: #550
- Date: 2026-04-17

## Problem

`crates/omnish-llm/src/anthropic.rs` 中的 `cache_control` 放置逻辑耦合了上层业务语义：

- 多轮路径假设倒数第二条 message 是缓存边界（隐含"最后一条带 system-reminder，会变"的业务知识）
- system_prompt 与 tools 的缓存默认行为硬编码在 backend 内
- 单轮路径完全不缓存（context+query 拼成单一 block，无 cache_control）

这使得：
- backend 兼任了"在哪打缓存"的策略，违反单一职责
- TTL（5min vs 1h）无法按上层意图选择
- 需要新缓存策略时，必须改 backend 而不是上层调用点

## Goal

把"哪里缓存、缓存多久"的决策权从 backend 移到上层（daemon/server），backend 只负责把上层的意图翻译成对应 wire 协议字段。

非目标：
- 不优化 OpenAI-compat backend 的缓存（其 prompt caching 由服务端自动处理，客户端无需标记）
- 不修改 system-reminder 注入位置或重构 ChatTurn
- 不引入 cache 命中率的 telemetry 统计（独立工作项，本 spec 不涉及）

## Design

### CacheHint 类型

引入 backend-agnostic 的缓存生命周期标记：

```rust
// crates/omnish-llm/src/backend.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheHint {
    #[default]
    None,
    /// Anthropic: ephemeral，默认 5min TTL
    Short,
    /// Anthropic: ephemeral, ttl="1h"
    Long,
}
```

- Anthropic backend：翻译为对应 `cache_control` 字段
- OpenAI-compat backend：完全忽略（编译时无副作用，运行时无操作）

### 可缓存单元的内嵌 hint

每个可缓存单元在类型上携带自己的 `CacheHint`，由上层在 build request 时设置：

```rust
pub struct CachedText {
    pub text: String,
    pub cache: CacheHint,
}

pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub cache: CacheHint,             // 新增字段；默认 None
}

pub struct TaggedMessage {
    pub content: serde_json::Value,
    pub cache: CacheHint,             // 默认 None
}
```

`LlmRequest` 对应字段类型变更：

```rust
pub struct LlmRequest {
    // ...保留：context, query, trigger, session_ids, use_case,
    //        max_content_chars, enable_thinking, tools, ...
    pub system_prompt: Option<CachedText>,        // 之前是 Option<String>
    pub extra_messages: Vec<TaggedMessage>,       // 之前是 Vec<serde_json::Value>
    // 删除：conversation: Vec<ChatTurn>          // dead 字段
}
```

`tools: Vec<ToolDef>` 字段名保持，只是 `ToolDef` 内部新增 `cache` 字段。

### Backend 翻译规则（Anthropic）

`anthropic.rs` 内删除以下硬编码逻辑：
- `inject_cache_control()`（"second-to-last 消息"启发式）
- system 段固定 `cache_control: ephemeral` 的默认行为
- tools 末尾固定 `cache_control: ephemeral` 的默认行为

替换为单一翻译函数：

```rust
fn cache_control_value(hint: CacheHint) -> Option<serde_json::Value> {
    match hint {
        CacheHint::None  => None,
        CacheHint::Short => Some(serde_json::json!({"type": "ephemeral"})),
        CacheHint::Long  => Some(serde_json::json!({"type": "ephemeral", "ttl": "1h"})),
    }
}
```

应用规则：
- **system**：若 `req.system_prompt` 是 `Some(ct)` 且 `ct.cache != None` → 在该 system content block 上写 `cache_control`
- **tools**：遍历 `req.tools`，对每个 `cache != None` 的 tool 在其 wire JSON 上写 `cache_control`
- **messages**：经预算管控（见下）后，对生效的 hint，在该 message `content` 数组的最后一个 block 上写 `cache_control`（保留今天 `inject_cache_control` 对 String/Array 内容的处理形式）

### Anthropic backend 内的预算管控

Anthropic 一次请求最多 4 个 cache_control 断点。Backend 在序列化前做一次裁剪：

```rust
fn enforce_breakpoint_budget(req: &LlmRequest) -> Vec<CacheHint> {
    const MAX: usize = 4;

    let used_static =
        req.tools.iter().filter(|t| t.cache != CacheHint::None).count()
        + req.system_prompt.as_ref()
              .map_or(0, |s| (s.cache != CacheHint::None) as usize);
    let remaining = MAX.saturating_sub(used_static);

    let marked: Vec<usize> = req.extra_messages.iter()
        .enumerate()
        .filter(|(_, m)| m.cache != CacheHint::None)
        .map(|(i, _)| i)
        .collect();

    if marked.len() > remaining {
        tracing::warn!(
            "cache breakpoint budget exceeded: {} static + {} message hints, \
             dropping {} earliest message hints (max breakpoints = {})",
            used_static, marked.len(), marked.len() - remaining, MAX
        );
    }

    let kept: std::collections::HashSet<usize> =
        marked.iter().rev().take(remaining).copied().collect();

    req.extra_messages.iter().enumerate()
        .map(|(i, m)| if kept.contains(&i) { m.cache } else { CacheHint::None })
        .collect()
}
```

策略：**保留最靠后的 N 个 message marker**（最近的最有价值）。被裁剪的降级为 `None`，并发一条 `tracing::warn!`。

### 上层调用点 policy

#### Chat agent loop（`server.rs` 的 chat 路径，约 line 1266 / 2802）

```rust
// system prompt：整 session 稳定 → Long
let system_prompt = Some(CachedText {
    text: full_system_prompt,
    cache: CacheHint::Long,
});

// tools 不打 hint（保持 None）
//   - 单一断点策略：system 上的 Long 已经自动覆盖 tools 段（前缀顺序 tools→system→messages）
//   - 留出 tools 段的独立断点不必要

// messages：每次 LLM 调用前重新标记。先清除旧标记（agent loop
// 多次迭代会追加新消息，旧 marker 若不清会累积超出 budget），再
// 把当前的 last-2 标 Long
for m in extra_messages.iter_mut() {
    m.cache = CacheHint::None;
}
let len = extra_messages.len();
for i in 0..2.min(len) {
    extra_messages[len - 1 - i].cache = CacheHint::Long;
}
```

总断点占用：1 (system) + 2 (messages) = 3，留 1 slack。

为什么 last-2 而不是 last-3：
- 在 active 多轮场景下 last-2 提供了"最新写 + 一条可能命中位"，agent loop 多轮迭代时 ladder 仍工作
- 留 1 个 budget slack 便于将来策略调整时不需要立即收缩

为什么用 Long（不区分 tool_use/tool_result vs final response）：
- build request 时无法预知 LLM 会返回 `EndTurn` 还是再来一轮 `ToolUse`，所以"识别 final response"只能事后判断，无法用于本次 marker
- 与 Short 相比，Long 在跨 turn 场景（用户停顿数分钟到 1 小时）能持续命中；写溢价 0.75× base 极小
- Telemetry 拉起后再视情况差异化

#### Completion warmup（`server.rs` 约 line 1991）

```rust
// 用 build_simple_completion_content 构建后，把整段 prompt 作为 user message
let extra_messages = vec![TaggedMessage {
    content: serde_json::json!({"role": "user", "content": prompt}),
    cache: CacheHint::Long,
}];
```

补全 warmup 频繁触发且 context 在多分钟尺度上稳定 → Long 划算。

#### 其他 5 个调用点

`hourly_summary.rs` / `daily_notes.rs` / `thread_summary.rs` / `summarize_tool_result`（server.rs ~377）/ Completion 主路径（server.rs ~2883）：

- `system_prompt: None`（无变更）
- `extra_messages: vec![]`（无变更）
- 仅类型适配：原 `Option<String>` → `Option<CachedText>`，原 `vec![]` → `Vec<TaggedMessage>::new()`

#### 删除 `conversation` 字段

所有 7 个调用点当前都传 `vec![]`。Backend 内部相关分支（`if req.conversation.is_empty() && req.extra_messages.is_empty()` 单轮路径）需要改写为只检查 `extra_messages`。`anthropic.rs` 与 `openai_compat.rs` 内构建 messages 的循环 `for turn in req.conversation.iter()` 整体删除。

单轮 fallback（`extra_messages` 为空时用 `build_user_content` 拼 user message）保留，仅依据 `extra_messages.is_empty()` 判定。

## Wire 翻译示例

输入：
```rust
LlmRequest {
    system_prompt: Some(CachedText { text: "...", cache: CacheHint::Long }),
    tools: vec![
        ToolDef { name: "a", ..., cache: CacheHint::None },
        ToolDef { name: "b", ..., cache: CacheHint::None },
    ],
    extra_messages: vec![
        TaggedMessage { content: m0, cache: CacheHint::None },
        TaggedMessage { content: m1, cache: CacheHint::None },
        TaggedMessage { content: m2, cache: CacheHint::Long },
        TaggedMessage { content: m3, cache: CacheHint::Long },
    ],
    ...
}
```

发送到 Anthropic 的 wire JSON：
```json
{
  "model": "...",
  "system": [
    {"type": "text", "text": "...", "cache_control": {"type":"ephemeral","ttl":"1h"}}
  ],
  "tools": [
    {"name":"a", ...},
    {"name":"b", ...}
  ],
  "messages": [
    m0,
    m1,
    {... m2 with last content block having cache_control ephemeral 1h ...},
    {... m3 with last content block having cache_control ephemeral 1h ...}
  ]
}
```

发送到 OpenAI-compat 的 wire JSON：所有 cache_control 字段都不出现。

## Testing

新增 `crates/omnish-llm/tests/cache_hint_test.rs`：

1. **基本翻译**：构造一个 `LlmRequest`（system Long + 2 messages Long），断言 Anthropic body 在 `system[0].cache_control` 与最后两条 messages 末位 block 上有正确字段；断言 OpenAI body 不含任何 cache_control。
2. **TTL 区分**：Short 不带 ttl 字段，Long 带 `"ttl":"1h"`。
3. **预算裁剪**：构造 `system Long + 1 tool Long + 5 messages Long`（共 7 marker），断言只有最靠后的 2 条 messages 保留 marker（4 - 2 = 2），并验证 `tracing::warn!` 已触发（用 `tracing-test` 或 captured logs）。
4. **空请求**：所有字段 None / 空 → 不出现任何 cache_control。
5. **OpenAI 转换不丢消息**：原 `extra_messages` 内含 tool_use/tool_result 的多 block 消息，转换后消息序列正确（这是回归测试，不依赖 cache hint）。

修改 `crates/omnish-llm/tests/llm_test.rs`：现有断言适配新字段类型（`Option<CachedText>`、`Vec<TaggedMessage>`）。

集成测试 `tools/integration_tests`：现有 chat 相关 case 应在改完后照常通过；如果有断言 wire body 的 case，更新对应字段。

## Migration

无在野持久化数据格式变更（`conversation_mgr` 持久化的仍然是 `serde_json::Value`，与 `TaggedMessage.content` 一致），仅代码内部类型迁移。

`ToolDef` 的新 `cache` 字段在所有现有 `ToolDef::default()` / 构造点上以 `CacheHint::None` 初始化，不影响外部插件协议（plugin 的 `tool.json` 不包含 cache 字段）。

## Out of Scope

- Cache 命中率/经济性 telemetry：`Usage.cache_read_input_tokens` 已采集，但 daemon 端按 message 类型/位置聚合统计是独立工作项
- 差异化 hint（assistant_with_tool_use → Short，pure_text → Long 等）：等 telemetry 出现后基于数据再决定
- OpenAI-compat 的 prompt caching：服务端自动处理，无需客户端 marker
