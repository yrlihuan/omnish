# 实现文档索引

## omnish-common

共享配置与工具函数。

- **ClientConfig**：客户端配置，含 shell 设置、守护进程地址、新手引导状态、沙箱配置
- **DaemonConfig**：守护进程配置，含监听地址、代理设置（ProxyConfig）、LLM/上下文/定时任务/插件/沙箱/客户端推送等子配置
- **ShellConfig**：Shell 行为配置，含命令/前缀/拦截间隔/ghost-text 超时/开发者模式/补全开关/extended_unicode/language
- **ProxyConfig**：代理配置（http_proxy/no_proxy），支持旧版字符串格式向后兼容反序列化
- **ClientSection**：守护进程端客户端配置，通过 ConfigClient 消息推送到客户端
- **LlmConfig / LlmBackendConfig / LangfuseConfig**：LLM 后端选择、模型参数、API 密钥获取方式及 Langfuse 可观测性集成；均派生 PartialEq 用于热重载差异检测；LlmBackendConfig 含 per-backend use_proxy 和 context_window
- **ContextConfig / CompletionContextConfig**：补全上下文构建参数，含详细命令数、历史命令数、输出截断行数、弹性窗口范围
- **ConfigMap**：动态键值配置，维护 values + defaults 双层查询，序列化输出合并视图
- **TasksConfig**：类型别名 `HashMap<String, ConfigMap>`，每个任务以名称为键，默认值通过 set_defaults() 注入
- **PluginsConfig**：插件系统配置，指定启用的插件列表，插件通过 JSON-RPC 通信
- **ClientSandboxConfig**：客户端本地沙箱设置（enabled/backend/plugins），按主机配置
- **SandboxConfig / SandboxPluginConfig**：守护进程端沙箱豁免规则配置，按工具名配置 permit_rules
- **omnish_dir()**：获取 omnish 基础目录路径，优先级为 `$OMNISH_HOME` > `~/.omnish` > `/tmp/omnish`
- **load_client_config() / load_daemon_config()**：从配置文件或环境变量加载客户端/守护进程配置
- **config_edit 模块**：格式保留地原地更新 TOML 键值，支持嵌套键值、数组增删改、表删除
- **sandbox_rule 模块**：共享沙箱豁免规则工具函数（规则解析、匹配评估）
- **update 模块**：SHA-256 校验和、本地更新包缓存、版本字符串提取与 semver 比较、解压更新包并运行安装器
- **auth 模块**：认证令牌的路径获取、生成与加载，文件权限 0600
- **配置加载优先级**：三级加载——环境变量指定路径 > 默认路径 > 内置默认值

## omnish-protocol

客户端与守护进程之间的二进制通信协议，使用 bincode 序列化，帧以魔术字节 "OS" 验证，当前协议版本 v18，最低兼容版本 v14。

- **Message 枚举**：定义全部消息类型，涵盖会话生命周期、终端 I/O 转发、事件通知、LLM 请求/响应、命令补全、聊天会话、工具调用转发、认证、配置管理、客户端更新等
- **会话消息**：SessionStart/SessionEnd/SessionUpdate，携带会话 ID、时间戳、退出码及 Probe 采集的属性
- **I/O 与事件消息**：IoData 传输带方向的原始终端数据；Event 通知非零退出、模式匹配、命令边界等事件
- **LLM 请求/响应**：Request 携带查询与作用域；Response 支持流式与最终标记；CommandComplete 返回命令记录
- **命令补全**：CompletionRequest/Response 实现光标位置感知的自动补全；CompletionSummary 记录补全交互指标
- **聊天会话**：ChatStart/Ready/End/Message/Response/Interrupt 管理聊天生命周期与线程恢复；ChatToolStatus 流式推送工具执行状态
- **客户端侧工具调用**：ChatToolCall/ChatToolResult 实现守护进程到客户端的工具执行转发，支持 Landlock 沙箱标记；ChatToolResult 含 `needs_summarization` 字段请求 LLM 摘要
- **认证**：Auth 发送令牌与协议版本；AuthResult 统一成功/失败响应，版本不匹配时保持连接
- **配置管理**：ConfigQuery/Response/Update/UpdateResult 实现远程配置查询与修改；ConfigItem 支持 Toggle/Select/TextInput/Label/Data 类型及 prefills 预填充数据；ConfigClient 实现守护进程到客户端的配置推送
- **客户端更新**：UpdateCheck/UpdateInfo/UpdateRequest/UpdateChunk 实现版本检查与分块包下载
- **测试辅助**：TestDisconnect 消息用于测试客户端断线恢复
- **Frame 与序列化**：帧封装请求 ID 与消息负载；消息格式为 [魔术字节(2)][长度(4)][序列化消息]
- **协议版本管理**：PROTOCOL_VERSION + MIN_COMPATIBLE_VERSION 管理兼容范围（v4-v18），编译时守卫测试检测枚举变体变化和变体索引稳定性

