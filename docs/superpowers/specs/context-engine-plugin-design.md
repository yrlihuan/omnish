# ContextEngine 插件系统设计文档

## 概述

ContextEngine 是 OpenClaw 的可插拔上下文管理抽象层，允许外部插件通过标准插件 API 注册自定义的上下文管理策略，替代内置的 legacy 上下文处理流程。整个系统遵循 **接口定义 → 注册表管理 → 插件注入 → 生命周期集成** 的设计路径。

## 架构总览

```
┌─────────────────────────────────────────────────────┐
│                  Plugin (外部插件)                    │
│  api.registerContextEngine("my-engine", factory)     │
└─────────────────┬───────────────────────────────────┘
                  │ 注册
                  ▼
┌─────────────────────────────────────────────────────┐
│            Context Engine Registry                   │
│  (模块级单例, Symbol.for 跨模块安全)                   │
│  Map<string, ContextEngineFactory>                   │
│  ┌───────────┬───────────┬───────────┐              │
│  │  legacy   │ my-engine │   ...     │              │
│  └───────────┴───────────┴───────────┘              │
└─────────────────┬───────────────────────────────────┘
                  │ resolveContextEngine(config)
                  │ 根据 config.plugins.slots.contextEngine 选择
                  ▼
┌─────────────────────────────────────────────────────┐
│           Agent Run Lifecycle (run.ts)                │
│                                                      │
│  1. bootstrap    ─ 初始化引擎状态                      │
│  2. assemble     ─ 组装模型上下文                      │
│  3. afterTurn    ─ 后置生命周期 (摄入+压缩决策)          │
│  4. compact      ─ 溢出压缩                           │
│  5. dispose      ─ 清理资源                           │
│                                                      │
│  Subagent hooks:                                     │
│  - prepareSubagentSpawn                              │
│  - onSubagentEnded                                   │
└──────────────────────────────────────────────────────┘
```

## 核心组件

### 1. ContextEngine 接口 (`src/context-engine/types.ts`)

定义了上下文引擎必须实现的生命周期契约：

| 方法 | 必选 | 说明 |
|------|------|------|
| `info` | 是 | 引擎元数据 (id, name, version, ownsCompaction) |
| `bootstrap` | 否 | 会话初始化时从 session 文件导入历史上下文 |
| `ingest` | 是 | 将单条消息摄入引擎存储 |
| `ingestBatch` | 否 | 批量摄入一个完整 turn 的消息 |
| `afterTurn` | 否 | run attempt 完成后的后置钩子，可做持久化和后台压缩决策 |
| `assemble` | 是 | 在 token 预算内组装模型上下文，返回有序消息列表 |
| `compact` | 是 | 压缩上下文以减少 token 使用 (摘要、裁剪旧 turn 等) |
| `prepareSubagentSpawn` | 否 | 子代理启动前的上下文准备 (含 rollback 句柄) |
| `onSubagentEnded` | 否 | 子代理结束时的清理通知 |
| `dispose` | 否 | 释放引擎持有的资源 |

关键类型：

```typescript
// assemble 的返回值可以附带 system prompt 增量
type AssembleResult = {
  messages: AgentMessage[];
  estimatedTokens: number;
  systemPromptAddition?: string;  // 追加到运行时 system prompt
};

// 引擎元数据中的 ownsCompaction 标志
type ContextEngineInfo = {
  id: string;
  name: string;
  ownsCompaction?: boolean;  // 为 true 时禁用内置自动压缩
};
```

### 2. 注册表 (`src/context-engine/registry.ts`)

基于 `Symbol.for("openclaw.contextEngineRegistryState")` 的全局单例 Map，确保跨模块/dist chunk 重载安全：

```typescript
// 注册
registerContextEngine(id: string, factory: ContextEngineFactory): void

// 解析 - 根据配置的 slot 值选取引擎
resolveContextEngine(config?: OpenClawConfig): Promise<ContextEngine>
```

**解析优先级：**
1. `config.plugins.slots.contextEngine` - 用户显式指定
2. `defaultSlotIdForKey("contextEngine")` - 回退默认值 `"legacy"`

### 3. LegacyContextEngine (`src/context-engine/legacy.ts`)

透传实现，包装现有压缩行为，保证 100% 向后兼容：

- `ingest` → no-op (SessionManager 负责消息持久化)
- `assemble` → 透传 (现有的 sanitize → validate → limit 管道在 attempt.ts 处理)
- `compact` → 委托给 `compactEmbeddedPiSessionDirect`
- `afterTurn` → no-op
- `ownsCompaction` 未设置 → 保留内置自动压缩

### 4. 插件 Slot 系统 (`src/plugins/slots.ts`)

上下文引擎作为排他性 slot 注册，同一时刻只有一个 context engine 处于激活状态：

