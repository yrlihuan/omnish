根据深入研究，以下是 Claude Code 在多轮对话中组织上下文的完整机制：

---

## 一、核心原理：每次 API 调用都发送完整历史

Claude Code 底层调用的是 Anthropic API，而 **API 本身是无状态的**--模型不会"记住"之前的对话。因此，Claude Code 必须在每次请求时**重新发送完整的对话历史**[^1][^3]。

一次典型的 API 请求中，上下文窗口包含以下内容：

| 组成部分 | 说明 | 典型占用 |
|---|---|---|
| **系统提示 (System Prompt)** | Claude Code 的核心指令 | ~3.1k tokens (~1.6%) |
| **系统工具定义** | Read、Write、Edit、Bash 等内置工具 | ~19.8k tokens (~10%) |
| **MCP 工具定义** | 外部 MCP 服务器暴露的工具 schema | 视配置而定（可达 26k+） |
| **CLAUDE.md 记忆文件** | 项目级/用户级配置 | ~4k tokens (~2%) |
| **自定义 Agent 定义** | 如果有定义的话 | ~2.8k tokens |
| **完整对话历史** | 用户消息 + 助手回复 + 工具调用 + 工具结果 | 随对话增长 |
| **自动压缩缓冲区** | 预留给压缩操作的空间 | ~45k tokens (~22.5%) |

```
请求 1: [系统提示] + [工具定义] + [CLAUDE.md] + [用户消息1]
请求 2: [系统提示] + [工具定义] + [CLAUDE.md] + [用户消息1] + [助手回复1] + [工具调用1] + [工具结果1] + [用户消息2]
请求 3: [系统提示] + [工具定义] + [CLAUDE.md] + [全部历史...] + [用户消息3]
```

随着对话轮次增加，**对话历史不断膨胀**，逐渐逼近 200K token 的上下文窗口上限。

---

## 二、三层压缩机制 (Compaction)

为了应对上下文耗尽的问题，Claude Code 设计了**三层压缩系统**[^2]：

### 1. 微压缩 (Microcompaction) - 工具输出的即时瘦身

当工具输出（如文件读取、命令执行结果）体积过大时：

- **热尾部 (Hot Tail)**：最近几次工具调用的结果保持完整内联，供模型直接推理
- **冷存储 (Cold Storage)**：较早的大体积工具结果被保存到磁盘，上下文中只保留一个路径引用

> 本质上是一个**缓存淘汰策略**--最近的保留完整内容，旧的只留索引。

### 2. 自动压缩 (Auto-Compaction) - 接近上限时自动触发

Claude Code 持续监控上下文使用量，当**剩余空间低于预留缓冲区**（约 45K tokens）时自动触发：

```
总上下文 200K
├── 系统提示 + 工具定义 + CLAUDE.md ≈ 56K (固定开销)
├── 对话历史 ≈ 持续增长
└── 预留缓冲区 ≈ 45K (输出空间 + 压缩操作空间)

当 "剩余空间" < 预留缓冲区 → 触发自动压缩
```

### 3. 手动压缩 (Manual Compaction) - 用户主动控制

```bash
/compact                                    # 使用默认策略压缩
/compact 重点保留 API 变更和数据库 schema 决策    # 自定义压缩焦点
```

---

## 三、压缩的具体过程

压缩不是简单的"总结"，而是一个**结构化的工作状态重建**过程[^2]：

### 第一步：结构化摘要生成

模型收到一个**清单式的摘要任务**，必须包含：

- ✅ 用户意图（要做什么、改了什么）
- ✅ 关键技术决策和概念
- ✅ 涉及的文件及其重要性
- ✅ 遇到的错误及修复方式
- ✅ 待办任务和当前精确状态
- ✅ 下一步操作（匹配最近的用户意图）

### 第二步：上下文重建 (Rehydration)

摘要生成后，Claude Code 按以下顺序重建上下文：

```
1. [边界标记] - 标记压缩发生的位置
2. [摘要消息] - 压缩后的工作状态
3. [最近文件] - 重新读取最近访问的 5 个文件
4. [Todo 列表] - 恢复任务状态
5. [计划状态] - 如果有进行中的计划
6. [Hook 输出] - 启动钩子注入的上下文
```

