# 实现文档索引

## omnish-common

共享配置与工具函数。

- **ClientConfig** (L11-L18)：客户端配置，含 shell 设置、守护进程地址、自动补全开关、新手引导状态
- **DaemonConfig**：守护进程配置，含监听地址、代理设置、LLM/上下文/定时任务/插件/工具注入/沙箱等子配置
- **ShellConfig**：Shell 行为配置，含命令/前缀/拦截间隔/ghost-text 超时/开发者模式
- **LlmConfig / LlmBackendConfig / LangfuseConfig**：LLM 后端选择、模型参数、API 密钥获取方式及 Langfuse 可观测性集成
- **ContextConfig / CompletionContextConfig**：补全上下文构建参数，含详细命令数、历史命令数、输出截断行数、弹性窗口范围
- **TasksConfig / PeriodicSummaryConfig / AutoUpdateConfig**：定时任务配置，含会话淘汰、日报生成、周期性摘要、磁盘清理、自动更新计划与客户端分发
- **PluginsConfig**：插件系统配置，指定启用的插件列表，插件通过 JSON-RPC 通信
- **SandboxConfig / SandboxPluginConfig**：沙箱豁免规则，按工具名配置 permit_rules 以跳过 Landlock 沙箱
- **omnish_dir()** (L115-L139)：获取 omnish 基础目录路径，优先级为 `$OMNISH_HOME` > `~/.omnish` > `/tmp/omnish`
- **load_client_config() / load_daemon_config()**：从配置文件或环境变量加载客户端/守护进程配置
- **config_edit 模块** (L141-L181)：格式保留地原地更新 TOML 键值，支持点分隔路径的嵌套键值更新
- **update 模块** (L274-L318)：SHA-256 校验和、本地更新包缓存、版本字符串提取与 semver 比较、解压更新包并运行安装器
- **auth 模块** (L320-L345)：认证令牌的路径获取、生成与加载，文件权限 0600
- **配置加载优先级** (L361-L384)：三级加载——环境变量指定路径 > 默认路径 > 内置默认值

## omnish-protocol

客户端与守护进程之间的二进制通信协议，使用 bincode 序列化，帧以魔术字节 "OS" 验证，当前协议版本 v11。

- **Message 枚举** (L11-L43)：定义全部消息类型，涵盖会话生命周期、终端 I/O 转发、事件通知、LLM 请求/响应、命令补全、聊天会话、工具调用转发、认证、配置管理、客户端更新等
- **会话消息** (L45-L72)：SessionStart/SessionEnd/SessionUpdate，携带会话 ID、时间戳、退出码及 Probe 采集的属性
- **I/O 与事件消息** (L74-L96)：IoData 传输带方向的原始终端数据；Event 通知非零退出、模式匹配、命令边界等事件
- **LLM 请求/响应** (L98-L121)：Request 携带查询与作用域；Response 支持流式与最终标记；CommandComplete 返回命令记录
- **命令补全** (L123-L152)：CompletionRequest/Response 实现光标位置感知的自动补全；CompletionSummary 记录补全交互指标
- **聊天会话** (L153-L231)：ChatStart/Ready/End/Message/Response/Interrupt 管理聊天生命周期与线程恢复；ChatToolStatus 流式推送工具执行状态
- **客户端侧工具调用** (L233-L251)：ChatToolCall/ChatToolResult 实现守护进程到客户端的工具执行转发，支持 Landlock 沙箱标记
- **认证** (L253-L264)：Auth 发送令牌与协议版本；AuthResult 统一成功/失败响应，版本不匹配时保持连接
- **配置管理** (L266-L304)：ConfigQuery/Response/Update/UpdateResult 实现远程配置查询与修改；ConfigItem 支持 Toggle/Select/TextInput 类型
- **客户端更新** (L306-L333)：UpdateCheck/UpdateInfo/UpdateRequest/UpdateChunk 实现版本检查与分块包下载
- **Frame 与序列化** (L335-L371)：帧封装请求 ID 与消息负载；消息格式为 [魔术字节(2)][长度(4)][序列化消息]
- **协议版本管理** (L430-L444)：PROTOCOL_VERSION 常量管理版本演进（v4-v11），编译时守卫测试检测枚举变体变化