## omnish-transport

RPC 传输层，处理 Unix socket 和 TCP 连接，提供客户端与守护进程之间的可靠通信。

- **重要数据结构**：`TransportAddr` 地址枚举、`RpcClient` 客户端结构（连接管理、请求 ID、ReplyTx 单响应/流响应模式、push_rx 推送接收）、`RpcServer` 服务器结构、`PushRegistry`/`OnPushConnect` 推送机制、TLS 支持、`Frame` 消息帧
- **地址解析 parse_addr()**：解析地址字符串为 TransportAddr，支持 Unix socket 路径、TCP 地址、显式协议前缀等多种格式
- **客户端连接 API**：connect/connect_unix/connect_tcp 显式协议连接、connect_with_reconnect 自动重连、call 单响应请求、send fire-and-forget、call_stream 多响应流式接收
- **服务器 API**：bind/bind_unix/bind_tcp 地址绑定与监听、serve 连接处理（支持认证令牌、TLS、PushRegistry 推送和 OnPushConnect 回调）
- **客户端内部机制**：读写分离、原子请求 ID、指数退避重连（支持 PermanentFailure 阈值终止）、ReplyTx 响应分发
- **服务器内部机制**：每连接独立异步任务、边接收边写入的流式转发、EMFILE/ENFILE fd 耗尽诊断
- **消息传输协议**：帧格式 `[u32长度][序列化数据]`、request_id 请求-响应匹配、多消息流式传输（Ack 结束标记）
- **多消息流式传输机制**：ReplyTx 枚举区分 Once/Stream 模式、mpsc 通道容量 128、背压机制
- **协议版本校验**：Auth 消息携带 protocol_version、versions_compatible() 兼容范围检查、帧反序列化失败时优雅跳过
- **重连机制与永久失败终止**：指数退避（1s~30s）、PermanentFailure 连续 5 次放弃重连
- **安全模型**：Unix socket 权限 0600+peer UID 验证、TCP TLS 自签名证书加密、5 秒认证超时

## omnish-pty

PTY（伪终端）处理，原始模式设置。

- **PtyProxy 数据结构**：PTY 代理核心结构，持有主端文件描述符和子进程 PID，负责伪终端创建、I/O 转发、子进程生命周期管理及终端窗口大小设置
- **RawModeGuard 数据结构**：RAII 风格的原始模式守卫，进入时禁用回显和规范模式，退出时自动恢复终端设置
- **spawn / spawn_with_env**：创建伪终端并 fork 子进程，设置控制终端、重定向标准 I/O、注入自定义环境变量后 exec 目标命令
- **read / write_all**：PTY 主端的数据读写
- **set_window_size**：通过 ioctl TIOCSWINSZ 通知子进程终端尺寸变化
- **wait**：使用 waitpid 等待子进程终止并获取退出状态码
- **from_raw_fd**：从已有文件描述符和 PID 重建 PTY 代理（unsafe），用于 `/update` 自重启场景下跨 exec 边界恢复 PTY
- **respawn**：终止当前子进程并创建全新 PTY 重新启动 shell，支持 `/lock on/off` 切换 Landlock 沙箱
- **RawModeGuard::enter**：保存当前终端设置后通过 cfmakeraw 配置原始模式，返回守卫对象
- **设计模式**：RAII 模式保障资源安全；代理模式封装 PTY 底层操作
- **平台支持**：Linux 全功能支持；macOS 基本支持（TIOCSCTTY 已适配）

## omnish-store

数据存储，命令记录、流存储和补全采样。

