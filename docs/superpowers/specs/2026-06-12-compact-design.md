# /compact 命令设计 (issue #641)

## 背景与目标

聊天线程的历史以 JSONL 形式存储，每轮请求全量加载发送给 LLM。长线程中体积最大的
通常是工具调用结果（tool_result），随着轮数增加会迅速消耗上下文窗口。

`/compact` 提供手动压缩能力：把历史中体积大的 tool_result 替换为截断占位文本，
缩减后续请求的上下文长度。压缩前完整快照原文件，供后续查找原始内容。

不在本期范围：自动触发（按 token 阈值）、LLM 摘要式压缩、压缩 thinking 或
assistant 文本。

## 命令流程

沿用 `/thread` 系列命令的既有模式：

1. 客户端（chat_session.rs）新增 `/compact` handler：
   - 无活动线程（`current_thread_id` 为空）时报错提示
   - 有则发送 `__cmd:compact:<tid>` 的 `Message::Request` 到守护进程
   - 渲染守护进程返回的 display 文本
2. 守护进程 `handle_builtin_command`（server.rs）新增 `compact:<tid>` 分支：
   - 校验 `thread_exists`
   - 检查 `pending_loops`：该线程有暂停等待客户端工具结果的 agent loop 时拒绝
     压缩并提示稍后再试（注意不能用 `active_threads`，发起 /compact 的会话自己
     就持有该线程的 claim）
   - `bump_generation`：使任何仍在等待 LLM 响应的 in-flight 循环失效。既有的
     generation 失配机制保证这类循环会丢弃工作而不写盘，避免按 `saved_up_to`
     增量追加时与重写后的文件错位
   - 调用 `ConversationManager::compact_thread(tid)`，返回统计信息
3. 全程无 LLM 调用，同步完成

返回示例：`compacted 12 tool results: 84.3k -> 6.1k chars`；无可压缩内容时返回
`nothing to compact`。

## 压缩算法（ConversationManager::compact_thread）

核心逻辑实现为可单测的纯函数 `compact_messages(msgs: &mut Vec<Value>) -> CompactStats`，
`compact_thread` 负责快照、调用纯函数、落盘。

- 定位 user 消息 content 数组中的 `tool_result` block。其 `content` 字段兼容两种
  格式：纯字符串、content block 数组（逐个处理其中的 text block）
- 保护窗口：最近 2 轮不压缩。以真实用户文本消息（复用 `is_user_message` 判定，
  排除纯 tool_result 消息）为轮边界，从尾部向前数
- 压缩条件：文本超过 30 行才处理（保留 10+10 行后至少省 10 行才值得压）
- 压缩格式：保留前 10 行 + `... [compacted: omitted N lines, M chars] ...` +
  后 10 行
- 幂等：占位标记 `[compacted:` 同时作为已压缩标志，含标记的 block 跳过，
  重复执行 `/compact` 不会再压
- 行数阈值与保护轮数本期硬编码为常量，不做配置项

## 快照保留

- 有实际变更时，重写前把当前 JSONL 完整复制到
  `threads/precompact/{tid}.{unix_ts}.jsonl`
- 必须放子目录：`get_latest_thread()` 等会扫描 threads 目录下所有 `*.jsonl`，
  同目录快照会被误认成线程文件
- 多次 compact 产生多个快照，内容有重叠但互不干扰；本期不做自动清理
  （后续可挂入 disk_cleanup 任务）
- `delete_thread` 时一并删除该线程的所有快照文件

## 一致性保证

- `replace_messages` 同时更新内存 map 与磁盘文件，主 JSONL 保持
  "下一轮发送内容的精确副本" 这一既有不变量（`_usage`/`_model` 字段发送前剥离，
  属既有行为，不受影响）
- 压缩在 `__cmd` 请求处理中同步执行，与 agent loop 的互斥由 `pending_loops`
  检查 + `bump_generation` 保证

## 错误处理

- 线程不存在：`Error: thread not found`
- 线程有进行中请求：提示等待当前回复完成
- 快照写入失败：中止压缩并返回错误（不允许无快照的破坏性重写）

## 测试

- conversation_mgr 单元测试：
  - string 与数组两种 tool_result content 格式
  - 保护窗口（最近 2 轮不动，更早的被压）
  - 行数阈值边界（30 行不压、31 行压）
  - 幂等性（二次 compact 无变化、不产生新快照）
  - 快照文件生成与内容完整性
  - delete_thread 清理快照
- server 侧分支（pending_loops 拒绝）构造成本高，以手动验证代替
- 视情况补一条 integration test（tools/integration_tests）