## omnish-transport

RPC 传输层，处理 Unix socket 和 TCP 连接，提供客户端与守护进程之间的可靠通信。

- **重要数据结构** (L22-L80)：`TransportAddr` 地址枚举、`RpcClient` 客户端结构（连接管理、请求 ID、ReplyTx 单响应/流响应模式）、`RpcServer` 服务器结构、TLS 支持、`Frame` 消息帧
- **地址解析 parse_addr()** (L84-L93)：解析地址字符串为 TransportAddr，支持 Unix socket 路径、TCP 地址、显式协议前缀等多种格式
- **客户端连接 API** (L94-L174)：connect/connect_unix/connect_tcp 显式协议连接、connect_with_reconnect 自动重连、call 单响应请求、send fire-and-forget、call_stream 多响应流式接收
- **服务器 API** (L176-L215)：bind/bind_unix/bind_tcp 地址绑定与监听、serve 连接处理（支持认证令牌和 TLS，处理器通过 mpsc::Sender 实现流式传输）
- **客户端内部机制** (L363-L373)：读写分离、原子请求 ID、指数退避重连（支持 PermanentFailure 阈值终止）、ReplyTx 响应分发
- **服务器内部机制** (L375-L382)：每连接独立异步任务、边接收边写入的流式转发、EMFILE/ENFILE fd 耗尽诊断
- **消息传输协议** (L384-L393)：帧格式 `[u32长度][序列化数据]`、request_id 请求-响应匹配、多消息流式传输（Ack 结束标记）
- **多消息流式传输机制** (L416-L457)：ReplyTx 枚举区分 Once/Stream 模式、mpsc 通道容量 128、背压机制
- **协议版本校验** (L459-L490)：Auth 消息携带 protocol_version、版本不匹配返回 ok=false 但保持连接
- **重连机制与永久失败终止** (L492-L515)：指数退避（1s~30s）、PermanentFailure 连续 5 次放弃重连
- **安全模型** (L538-L556)：Unix socket 权限 0600+peer UID 验证、TCP TLS 自签名证书加密、5 秒认证超时

## omnish-pty

PTY（伪终端）处理，原始模式设置。

- **PtyProxy 数据结构** (L11-L20)：PTY 代理核心结构，持有主端文件描述符和子进程 PID，负责伪终端创建、I/O 转发、子进程生命周期管理及终端窗口大小设置
- **RawModeGuard 数据结构** (L22-L30)：RAII 风格的原始模式守卫，进入时禁用回显和规范模式，退出时自动恢复终端设置
- **spawn / spawn_with_env** (L34-L53)：创建伪终端并 fork 子进程，设置控制终端、重定向标准 I/O、注入自定义环境变量后 exec 目标命令
- **read / write_all** (L54-L68)：PTY 主端的数据读写
- **set_window_size** (L70-L76)：通过 ioctl TIOCSWINSZ 通知子进程终端尺寸变化
- **wait** (L78-L84)：使用 waitpid 等待子进程终止并获取退出状态码
- **from_raw_fd** (L86-L93)：从已有文件描述符和 PID 重建 PTY 代理（unsafe），用于 `/update` 自重启场景下跨 exec 边界恢复 PTY
- **respawn** (L95-L117)：终止当前子进程并创建全新 PTY 重新启动 shell，支持 `/lock on/off` 切换 Landlock 沙箱
- **RawModeGuard::enter** (L119-L129)：保存当前终端设置后通过 cfmakeraw 配置原始模式，返回守卫对象
- **设计模式** (L165-L178)：RAII 模式保障资源安全；代理模式封装 PTY 底层操作
- **平台支持** (L191-L202)：Linux 全功能支持；macOS 基本支持（TIOCSCTTY 已适配）

