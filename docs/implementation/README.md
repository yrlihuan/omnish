# omnish 模块文档

本目录包含omnish项目各模块的详细说明文档。

## 模块列表

1. [omnish-common](./omnish-common.md) - 共享配置、工具函数和常量定义
2. [omnish-protocol](./omnish-protocol.md) - 客户端与守护进程间的通信协议定义
3. [omnish-transport](./omnish-transport.md) - RPC传输层，处理消息序列化和反序列化
4. [omnish-pty](./omnish-pty.md) - PTY（伪终端）处理，管理shell进程交互
5. [omnish-store](./omnish-store.md) - 数据存储层，提供键值存储和会话管理
6. [omnish-context](./omnish-context.md) - 上下文构建器，收集和构建LLM提示上下文
7. [omnish-llm](./omnish-llm.md) - LLM后端抽象，支持多种大语言模型提供商
8. [omnish-tracker](./omnish-tracker.md) - 命令跟踪器，监控和分析shell命令执行
9. [omnish-daemon](./omnish-daemon.md) - 守护进程，协调所有组件并提供RPC服务
10. [omnish-client](./omnish-client.md) - 客户端，处理用户交互和shell集成
11. [shell-prompt-state-tracking](./shell-prompt-state-tracking.md) - Shell提示状态跟踪机制说明

## 文档结构

每个模块文档包含以下部分：
- 模块概述
- 重要数据结构
- 关键函数说明
- 使用示例
- 依赖关系