- **CommandRecord**：命令记录的持久化结构，包含命令 ID、会话 ID、命令行、工作目录、时间戳、输出摘要、流偏移/长度、退出码
- **SessionMeta**：会话元数据管理，记录会话 ID、父会话关系、起止时间和自定义属性
- **StreamWriter / StreamEntry**：原始 I/O 流的二进制存储，按 `timestamp+direction+length+data` 紧凑格式写入
- **流读取函数**：`read_range()` 按偏移量精确读取指定范围流条目，`read_entries()` 读取全部条目
- **PendingSample / CompletionSample**：补全采样系统，缓冲待处理样本并关联下一条命令，最终写入 JSONL 文件
- **levenshtein / similarity**：编辑距离与归一化相似度计算，用于评估补全建议与用户实际命令的匹配质量
- **spawn_sample_writer**：后台异步样本写入线程，通过 mpsc channel 接收样本
- **SessionUpdateRecord**：会话状态快照记录，定期保存状态变化，写入 CSV 文件
- **CompletionRecord**：补全请求完整记录，包含序列号、延迟、停留时间等指标
- **文件结构**：存储目录布局，含 `commands.json`、`meta.json`、`stream.bin`、会话更新 CSV 和按日期轮转的采样 JSONL

## omnish-context

上下文构建，命令选择和格式化。

- **CommandContext 数据结构**：预处理的命令数据，包含会话 ID、主机名、命令行、工作目录、时间戳、输出和退出码
- **核心 trait 接口**：`StreamReader`（读取命令输出流）、`ContextStrategy`（选择要包含的命令）、`ContextFormatter`（将命令格式化为上下文字符串，区分 history 仅命令行和 detailed 含完整输出）
- **RecentCommands 策略**：选择最近 N 条命令的策略实现，支持设置当前会话最小命令数保障
- **GroupedFormatter**：按会话分组的格式化器，当前会话命令置于末尾
- **InterleavedFormatter**：按时间顺序交错排列所有会话命令的格式化器
- **CompletionFormatter**：补全场景专用格式化器，通过冻结 history 区 + 追加式 recent 区优化 KV 缓存命中率；支持 `live_cwd` 解决 DEBUG trap 记录旧路径问题
- **build_context / build_context_with_session**：构建 LLM 上下文的主函数，协调策略选择命令、读取流数据、格式化器生成文本
- **select_and_split**：策略选择命令并分割为 history/detailed 的单一入口
- **格式化工具函数**：相对时间格式化、会话终端标签分配（双射 base-26 编码）、行数+字符数双重截断
- **ANSI 清理与输出预处理**：去除 ANSI 转义序列、缩写 home 目录路径、跳过 PTY 首行回显

## omnish-llm

LLM 后端抽象和实现，支持多种 LLM 提供商，提供统一的补全、工具调用、可观测性接口。

- **工具调用类型（ToolDef/ToolCall/ToolResult）**：定义工具的名称/描述/输入 schema，表示 LLM 发起的工具调用请求及其执行结果
- **LlmBackend trait 与核心请求/响应类型**：统一的 LLM 后端接口（含 model_name），LlmRequest/LlmResponse 核心类型，UseCase 路由；UnavailableBackend 作为未配置时回退
- **PromptManager（系统提示词管理）**：可组合的具名片段管理器，支持从 JSON 加载、同名覆盖合并；用户可通过 chat.override.json 和 tool.override.json 覆盖
- **模型预设（presets）**：编译时嵌入的提供商元数据 JSON，供配置菜单和安装脚本使用
- **AnthropicBackend**：Anthropic Messages API 后端，支持多轮对话、思考模式（签名保留）、工具调用、提示缓存（3 个 cache_control 断点）、自动重试
- **OpenAiCompatBackend**：OpenAI 兼容 API 后端，`<think>` 标签解析、Anthropic 格式 extra_messages 转换
- **MultiBackend（多后端路由）**：根据 UseCase 路由请求；SharedLlmBackend 类型别名支持热重载；from_single() 便于测试
- **LangfuseBackend（可观测性）**：装饰器模式包装任意后端，异步发送 trace/generation 事件到 Langfuse
- **请求日志（message_log）**：仅记录 Chat 类型请求的完整 JSON payload，滚动保留最近 30 个文件
- **工厂函数**：根据配置创建后端实例；per-backend use_proxy 控制代理；effective_max_content_chars 推导
- **提示模板（template）**：build_user_content 构建用户提示，build_simple_completion_content 构建补全提示（KV cache 前缀稳定性设计），定期总结和线程摘要提示模板（支持多语言指令）
- **配置结构**：后端配置（类型/模型/API 密钥命令/base_url/use_proxy/context_window），Langfuse 配置，全局 proxy/no_proxy

## omnish-tracker

命令跟踪，shell 提示检测，OSC 133 检测。