## omnish-store

数据存储，命令记录、流存储和补全采样。

- **CommandRecord** (L11-L27)：命令记录的持久化结构，包含命令 ID、会话 ID、命令行、工作目录、时间戳、输出摘要、流偏移/长度、退出码
- **SessionMeta** (L29-L40)：会话元数据管理，记录会话 ID、父会话关系、起止时间和自定义属性
- **StreamWriter / StreamEntry** (L42-L61)：原始 I/O 流的二进制存储，按 `timestamp+direction+length+data` 紧凑格式写入
- **流读取函数** (L122-L134)：`read_range()` 按偏移量精确读取指定范围流条目，`read_entries()` 读取全部条目
- **PendingSample / CompletionSample** (L136-L164)：补全采样系统，缓冲待处理样本并关联下一条命令，最终写入 JSONL 文件
- **levenshtein / similarity** (L166-L178)：编辑距离与归一化相似度计算，用于评估补全建议与用户实际命令的匹配质量
- **spawn_sample_writer** (L180-L185)：后台异步样本写入线程，通过 mpsc channel 接收样本
- **SessionUpdateRecord** (L267-L281)：会话状态快照记录，定期保存状态变化，写入 CSV 文件
- **CompletionRecord** (L283-L301)：补全请求完整记录，包含序列号、延迟、停留时间等指标
- **文件结构** (L303-L329)：存储目录布局，含 `commands.json`、`meta.json`、`stream.bin`、会话更新 CSV 和按日期轮转的采样 JSONL

## omnish-context

上下文构建，命令选择和格式化。

- **CommandContext 数据结构** (L11-L25)：预处理的命令数据，包含会话 ID、主机名、命令行、工作目录、时间戳、输出和退出码
- **核心 trait 接口** (L27-L49)：`StreamReader`（读取命令输出流）、`ContextStrategy`（选择要包含的命令）、`ContextFormatter`（将命令格式化为上下文字符串，区分 history 仅命令行和 detailed 含完整输出）
- **RecentCommands 策略** (L54-L62, L191-L204)：选择最近 N 条命令的策略实现，支持设置当前会话最小命令数保障
- **GroupedFormatter** (L64-L73, L206-L218)：按会话分组的格式化器，当前会话命令置于末尾
- **InterleavedFormatter** (L75-L84, L219-L231)：按时间顺序交错排列所有会话命令的格式化器
- **CompletionFormatter** (L86-L245)：补全场景专用格式化器，通过冻结 history 区 + 追加式 recent 区优化 KV 缓存命中率；支持 `live_cwd` 解决 DEBUG trap 记录旧路径问题
- **build_context / build_context_with_session** (L115-L155)：构建 LLM 上下文的主函数，协调策略选择命令、读取流数据、格式化器生成文本
- **select_and_split** (L157-L167)：策略选择命令并分割为 history/detailed 的单一入口
- **格式化工具函数** (L247-L299)：相对时间格式化、会话终端标签分配（双射 base-26 编码）、行数+字符数双重截断
- **ANSI 清理与输出预处理** (L176-L189)：去除 ANSI 转义序列、缩写 home 目录路径、跳过 PTY 首行回显

## omnish-llm

LLM 后端抽象和实现，支持多种 LLM 提供商，提供统一的补全、工具调用、可观测性接口。