```typescript
// PluginKind → SlotKey 映射
const SLOT_BY_KIND = {
  memory: "memory",
  "context-engine": "contextEngine",
};

// 默认 slot 值
const DEFAULT_SLOT_BY_KEY = {
  memory: "memory-core",
  contextEngine: "legacy",
};
```

配置结构 (`src/config/types.plugins.ts`)：

```typescript
type PluginSlotsConfig = {
  memory?: string;
  contextEngine?: string;  // 选择激活的 context engine id
};
```

当一个新的 context engine 插件被选中时，`applyExclusiveSlotSelection` 会自动禁用同 slot 的其他插件。

## 插件注入流程

### 步骤 1：插件定义与注册

外部插件通过标准 `OpenClawPluginDefinition` 声明 `kind: "context-engine"`，并在 `register` 回调中调用 `api.registerContextEngine`：

```typescript
// 外部插件示例
export default {
  id: "my-context-engine",
  name: "My Context Engine",
  kind: "context-engine",
  register(api) {
    api.registerContextEngine("my-context-engine", () => {
      return new MyContextEngine();
    });
  },
} satisfies OpenClawPluginDefinition;
```

### 步骤 2：加载与连接

插件加载链路：

```
loadGatewayPlugins()
  → loadOpenClawPlugins()
    → discoverOpenClawPlugins()    // 发现插件
    → createPluginRegistry()       // 创建注册表
    → plugin.register(api)         // 调用插件注册函数
      → api.registerContextEngine(id, factory)
        → registerContextEngine(id, factory)  // 写入全局 Map
```

`api.registerContextEngine` 在 `src/plugins/registry.ts:603` 中绑定，直接代理到 `src/context-engine/registry.ts` 的模块级注册函数。

### 步骤 3：初始化守卫

`ensureContextEnginesInitialized()` (`src/context-engine/init.ts`) 确保内置的 legacy 引擎只注册一次：

```typescript
let initialized = false;
export function ensureContextEnginesInitialized(): void {
  if (initialized) return;
  initialized = true;
  registerLegacyContextEngine();  // 注册 "legacy" 到 Map
}
```

该函数在 agent run 开始前调用，保证即使没有任何插件，`"legacy"` 引擎始终可用。

### 步骤 4：解析激活

在 `run.ts` 中，每次 agent run 开始时解析一次 context engine 并跨重试复用：

```typescript
ensureContextEnginesInitialized();
const contextEngine = await resolveContextEngine(params.config);
// contextEngine 在整个 run 生命周期中复用
```

## 生命周期集成

### Agent Run 主流程 (`src/agents/pi-embedded-runner/run.ts` + `run/attempt.ts`)

```
run() 开始
│
├─ ensureContextEnginesInitialized()
├─ resolveContextEngine(config)         // 解析一次
│
├─ try {
│   └─ while (重试循环) {
│       └─ runEmbeddedAttempt({
│           contextEngine,              // 传入 attempt
│           contextTokenBudget,
│           ...
│       })
│       │
│       │  attempt 内部:
│       │  ├─ 1. bootstrap (首次运行时)
│       │  │    contextEngine.bootstrap({ sessionId, sessionFile })
│       │  │
│       │  ├─ 2. 自动压缩守卫
│       │  │    applyPiAutoCompactionGuard({ contextEngineInfo })
│       │  │    // ownsCompaction=true → 禁用内置自动压缩
│       │  │
│       │  ├─ 3. assemble (发送模型请求前)
│       │  │    contextEngine.assemble({ sessionId, messages, tokenBudget })
│       │  │    // 可替换消息列表、追加 systemPromptAddition
│       │  │
│       │  └─ 4. afterTurn (attempt 完成后)
│       │       contextEngine.afterTurn({ sessionId, messages, ... })
│       │       // 或回退到 ingestBatch / ingest
│       │
│       ├─ 溢出压缩 (context overflow 时)
│       │    contextEngine.compact({ force: true, ... })
│       │
│       └─ 继续或返回结果
│   }
│
└─ finally {
    contextEngine.dispose()              // 清理资源
  }
```

### 关键集成点详解

**1. Bootstrap（attempt.ts:1095-1104）**

在 session 文件已存在时调用，让引擎从历史 JSONL 会话文件中导入消息到自己的存储：

```typescript
if (hadSessionFile && params.contextEngine?.bootstrap) {
  await params.contextEngine.bootstrap({
    sessionId: params.sessionId,
    sessionFile: params.sessionFile,
  });
}
```

**2. 自动压缩守卫（attempt.ts:1119-1122 + pi-settings.ts:101-122）**

当引擎声明 `ownsCompaction: true` 时，禁用 Pi SDK 内置的自动压缩，防止双重压缩：

```typescript
applyPiAutoCompactionGuard({
  settingsManager,
  contextEngineInfo: params.contextEngine?.info,
});
// ownsCompaction=true → settingsManager.setCompactionEnabled(false)
```