- **CommandTracker**：命令生命周期管理器，维护待处理命令状态，通过 `feed_input`/`feed_output`/`feed_osc133` 三个入口接收数据，检测命令边界并生成 `CommandRecord`；seq 编号仅在命令真正完成时分配
- **Osc133Detector**：OSC 133 终端控制序列的字节级状态机解析器，支持跨数据块解析；识别 A/B/C/D/RL/NO_READLINE 六类事件
- **PromptDetector**：基于正则表达式的 shell 提示符检测器，默认匹配 `$#%❯` 结尾行，支持自定义模式
- **命令行解析优先级**：`finalize_command` 按 osc_original_input > osc_command_line > extract_command_line 三级回退确定最终命令文本
- **CWD 跟踪**：优先使用运行时 CWD（OSC 133 CommandStart 或 `/proc/{pid}/cwd` 探针），回退到会话级 CWD
- **OSC 133;B 扩展格式**：payload 以未转义分号分隔字段，命令内分号转义为 `\;`，支持 `cwd:` 和 `orig:` 可选前缀
- **双模式检测**：正则表达式模式与 OSC 133 模式互斥运行
- **错误恢复**：自动丢弃无效转义序列；PromptStart 丢失时自动创建恢复性 pending 防止命令丢失

## omnish-client

终端客户端，提供交互式 shell 包装和 LLM 集成界面。

- **InputInterceptor 输入拦截器**：检测命令前缀进入聊天模式，支持双前缀恢复对话、ESC 序列过滤、UTF-8 退格、前缀超时计时
- **ShellCompleter 命令补全**：LLM 驱动的 shell 命令幽灵文本建议，防抖、isearch 过滤、过时建议丢弃、并发请求管理
- **ShellInputTracker 输入跟踪**：通过 OSC 133 状态和转发字节跟踪 shell 命令行内容、光标位置、readline 报告、isearch 模式
- **CursorColTracker / DsrDetector 光标跟踪**：终端光标行列位置跟踪，DSR 响应检测用于 InlineNotice 渲染模式选择
- **AltScreenDetector 全屏检测**：检测 vim/less 等交替屏幕程序切换，抑制通知和拦截
- **ChatAction / OutputLimit 命令解析**：聊天动作分类（本地命令/LLM 查询/守护进程查询），管道限制支持
- **Widgets 系统**：交互式 UI 组件集，包含 LineEditor、LineStatus、InlineNotice、ScrollView、ChatLayout、Picker、Menu（含 MenuChangeHandler 即时变更回调、失败自动回滚、Select prefills 预填充）、TextView、Common
- **Markdown 渲染**：pulldown-cmark 解析，标题/粗体/代码块/列表/引用/链接/表格等 ANSI 终端样式输出
- **粘贴支持**：括号粘贴模式、快速粘贴检测、多行折叠显示
- **客户端插件系统**：ClientPluginManager 通过子进程执行工具，统一多后端沙箱（bwrap/landlock/seatbelt）、运行时可用性检测、`/test lock on/off` 命令、JSON 协议；execute_tool 返回 `(content, is_error, needs_summarization)` 三元组
- **自更新系统**：`/update` 透明自重启（execvp 恢复 PTY/session）、mtime 自动检测、协议级 UpdateCheck 轮询+后台下载+缓存机制
- **多轮聊天模式**：ChatSession 驱动的多轮对话循环，线程懒创建、双前缀快速恢复、线程绑定与多会话保护、空闲自动关闭、ChatLayout 统一渲染、Ctrl-C 中断、聊天历史持久化、`/thread` 命令族（stats/sandbox/list）
- **Probe 系统**：可插拔数据收集器，静态 Probe 和动态 Probe，平台信息来自客户端上报
- **主事件循环**：poll I/O 多路复用，stdin/PTY master 监控，DSR 过滤，前缀匹配计时，OSC 133 命令跟踪
- **Polling 机制**：渐进式间隔（1-60s）后台探测任务，差异更新 SessionUpdate，tmux 窗口标题自动更新
- **事件日志**：全局环形缓冲区（200 条），记录 OSC 转换/补全/聊天/更新/连接/延迟等事件
- **守护进程通信**：connect_daemon 连接/认证/协议版本检查，send_or_buffer 失败缓冲（10000 条上限）
- **显示函数**：纯函数 ANSI 输出，分隔线/提示符/输入回显/响应渲染/幽灵文本/CJK 感知截断
- **命令分发**：统一命令注册表，Local/Daemon 命令类型，重定向/管道解析
- **Agent 工具调用循环**：自动工具调用/并行执行/结果反馈，ChatToolCall/ChatToolStatus/ChatToolResult 协议，redraw_tool_section 原地更新状态
- **OSC 133 与 Shell Hook**：Bash 与 Zsh 双 shell 支持，命令/CWD/readline 实时跟踪
- **i18n 多语言**：编译期内嵌翻译系统，客户端默认 en，由守护进程按系统语言推送覆盖
- **架构设计**：同步 poll I/O、TimeGapGuard 拦截策略、聊天模式两层架构（入口层+聊天层）