- **工具调用类型（ToolDef/ToolCall/ToolResult）** (L11-L31)：定义工具的名称/描述/输入 schema，表示 LLM 发起的工具调用请求及其执行结果
- **LlmBackend trait 与核心请求/响应类型** (L33-L106)：统一的 LLM 后端接口，LlmRequest 封装上下文/查询/对话历史/工具定义/思考模式，LlmResponse 封装内容块（Text/ToolUse/Thinking）/停止原因/用量统计，UseCase 区分补全/分析/对话用途
- **PromptManager（系统提示词管理）** (L107-L133)：可组合的具名片段管理器，支持从 JSON 加载、同名覆盖合并；用户可通过 chat.override.json 和 tool.override.json 覆盖
- **AnthropicBackend** (L137-L160)：Anthropic Messages API 后端，支持多轮对话、思考模式（签名保留）、工具调用、提示缓存（3 个 cache_control 断点）、自动重试
- **OpenAiCompatBackend** (L154-L168)：OpenAI 兼容 API 后端，支持 OpenAI/Azure/本地 API（vLLM），`<think>` 标签解析、Anthropic 格式 extra_messages 转换
- **MultiBackend（多后端路由）** (L170-L177)：根据 UseCase 将请求路由到不同后端实例，支持按名称获取后端，初始化时容忍单个后端失败并回退默认
- **LangfuseBackend（可观测性）** (L179-L187)：装饰器模式包装任意后端，异步发送 trace/generation 事件到 Langfuse
- **请求日志（message_log）** (L189-L195)：仅记录 Chat 类型请求的完整 JSON payload，滚动保留最近 30 个文件
- **工厂函数** (L224-L245)：根据配置类型创建对应后端实例，支持全局 proxy/no_proxy 配置传递
- **提示模板（template）** (L254-L302)：build_user_content 构建用户提示，build_simple_completion_content 构建补全提示（KV cache 前缀稳定性设计），定期总结和线程摘要提示模板
- **配置结构** (L197-L213)：后端配置（类型/模型/API 密钥命令/base_url/最大字符数），Langfuse 配置，全局 proxy/no_proxy

## omnish-tracker

命令跟踪，shell 提示检测，OSC 133 检测。

- **CommandTracker** (L16-L36)：命令生命周期管理器，维护待处理命令状态，通过 `feed_input`/`feed_output`/`feed_osc133` 三个入口接收数据，检测命令边界并生成 `CommandRecord`；seq 编号仅在命令真正完成时分配
- **Osc133Detector** (L38-L57)：OSC 133 终端控制序列的字节级状态机解析器，支持跨数据块解析；识别 A/B/C/D/RL/NO_READLINE 六类事件
- **PromptDetector** (L59-L69)：基于正则表达式的 shell 提示符检测器，默认匹配 `$#%❯` 结尾行，支持自定义模式
- **命令行解析优先级** (L109-L117)：`finalize_command` 按 osc_original_input > osc_command_line > extract_command_line 三级回退确定最终命令文本
- **CWD 跟踪** (L234-L245)：优先使用运行时 CWD（OSC 133 CommandStart 或 `/proc/{pid}/cwd` 探针），回退到会话级 CWD
- **OSC 133;B 扩展格式** (L247-L273)：payload 以未转义分号分隔字段，命令内分号转义为 `\;`，支持 `cwd:` 和 `orig:` 可选前缀
- **双模式检测** (L281-L283)：正则表达式模式与 OSC 133 模式互斥运行
- **错误恢复** (L294-L297)：自动丢弃无效转义序列；PromptStart 丢失时自动创建恢复性 pending 防止命令丢失

## omnish-client

终端客户端，提供交互式 shell 包装和 LLM 集成界面。