**3. Assemble（attempt.ts:1421-1444）**

在消息经过 sanitize → validate → limit 管道后，交给引擎做最终组装：

```typescript
const assembled = await params.contextEngine.assemble({
  sessionId, messages: activeSession.messages, tokenBudget,
});
if (assembled.messages !== activeSession.messages) {
  activeSession.agent.replaceMessages(assembled.messages);
}
if (assembled.systemPromptAddition) {
  systemPromptText = prependSystemPromptAddition({
    systemPrompt: systemPromptText,
    systemPromptAddition: assembled.systemPromptAddition,
  });
}
```

引擎可以：
- 重新排列/筛选消息
- 注入检索增强的上下文
- 向 system prompt 追加动态指令

**4. AfterTurn（attempt.ts:1884-1931）**

attempt 完成后，优先调用 `afterTurn`；如果引擎未实现该方法，则回退到逐条 `ingest` 或批量 `ingestBatch`：

```typescript
if (typeof contextEngine.afterTurn === "function") {
  await contextEngine.afterTurn({
    sessionId, sessionFile, messages, prePromptMessageCount,
    tokenBudget, runtimeContext,
  });
} else {
  // 回退：摄入新消息
  const newMessages = messages.slice(prePromptMessageCount);
  if (contextEngine.ingestBatch) {
    await contextEngine.ingestBatch({ sessionId, messages: newMessages });
  } else {
    for (const msg of newMessages) {
      await contextEngine.ingest({ sessionId, message: msg });
    }
  }
}
```

**5. 溢出压缩（run.ts:1027-1057）**

context overflow 时，通过引擎的 `compact` 方法处理，`runtimeContext` 携带完整的运行时状态：

```typescript
const compactResult = await contextEngine.compact({
  sessionId, sessionFile, tokenBudget,
  force: true, compactionTarget: "budget",
  runtimeContext: {
    sessionKey, messageChannel, provider, model,
    authProfileId, workspaceDir, config, ...
  },
});
```

**6. 子代理生命周期（subagent-registry.ts:314-334）**

子代理结束时通知 context engine 进行清理：

```typescript
async function notifyContextEngineSubagentEnded(params) {
  ensureContextEnginesInitialized();
  const engine = await resolveContextEngine(cfg);
  if (engine.onSubagentEnded) {
    await engine.onSubagentEnded({
      childSessionKey: params.childSessionKey,
      reason: params.reason,  // "deleted" | "completed" | "swept" | "released"
    });
  }
}
```

## 插件运行时能力

### Subagent API

插件可通过 `runtime.subagent` 管理子代理会话，无需直接访问 gateway dispatch：

```typescript
runtime.subagent.run({ sessionKey, message, deliver })
runtime.subagent.waitForRun({ runId, timeoutMs })
runtime.subagent.getSessionMessages({ sessionKey, limit })
runtime.subagent.deleteSession({ sessionKey })
```

底层通过 `AsyncLocalStorage` 请求作用域桥接（`src/plugins/runtime/gateway-request-scope.ts`），使用合成 operator client 内部分派到 `handleGatewayRequest`。非 WebSocket 路径（如 Telegram/WhatsApp 频道适配器）使用启动时缓存的 fallback gateway context。

### plugin-sdk 导出

`src/plugin-sdk/index.ts` 向外部消费者导出所有 context engine 类型：

```typescript
export type {
  ContextEngine, ContextEngineInfo,
  AssembleResult, CompactResult, IngestResult, IngestBatchResult,
  BootstrapResult, SubagentSpawnPreparation, SubagentEndReason,
} from "../context-engine/types.js";
export { registerContextEngine } from "../context-engine/registry.js";
export type { ContextEngineFactory } from "../context-engine/registry.js";
```

## 配置方式

在 `openclaw.yaml` 中激活自定义 context engine：

```yaml
plugins:
  slots:
    contextEngine: "my-context-engine"  # 引擎注册时使用的 id
```

不配置时默认使用 `"legacy"`，行为与引入 context engine 之前完全一致。

## 设计要点

1. **向后兼容**：LegacyContextEngine 作为所有生命周期方法的透传/no-op 实现，确保无插件时行为不变。
2. **排他性 slot**：同一时刻只有一个 context engine 激活，通过 `applyExclusiveSlotSelection` 自动禁用冲突插件。
3. **跨模块安全**：注册表使用 `Symbol.for` 全局单例，确保重复的 dist chunk 共享同一个 Map。
4. **优雅降级**：所有引擎调用都包裹在 try/catch 中，失败时 warn 日志并回退到默认行为。
5. **单次解析**：context engine 在每次 run 开始时解析一次，跨重试复用，避免重复初始化开销。
6. **防双重压缩**：`ownsCompaction` 标志让引擎声明自管压缩生命周期，自动禁用 Pi SDK 内置压缩。