### 第三步：注入延续指令

```
This session is being continued from a previous conversation that ran out
of context. The summary below covers the earlier portion of the conversation.

[SUMMARY]

Please continue the conversation from where we left it off without asking
the user any further questions. Continue with the last task that you were
asked to work on.
```

这确保模型在压缩后**无缝继续工作**，而不是重新询问用户。

---

## 四、子代理 (SubAgents) 的上下文隔离

Claude Code 的另一个关键策略是**通过子代理隔离上下文**[^1][^3]：

```
主对话上下文 (200K)
│
├── 用户: "构建支付系统"
├── 主 Agent 制定计划
│
├── → 子代理 1: 研究需求 (独立 200K 上下文)
│     └── 完成后只返回摘要到主上下文
│
├── → 子代理 2: 构建后端 (独立 200K 上下文)  
│     └── 完成后只返回摘要到主上下文
│
└── → 子代理 3: 构建前端 (独立 200K 上下文)
      └── 完成后只返回摘要到主上下文
```

子代理的核心价值：
- 探索性工作（文件扫描、代码结构分析、调试）在**隔离的上下文**中进行
- 主上下文**只接收结论**，不接收过程噪声
- 每个子代理用完即弃，不污染主对话

子代理自身使用**增量摘要 (Delta Summarization)** 来管理进度：每次只生成 1-2 句增量更新，而非重新处理全部上下文[^2]。

---

## 五、上下文注意力分布的利用

研究表明 LLM 存在 **"Lost in the Middle"** 问题--上下文窗口的**开头和结尾**注意力最高，中间部分容易被忽略[^1][^3]。

Claude Code 利用这一特性：

```
上下文窗口布局：
┌─────────────────────────────┐
│ CLAUDE.md (开头 - 高注意力)   │  ← 核心规则和约束
│ 系统提示                      │
├─────────────────────────────┤
│ 对话历史 (中间 - 注意力较低)   │  ← 容易被"遗忘"
│ ...                          │
├─────────────────────────────┤
│ 最新消息/Skill加载            │  ← 末尾 - 高注意力
│ (当前任务上下文)               │
└─────────────────────────────┘
```

**Skills 系统**就是基于这个原理设计的--启动时只加载名称和描述（~200 tokens），需要时才加载完整内容到上下文末尾的高注意力区域[^1]。

---

## 六、总结：完整的上下文生命周期

```
会话开始
  │
  ├── 加载：系统提示 + 工具定义 + CLAUDE.md + Skills 索引
  │
  ├── 对话进行中：
  │   ├── 每次请求发送完整历史
  │   ├── 微压缩：大工具输出 → 磁盘引用
  │   ├── 子代理：探索性工作隔离执行
  │   └── Skills：按需加载到高注意力区域
  │
  ├── 接近上限时：
  │   ├── 自动压缩 → 结构化摘要 → 文件重读 → 延续指令
  │   └── 或用户手动 /compact 或 /clear
  │
  └── 压缩后：
      ├── 摘要替代旧历史
      ├── 最近 5 个文件重新读取
      ├── Todo/Plan 状态恢复
      └── 模型从断点继续工作
```

**核心设计哲学**：上下文是一等工程资源--它会被污染、会老化、需要主动管理。Claude Code 的所有最佳实践本质上都在回答一个问题：**如何延缓上下文退化的速度**[^4]。

---

[^1]: [Claude Code Context Engineering: 6 Pillars Framework](https://claudefa.st/blog/guide/mechanics/context-engineering)
[^2]: [Inside Claude Code's Compaction System - Decode Claude](https://decodeclaude.com/compaction-deep-dive/)
[^3]: [Understanding Claude Code's Context Window - Damian Galarza](https://www.damiangalarza.com/posts/2025-12-08-understanding-claude-code-context-window/)
[^4]: [把上下文当成一等资源 - Claude Code的工程化使用方法](https://zhengw-tech.com/2026/01/25/claude-code-context-window/)