- **InputInterceptor 输入拦截器** (L24-L67)：检测命令前缀进入聊天模式，支持双前缀恢复对话、ESC 序列过滤、UTF-8 退格、前缀超时计时
- **ShellCompleter 命令补全** (L96-L131)：LLM 驱动的 shell 命令幽灵文本建议，防抖、isearch 过滤、过时建议丢弃、并发请求管理
- **ShellInputTracker 输入跟踪** (L133-L170)：通过 OSC 133 状态和转发字节跟踪 shell 命令行内容、光标位置、readline 报告、isearch 模式
- **CursorColTracker / DsrDetector 光标跟踪** (L172-L206)：终端光标行列位置跟踪，DSR 响应检测用于 InlineNotice 渲染模式选择
- **AltScreenDetector 全屏检测** (L208-L213)：检测 vim/less 等交替屏幕程序切换，抑制通知和拦截
- **ChatAction / OutputLimit 命令解析** (L215-L228)：聊天动作分类（本地命令/LLM 查询/守护进程查询），管道限制支持
- **Widgets 系统** (L230-L640)：交互式 UI 组件集，包含 LineEditor、LineStatus、InlineNotice、ScrollView、ChatLayout、Picker、Menu（含 Button 类型）、TextView、Common
- **Markdown 渲染** (L637-L660)：pulldown-cmark 解析，标题/粗体/代码块/列表/引用/链接/表格等 ANSI 终端样式输出
- **粘贴支持** (L662-L684)：括号粘贴模式、快速粘贴检测、多行折叠显示
- **客户端插件系统** (L686-L730)：ClientPluginManager 通过子进程执行工具，Landlock 沙箱、`/lock on/off` 命令、JSON 协议
- **自更新系统** (L736-L807)：`/update` 透明自重启（execvp 恢复 PTY/session）、mtime 自动检测、协议级 UpdateCheck 轮询+后台下载+缓存机制
- **多轮聊天模式** (L809-L1115)：ChatSession 驱动的多轮对话循环，线程懒创建、双前缀快速恢复、线程绑定与多会话保护、空闲自动关闭、ChatLayout 统一渲染、Ctrl-C 中断、聊天历史持久化、`/thread stats` 用量统计
- **Probe 系统** (L1117-L1172)：可插拔数据收集器，静态 Probe 和动态 Probe，平台信息来自客户端上报
- **主事件循环** (L1181-L1203)：poll I/O 多路复用，stdin/PTY master 监控，DSR 过滤，前缀匹配计时，OSC 133 命令跟踪
- **Polling 机制** (L1233-L1261)：渐进式间隔（1-60s）后台探测任务，差异更新 SessionUpdate，tmux 窗口标题自动更新
- **事件日志** (L1264-L1284)：全局环形缓冲区（200 条），记录 OSC 转换/补全/聊天/更新/连接/延迟等事件
- **守护进程通信** (L1286-L1334)：connect_daemon 连接/认证/协议版本检查，send_or_buffer 失败缓冲（10000 条上限）
- **显示函数** (L1352-L1378)：纯函数 ANSI 输出，分隔线/提示符/输入回显/响应渲染/幽灵文本/CJK 感知截断
- **命令分发** (L1380-L1421)：统一命令注册表，Local/Daemon 命令类型，重定向/管道解析
- **Agent 工具调用循环** (L1423-L1468)：自动工具调用/并行执行/结果反馈，ChatToolCall/ChatToolStatus/ChatToolResult 协议，redraw_tool_section 原地更新状态
- **OSC 133 与 Shell Hook** (L1635-L1655)：Bash PROMPT_COMMAND/DEBUG trap 集成，命令/CWD/readline 实时跟踪
- **架构设计** (L1598-L1693)：同步 poll I/O、TimeGapGuard 拦截策略、聊天模式两层架构（入口层+聊天层）

## omnish-daemon

守护进程主服务，管理会话、LLM、插件和定时任务。