## omnish-daemon

守护进程主服务，管理会话、LLM、插件和定时任务。

- **DaemonServer**：守护进程主服务结构，持有 SharedLlmBackend（支持热重载）、会话/对话/插件/工具/格式化管理器等，提供 RPC 服务接口
- **AgentLoopState**：智能体循环状态，含 saved_up_to 增量持久化索引、用量追踪、cancel_flag 守护进程侧取消
- **SessionManager**：会话生命周期管理（注册、结束、驱逐），I/O 数据流写入，命令记录存储，补全上下文构建（弹性窗口+KV cache 预热），补全采样，后台 JSONL 写入线程
- **ConversationManager**：多轮聊天线程管理（创建、存储、加载、删除），JSONL 文件+内存双写，线程元数据（含用量统计、system_reminder 变更检测、sandbox_disabled per-thread 沙箱覆盖）
- **PluginManager**：元数据驱动的插件系统，从 tool.json 加载工具定义，DaemonTool/ClientTool 双类型分发，tool.override.json 描述覆盖与热重载（inotify/轮询），内嵌资源自动安装
- **ToolRegistry**：统一工具元数据注册表，启动时填充后以 Arc 共享只读，支持运行时描述覆盖和热重载原子更新
- **插件系统与内置工具**：元数据+子进程分离架构，内置工具由客户端 omnish-plugin 执行，CommandQueryTool 在守护进程内执行，Landlock 沙箱，PROMPT.MD 支持
- **FormatterManager**：工具结果格式化注册表，内置格式化器+ 外部格式化器子进程
- **PromptManager**：可组合系统提示词片段管理，基础 chat.json + 用户 chat.override.json 覆盖/追加合并
- **system-reminder**：环境上下文附加到系统提示词（非用户消息），ThreadMeta 记录上次 system-reminder 用于变更检测
- **智能体循环（Agent Loop）**：多轮工具调用循环，DaemonTool 直接执行+ClientTool 暂停/恢复转发，persist_unsaved() 增量持久化（早退路径消毒孤立 tool_use），超时与断开时保存进度，thinking 标签处理
- **聊天消息流程**：ChatStart 创建/恢复线程→ChatMessage→工具转发→ChatResponse→ChatInterrupt 中断处理
- **配置管理**：ConfigSchema 基于 config_schema.toml 的 TUI 配置菜单构建器，即时保存，api_key 自动转换为 api_key_cmd，支持动态占位符和客户端侧占位符展开，后端编辑/删除，沙箱规则增删改
- **UpdateCache 与客户端更新**：更新包缓存管理器（多平台包缓存、版本比较、传输锁），UpdateCheck 版本检查，UpdateRequest 流式包分发
- **SandboxRules**：沙箱许可规则模块，白名单规则
- **FileWatcher 与 ConfigWatcher**：共享文件监视基础设施，ConfigWatcher 分节发布/订阅机制，支持 LLM 后端热重载
- **TaskManager 与定时任务**：基于 tokio-cron-scheduler 的集中式任务管理器，内置任务：eviction、hourly_summary、daily_notes（基于 hourly summaries 汇总）、disk_cleanup、thread_summary、auto_update；均使用 SharedLlmBackend
- **补全采样**：pending sample 捕获→accepted 标志更新→条件写入，JSONL 持久化
- **update_thread_usage() / format_thread_stats()**：线程用量持久化（last_response + cumulative 双参数），`/thread stats` 显示
- **并发与锁设计**：RwLock 分层（sessions/meta/commands 读多写少），Mutex 独占，两阶段驱逐和快照式清理避免锁争用
- **数据持久化**：会话目录（meta.json/commands.json/stream.bin），线程文件（JSONL+.meta.json），日志目录（轮转）

---

如需了解模块的具体实现，可使用 `split_doc_sections.sh` 获取模块文档的段落行号范围，再用 Read tool 的 offset/limit 读取相关段落（请谨慎使用，模块文档较大，会消耗大量 token）：
```bash
bash docs/implementation/split_doc_sections.sh docs/implementation/<module>.md
```