- **DaemonServer** (L31-L44)：守护进程主服务结构，持有会话管理器、LLM 后端、定时任务管理器、对话管理器、插件管理器、工具注册表、格式化管理器、更新缓存等核心组件，提供 RPC 服务接口（Unix domain socket，最多 30 工作线程）
- **AgentLoopState** (L46-L63)：智能体循环状态，含 cumulative_usage/last_response_usage 用量追踪、last_model 模型名
- **SessionManager** (L64-L79)：会话生命周期管理（注册、结束、驱逐），I/O 数据流写入，命令记录存储，补全上下文构建（弹性窗口+KV cache 预热），补全采样（pending sample 捕获与 flush），后台 JSONL 写入线程
- **ConversationManager** (L149-L184)：多轮聊天线程管理（创建、存储、加载、删除），JSONL 文件+内存双写，线程元数据（含 ThreadUsage 用量统计），线程恢复 UX 增强
- **PluginManager** (L80-L116)：元数据驱动的插件系统，从 tool.json 加载工具定义，DaemonTool/ClientTool 双类型分发，tool.override.json 描述覆盖与热重载（inotify/轮询），内嵌资源自动安装
- **ToolRegistry** (L117-L143)：统一工具元数据注册表（display_name/formatter/status_template/plugin_type），启动时填充后以 Arc 共享只读，支持运行时描述覆盖和热重载原子更新
- **插件系统与内置工具** (L268-L389)：元数据+子进程分离架构，内置工具（bash/read/edit/write/glob/grep）由客户端 omnish-plugin 执行，CommandQueryTool 在守护进程内执行，Landlock 沙箱，PROMPT.MD 支持
- **FormatterManager** (L390-L471)：工具结果格式化注册表，内置格式化器（default/read/edit）+ 外部格式化器子进程，格式化器选择顺序（external > builtin > default）
- **PromptManager** (L472-L494)：可组合系统提示词片段管理，基础 chat.json + 用户 chat.override.json 覆盖/追加合并
- **system-reminder** (L495-L526)：每条聊天消息自动附加环境上下文标签（日期、工作目录、Git 状态、平台信息、最近 5 条命令）
- **智能体循环（Agent Loop）** (L575-L670)：多轮工具调用循环，DaemonTool 直接执行+ClientTool 暂停/恢复转发，累计用量追踪，API 错误处理（重试+对话保留+`<event>api error</event>` 标记），Cancel 标志
- **聊天消息流程** (L763-L790)：ChatStart 创建/恢复线程→ChatMessage→工具转发→ChatResponse→ChatInterrupt 中断处理
- **配置管理** (L791-L837)：ConfigSchema 基于 config_schema.toml 的 TUI 配置菜单构建器，ConfigQuery/ConfigUpdate 消息处理，动态后端选项生成
- **UpdateCache 与客户端更新** (L229-L255, L839-L852)：更新包缓存管理器（多平台包缓存、版本比较、传输锁），UpdateCheck 版本检查，UpdateRequest 流式包分发
- **SandboxRules** (L243-L267)：沙箱许可规则模块，支持 starts_with/contains/equals/matches 操作符的白名单规则
- **FileWatcher 与 ConfigWatcher** (L187-L215)：共享文件监视基础设施（Linux inotify / 非 Linux 轮询），ConfigWatcher 分节点发布/订阅机制监视 daemon.toml 变更
- **TaskManager 与定时任务** (L878-L1104)：基于 tokio-cron-scheduler 的集中式任务管理器，内置任务：eviction、hourly_summary、daily_notes、disk_cleanup、thread_summary、auto_update
- **补全采样** (L853-L877)：pending sample 捕获→accepted 标志更新→条件写入（未接受+编辑距离+速率限制），JSONL 持久化
- **update_thread_usage() / format_thread_stats()** (L1396+)：线程用量持久化（last_response + cumulative 双参数），`/thread stats` 显示 context/total/cache_rate/model
- **并发与锁设计** (L1640-L1658)：RwLock 分层（sessions/meta/commands 读多写少），Mutex 独占，两阶段驱逐和快照式清理避免锁争用
- **数据持久化** (L1581-L1639)：会话目录（meta.json/commands.json/stream.bin），线程文件（JSONL+.meta.json 含用量统计），日志目录（completions/sessions/samples/daemon.log 轮转）

