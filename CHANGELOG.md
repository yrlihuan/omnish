# Changelog

## v0.8.8 (2026-04-08)

### Features
- **protocol**: Protocol version compatibility range and graceful frame skip (#496)
- **chat**: Add /test disconnect command for testing client-daemon disconnect (#495)
- **widget**: Add Label component to menu widget for non-interactive descriptions
- **spinner**: Add spinner animation for running tool status icons (#478)
- **test**: Add disconnect integration test (#494, #495)

### Fixes
- **sandbox**: Detect kernel version for Landlock ABI selection, skip sandbox on kernel < 5.13 (#502)
- **ci**: Set UTF-8 locale in CI for integration tests (#501)
- **tmux**: Fix tmux window title set to empty when child process exits (#500)
- **config**: Use openai-compat (hyphen) instead of openai_compat in config UI (#499)
- **config**: Tolerate duplicate TOML table headers and deduplicate keys (#498)
- **chat**: Only show "Daemon connection lost" on real disconnects, not normal stream end
- **chat**: Show error and mark tools failed on daemon disconnect (#494)
- **task**: Support standard Linux 5-field cron format in task schedules (#493)

### Refactor
- Rename "exchanges" to "messages" in chat thread resumption display
- Remove dead render_chat_history from display

## v0.8.7 (2026-04-03)

### Features
- **config**: Daemon-to-client config push via ConfigClient message (#490)
- **config**: Move completion_enabled under [shell] section in client config
- **task**: Add defaults layer to ConfigMap, unify task/config default values

### Fixes
- **protocol**: Move ConfigClient to end of Message enum to preserve bincode variant indices
- **config**: Use correct TOML types in client config cache, add string_or_bool deserializer
- **config**: Use flock for client.toml write safety, fix value comparison and cleanup recursion
- **config**: Compact config diff display, remove padding between old and new values
- **config**: Backward compat for old top-level proxy/no_proxy config format
- **menu**: Full redraw on rejected toggle to handle callback output
- **menu**: Remove test menu callback output that corrupted toggle redraw
- **client**: Hide /update from /help, add lock on/off to /test help
- **build**: Atomic tar.gz write in build-tar.sh to prevent daemon picking up partial files
- **clippy**: Resolve too_many_arguments and needless_update warnings

### Chores
- Remove dead /update auto command (#491)
- Remove stale auto_update references from client config and test data
- Add test_config_push.sh to integration tests

### Refactor
- Move proxy/no_proxy under [proxy] section in DaemonConfig
- Encapsulate ConfigMap internals, add delegate methods

## v0.8.6 (2026-03-31)

### Features
- **tool**: Increase bash tool default timeout to 5min, max to 15min (#472)
- **config**: Provider preset selector for Add Backend form (#468)
- **config**: Accept string values for integer config fields (e.g. `context_window = "200000"`)
- **notes**: Midnight hourly note saves as 24.md, daily notes at 00:10

### Fixes
- **llm**: Preserve vendor-specific tool call fields like Gemini thought_signature (#480)
- **llm**: Strip trailing slash from base_url to prevent double-slash 404 (#475)
- **llm**: Show full response body in OpenAI-compat decode/error messages (#475)
- **llm**: Fix Gemini backend_type mismatch and context_window type (#473)
- **chat**: Remove runtime sanitization that injects phantom interrupt tool_result (#479)
- **chat**: Remove redundant 600s agent loop timeout (#477)
- **chat**: Sanitize orphaned tool_use blocks to prevent API rejection (#471)
- **chat**: Extend pending agent loop cleanup timeout to 30 minutes (#472)
- **menu**: Prevent duplicate breadcrumb after preset provider select + ESC (#469)
- **config**: Support dotted backend names (e.g. gemini-3.1) in /config menu and TOML keys
- **config**: Fix /config showing stale backend use_proxy value (#476)
- **context**: Unify /context command with actual job logic (#481)
- **completion**: Revert: allow empty input completion requests (#470)

### Refactor
- **config**: Remove schedule_hour and periodic_summary config options (#481)

## v0.8.5 (2026-03-27)

### Features
- **client**: Event log entries for update checks, disconnects, and reconnects (#431)
- **client**: `/config` command with menu-based daemon configuration
- **daemon**: Handle ConfigQuery/ConfigUpdate messages for remote configuration
- **config**: Config schema parsing and config item builder for dynamic menu generation
- **menu**: Handler submenu support with `on_handler_exit` callback, form mode for auto-edit and cursor advance
- **update**: Download packages from GitHub releases, per-host transfer lock, hostname in protocol (#346)
- **update**: Protocol-based update polling, download, and local cache management (#346)
- **daemon**: Scheduled update downloads and periodic cache scan (#346)
- **tool**: Duplicate code finder tool (#375)

### Fixes
- **update**: Prevent multi-process tmp file collision with PID-unique filenames (#438)
- **update**: Add error context to download and install steps (#438)
- **sandbox**: Switch macOS profile from deny-default to allow-default (#437)
- **sandbox**: Add sysctl-read and missing permissions to macOS sandbox profile (#437)
- **config**: Add default values for LLM backend config fields (#440)
- **client**: Remove client-side auto_update config — update checks always enabled (#433)
- **transport**: Stop reconnect loop after repeated auth failures (#431)
- **transport**: Reject clients with mismatched protocol version (#346)
- **client**: Correct notice message on protocol mismatch
- **sandbox**: Upgrade Landlock ABI from V1 to V5 to fix EXDEV
- **client**: Suppress notices during alternate screen (vim, less, htop)
- **client**: Ghost text rendering fixes — deferred render, readline redraw handling
- **daemon**: Retry on LLM connection errors and save progress on failure (#407)
- **client**: Show resume picker when `::` has no last_thread_id (#406)
- **client**: Invert developer_mode semantics — default blocks chat when line has content (#393)
- **daemon**: EMFILE handling — dump fd stats instead of crashing
- **menu**: Navigation rendering, batched ASCII input, text editor quality improvements
- **tool**: Remove `[stderr]` prefix from bash tool output (#427)

### Refactoring
- **update**: Share update utilities, unify auth, support cross-version upgrades
- **client**: Pass cursor and thread-id via env vars on client restart
- **menu**: Extract shared terminal utilities

### Tests
- Integration tests for menu widget, ghost text, and interceptor
- Stricter clippy checks, CI improvements

## v0.8.4 (2026-03-23)

### Features
- **plugin**: Web search formatter — strips HTML tags, shows clean `[Title](URL)` with descriptions (#405)
- **client**: `/test multi_level_picker` — 3-level cascading picker for testing menu interactions

### Fixes
- **plugin**: Formatter must output single-line JSON (`jq -c`) to match daemon's line-based protocol
- **ci**: Fix daemon socket path (`~` not expanded by Rust), use `OMNISH_SOCKET` with absolute path
- **ci**: Copy omnish-plugin binary and plugins dir before starting daemon
- **ci**: Add scheduled integration tests on GitLab CI
- **daemon**: Improve omnish_debug completion matching

### Tests
- Fix CWD detection, chat prompt detection, tool header display, and ScrollView hint matching in integration tests
- Enhance clippy check with stricter warnings
- Fix shell prompt detection for root user in CI

## v0.8.3 (2026-03-22)

### Features
- **daemon**: Custom plugin formatter support via long-running subprocess binaries (#404)
- **daemon**: FormatterManager for unified built-in and external formatter registry
- **daemon**: External formatters communicate via newline-delimited JSON on stdin/stdout with mpsc queue
- **daemon**: Plugins can declare `formatter_binary` in tool.json for custom formatters
- **daemon**: Compact result hint shows (+N more lines) for truncated output (#403)

### Fixes
- **daemon**: System-reminder uses client platform/OS info instead of daemon's (#402)
- **daemon**: Edit tool diff shows only changed lines, not full old/new string (#400)
- **daemon**: Remove TIME field from system-reminder
- **client**: `/debug commands` argument parsing (#396)
- **client**: DEBUG trap moved after bind-x to avoid recording init commands (#395)
- **client**: Developer mode prevents `:` and `::` when command line has content (#393)

### Refactoring
- **plugin**: Move ToolFormatter trait and built-in formatters (Default, Read, Edit) to omnish-plugin
- **daemon**: Simplify FormatInput/FormatOutput — remove fields handled by caller (status_icon, param_desc, display_name, status_template)
- **daemon**: Replace formatter.rs with FormatterManager, decouple StatusIcon from formatters

### Other
- Increase plugin timeout from 30s to 600s
- Add global proxy/no_proxy support for outbound requests (#359)
- Auto-install bundled plugins when tool config has api_key (#397)
- Improve /resume prompt UX with colored host/path display

---

## v0.8.2 (2026-03-21)

### Features
- **daemon**: ToolRegistry for unified tool metadata management — replaces scattered display_name/status_text methods
- **daemon**: PluginManager.register_all() and CommandQueryTool::register() for ToolRegistry population
- **client**: `/debug commands` to show recent shell command history
- **client**: `/debug command <seq>` to show full command details and output
- **client**: Improve thread resume UX with lock-aware picker and configurable disabled icon
- **client**: Truncate long tool results to head 20 + tail 20 when >50 lines (#387)
- **client**: Stream agent loop messages and add daemon-side cancel (#384)
- **client**: Improve Ctrl+C interrupt display and resume cd (#384, #372, #383)
- **client**: Completion cursor awareness — suppress ghost text when cursor not at end of line (#66)
- **daemon**: Increase agent tool call limit from 30 to 100
- **plugin**: Add well-known writable paths to sandbox (#383)

### Fixes
- **tracker**: Recover pending command when CommandStart arrives without PromptStart (#392)
- **tracker**: Use CommandStart timestamp for started_at instead of PromptStart
- **tracker**: Assign seq at finalize time so unused prompts don't consume seq numbers
- **tracker**: Handle escaped semicolons in OSC 133;B command parsing (#391)
- **client**: Fix tool display corruption when output exceeds terminal width (#386)
- **client**: Skip technical error message when `::` resume hits locked thread
- **client**: Check cancel flag between daemon tool executions (#384)
- **client**: Resolve file redirects against intended cwd during chat (#372)
- **client**: Filter empty/unknown commands from system-reminder and history (#385)
- **daemon**: Improve omnish_get_output display to match bash tool style

### Refactoring
- **daemon**: Agent loop uses ToolRegistry for all tool metadata
- **daemon**: Override reload and reconstruct_history flow through ToolRegistry
- **daemon**: Remove redundant metadata methods from PluginManager and CommandQueryTool

---

## v0.8.1 (2026-03-19)

### Features
- **plugin**: macOS sandbox-exec support for plugin subprocesses (#345)
- **plugin**: `sandbox_profile()` and `build_sandbox_profile()` with path escaping and deduplication (#345)

### Fixes
- **common**: Restore build.rs git state tracking with dirty detection (revert 33056cd)
- **plugin**: Deduplicate sandbox profile rules when cwd == repo root (#345)
- **plugin**: Strip control characters in sandbox paths instead of panicking (#345)

### CI
- Add changelog to GitHub release page (#348)

### Docs
- Emphasize coding assistant and agent features in README (#347)
- Reorganize plans and specs under docs/superpowers/

---

## v0.8.0 (2026-03-18)

### Features
- **daemon**: Incremental tool status updates during parallel tool use — each tool sends status as it completes (#344)
- **client**: Parallel tool status redraw — full section erase-and-rerender approach with `redraw_tool_section()`, intermediate results processed from `rpc.call()` (#342)
- **client**: `/test picker` command for integration tests (#343)

### Fixes
- **client**: Parallel tool calls update status icon in place with output below header (#342)
- **common**: `build.rs` always re-runs to detect dirty version

### Docs
- Architecture overview document
- Implementation docs updated for v0.7.x changes (llm, protocol, common, transport, client, daemon)

---

## v0.7.4 (2026-03-18)

### Fixes
- **client**: Picker scroll_offset overflow causes duplicate items when pre-selected item is scrolled (#337)
- **client**: Resume separator missing `ctrl+o` hint (#341)
- **daemon**: Agent loop uses wrong backend after client-side tool resumption (#339)
- **daemon**: Preserve thinking blocks in OpenAI-compat tool use loop (#339)
- **daemon**: Enforce sandbox on all tools, remove sandboxed opt-out (#322)

### Other
- **client**: Deferred thread creation until first message in chat mode (#336)
- **client**: Integration test for picker scroll rendering (`verify_issue_337.sh`)

---

## v0.7.3 (2026-03-18)

### Features
- **client**: `/model` picker command for per-thread model selection in chat mode (#154)
- **daemon**: `__cmd:models` builtin command listing available backends with selected flag (#154)
- **protocol**: `ChatMessage.model` field for backend override, protocol v7 (#154)
- **llm**: `MultiBackend` stores named backends, supports `list_backends`/`get_backend_by_name` (#154)
- **client**: `pick_one_at` picker widget with pre-selected index (#154)
- **client**: Ghost text hint with model name in chat mode (#334)
- **daemon**: Preserve thinking blocks in assistant messages (#335)

---

## v0.7.2 (2026-03-18)

### Features
- **daemon**: `/debug daemon` command showing version, tasks, and auto-update status (#326)
- **daemon**: `omnish_debug` canned completion response for end-to-end ghost text testing (#328)
- **test**: Ghost text completion integration test using omnish_debug (#328)

### Fixes
- **client**: Defer ghost text render to survive bash readline redraw (#327)
- **client**: All command outputs skip markdown rendering to preserve formatting (#329)
- **client**: `/debug client` output missing blank lines between sections (#329)
- **client**: Remove unused auto_trigger feature (#330)
- **install**: Prevent duplicate `[tasks.auto_update]` section in daemon.toml
- **install**: Fix config/daemon.toml auto_update section name

---

## v0.7.1 (2026-03-18)

### Features
- **client**: `/integrate` command for tmux, screen, ssh integration (#318)
- **daemon**: Graceful shutdown and restart after auto-update (#325)
- **common**: Generic `set_toml_value` helper, persist `/update auto` setting (#324)
- **install**: Set `max_content_chars` per provider (deepseek=130k, completion=32k)

### Fixes
- **install**: Prevent self-replacement corruption during upgrade (`{ ... exit; }` wrapper)
- **install**: Remove old binaries before copy to avoid "Text file busy" error
- **install**: Remove remote binaries before scp to avoid overwrite failure

---

## v0.7.0 (2026-03-18)

### Features
- **install**: Generate daemon.toml/client.toml with all config options as commented defaults (#323)
- **client**: New user onboarding welcome message (#317)
- **install**: Show directory and client info in completion message (#317)
- **client**: Numbered diff display for edit tool output (#321)

### Fixes
- **install**: Deploy exits on first client due to `((0++))` under `set -e` (#320)
- **daemon**: Rename `source_dir` to `check_url` for auto-update source config

---

## v0.6.9 (2026-03-18)

### Features
- **install**: `--dir=<path>` flag to install from local directory containing tar.gz files (#316)
- **daemon**: Auto-update supports local directory source via `source_dir` config

### Fixes
- **install**: Prevent ssh/scp from stealing tty input during client deployment (BatchMode=yes)
- **install**: Move server IP selection to right after TCP address choice
- **daemon**: Default stderr log level to debug to match file output

---

## v0.6.8 (2026-03-17)

### Features
- **install**: Retry loop for backend config preview instead of aborting
- **install**: API type selection (OpenAI-compat vs Anthropic) for custom providers
- **install**: Bash re-exec guard for sh/dash invocations

### Fixes
- **llm**: Tolerate individual backend init failures — bad config no longer breaks all backends (#315)
- **install**: Skip deploy when `--upgrade` finds no update (exit code 2)
- **install**: Skip manual PATH hint when user declines shell profile update

### Performance
- Enable thin LTO and size optimization for release builds (~30% smaller binaries)

---

## v0.6.7 (2026-03-17)

### Features
- **daemon**: Include thread conversations in hourly notes context (#251)
- **client**: Configurable `resume_prefix` for resuming last thread (#314)
- **client**: Improve ctrl+o hint — show "to expand", hide in browse mode (#299)
- **plugin**: Add git repo root to sandbox writable paths (#312)
- **install**: Private IP selection, auto-update prompt, cleaner output

---

## v0.6.6 (2026-03-17)

### Features
- **install**: Add `--upgrade` flag for non-interactive updates (replaces standalone update.sh)
- **install**: Interactive client deployment via scp with SSH connectivity check
- **install**: Generate client.toml on server with full config options
- **install**: Prepend demonstration warning to installed tool.json and chat.json
- **daemon**: Periodic auto-update from GitHub with client distribution (#308)
- **ci**: Add macOS build to GitHub CI for client binaries (#307)

### Refactoring
- Move static assets (tool.json, chat.json) from daemon binary to tar package
- Split update/deploy into separate scripts; then consolidate update into install.sh --upgrade

---

## v0.6.5 (2026-03-17)

### Features
- **daemon**: Periodic thread summary generation task (#301)
- **daemon**: Claude Code-style numbered diff for edit formatter (#300)
- **daemon**: Read formatter shows numbered lines for N<=10, summary for N>10 (#298)
- **daemon**: Edit formatter shows old_string on error
- **client**: Show thread summary and title in `/thread list` (#306)
- **plugin**: Read tool output uses `cat -n` format (tab separator) (#305)
- **plugin**: Add edit-over-write preference hint to write tool description
- **install**: Add `install.sh` for automated server deployment with interactive LLM configuration
- **install**: Support `OMNISH_HOME` env var to override default `~/.omnish` directory
- **daemon**: `--init` flag for credential generation without starting server

### Bug Fixes
- **daemon**: Edit diff shows full lines instead of raw old_string (#303)
- **daemon**: Edit formatter shows diff for deletions and duplicate text (#300)
- **llm**: CJK char truncation panic in langfuse (#302)
- **client**: Indent LLM response lines to align with bullet prefix (#297)
- **client**: Apply bullet prefix and indent to intermediate LLM text (#297)
- **client**: Remove "+N lines" hint after result_compact
- **client**: Add empty line before response in browse mode (#296)
- **client**: Correct page-up/down key order in browse hint (#296)

### Build & CI
- Fix GitHub release permissions and update action version

---

## v0.6.4 (2026-03-16)

### Features
- **client**: Markdown rendering in chat responses (#272)
- **client**: ScrollView widget with compact/browse mode for long chat responses (#274)
- **client**: Full conversation history on `/resume` with ScrollView (#275)
- **client**: ChatLayout region-based widget layout manager for unified chat rendering
- **client**: Alternate screen for scroll view browse mode (#281)
- **client**: Combine tool status and chat response in scroll view (#284)
- **client**: Visual markers for user/tool/response in chat (#285)
- **client**: Preserve chat history across query rounds (#286)
- **client**: Unified tool output format with ⎿ gutter and head-first truncation (#287)
- **client**: Dismiss ghost completion with ESC key (#259)
- **client**: Ctrl-F/Ctrl-B page scrolling in browse mode (#282)
- **client**: Wait for Enter after each test case in `-w` mode (#289)
- **daemon**: Structured ChatToolStatus with display fields (#292)
- **daemon**: Formatter module with built-in tool formatters (#292)
- **daemon**: Structured history reconstruction for `/resume` (#293)
- **plugin**: Display name and formatter fields in tool metadata (#292)
- **plugin**: Edit formatter shows colored context diff output (#295)

### Bug Fixes
- **client**: Position cursor after "> " prefix when entering chat mode (#277)
- **client**: Use relative cursor movement for editor redraw (#278)
- **client**: Preserve shell prompt when entering chat mode (#279)
- **client**: Handle visual line wrapping in editor redraw (#283)
- **client**: Panic on CJK char boundary when truncating tool status (#288)
- **client**: Let long lines wrap in browse mode instead of clipping (#288)
- **client**: ESC dismiss — consume key and flush DSR detector (#259)
- **client**: Include user input lines in chat browse history (#290)
- **client**: Color only the > prompt, not user text in browse history (#290)
- **daemon**: Read formatter shows "Read N lines" in compact output (#294)
- **daemon**: Edit context diff — tool returns snippet, formatter parses it (#295)
- **plugin**: Add /dev/null to Landlock writable paths (#273)

### Performance
- **client**: Fire-and-forget completion summary RPC to avoid blocking event loop

### Refactoring
- **client**: Extract chat logic into ChatSession, use natural scrollback
- **client**: Unify truncation into `display::truncate_cols` with CJK support (#288)
- **client**: Inline ScrollView hint and Ctrl+O browse into chat input
- **client**: Move browse key handling into `ScrollView::run_browse()`
- **client**: Resolve all clippy warnings across workspace

### Build & CI
- **ci**: Add GitHub Actions CI workflow (stable only)

---

## v0.6.3 (2026-03-13)

### Features
- **plugin**: Add grep tool with regex search, glob/type filters, context lines, multiline mode, and pagination (#271)
- **plugin**: Add glob tool for file pattern matching (#265)
- **plugin**: Update bash tool description and timeout handling — timeout now in milliseconds, 30000 char truncation (#268)
- **plugin**: Update read tool limits to 2000 lines / 2000 chars per line (#267)
- **plugin**: Update edit tool description with usage guidelines (#269)
- **plugin**: Update write tool description (#270)
- **client**: Timing-based `::` resume shortcut for chat (#261)

### Bug Fixes
- **plugin**: Fix read tool panic on multi-byte UTF-8 character truncation (#266)
- **tls**: Use native root certs for TLS proxy compatibility (rustls-tls-native-roots)
- **client**: Remove redundant prompt from prefix buffering phase, tune prefix timeout to 250ms (#261)
- **llm**: Add missing usage field in mock LlmResponse constructors

### Refactoring
- **plugin**: Replace hand-rolled grep with ripgrep crates (grep-regex, grep-searcher, ignore)
- **plugin**: Move plugin assets from `plugins/builtin/` to `assets/`

---

## v0.6.2 (2026-03-13)

### Features
- **llm**: Parse usage from LLM API responses — input/output tokens, cache read/creation tokens from both Anthropic and OpenAI-compat backends (issue #263)
- **llm**: Enable thinking mode for chat (issue #262)
- **client**: Add configurable shortcut to resume last conversation (issue #261)
- **daemon**: Add system-reminder to `/context chat` display
- **daemon**: Embed chat prompt JSON in binary, install to `~/.omnish/` on startup (issue #257)
- **daemon**: Parallel tool execution when LLM requests multiple tools (issue #248)
- **client**: Allow Ctrl-C to interrupt agent tool-calling loop (issue #241)
- **client**: Adjust `/template chat` and `/context chat` behavior (issue #250)
- **daemon**: Redesign chat system-reminder with time, cwd, and last 5 commands (issue #249)
- **daemon**: Keep line status visible after operation completes
- **plugin**: Allow plugins to write to current working directory
- **plugin**: Hot-reload prompt.json / tool.override.json via inotify
- **plugin**: Support prompt.json for user-specified tool descriptions
- **plugin**: Support multi-line description arrays in tool.json
- **plugin**: Auto-install builtin tool.json on first daemon startup
- **llm**: Add environment info to chat system-reminder

### Refactoring
- **daemon**: Rename prompt.json to tool.override.json, add chat.override.json support (issue #254)
- **daemon**: Redesign chat system prompt based on Claude Code patterns
- **plugin**: Remove Plugin trait, simplify to inherent methods and tool.json definitions
- **plugin**: Rewrite PluginManager to load from tool.json files

### Bug Fixes
- **llm**: Record actual input content in Langfuse instead of char count (issue #260)
- **build**: Make inotify usage conditional for cross-platform compilation
- **daemon**: Increase agent loop timeout from 60s to 600s (issue #237)
- **client**: Support redirect and limit for `/debug client` and `/update auto` (issue #239)

---

## v0.6.0 (2026-03-11)

### Features
- **client**: Transparent self-restart via `execvp` with `/update` command — preserves PTY connection across binary updates (issue #217)
- **client**: Auto-update when binary changes on disk — periodic mtime check with idle detection (issue #223)
- **client**: `/update auto` runtime toggle for auto-update (not persisted)
- **plugin**: Add privileged mode for `write` and `edit` tools to bypass Landlock sandbox (issue #219)
- **tools**: Add `edit` tool for exact string replacement (issue #216)
- **tools**: Add `write` tool for file creation
- **bash**: Set cwd to shell's current directory for bash tool execution
- **client**: `/resume` picker shows all threads with dynamic viewport (issue #220)
- **widgets**: `InlineNotice` widget for non-intrusive notifications above cursor
- **widgets**: Deferred notice queue with flush-on-chat-exit for chat mode
- **daemon**: Increase max tool call iterations from 5 to 30

### Bug Fixes
- **client**: Fix `/update` message rendering in raw terminal mode (use `\r\n` and InlineNotice)
- **client**: Strip " (deleted)" suffix from `/proc/self/exe` on Linux
- **client**: Prevent completion request flood after `/update`  (issue #224)
- **update**: Codesign binary on macOS before exec to avoid SIGKILL
- **plugin**: Gate landlock and prctl behind `cfg(target_os = "linux")` for macOS support
- **notice**: Route all `[omnish]` messages through notice system

### Refactoring
- **plugin**: Extract shared spawn/sandbox/JSON-RPC into omnish-plugin lib
- **daemon**: Remove re-export stubs, import tools from omnish-plugin directly
- **plugin**: Trim edit tool system prompt

### Build & CI
- **ci**: Run tests on regular commits, build+release only on tags

---

## v0.5.5 (2026-03-10)

### Features
- **client**: Rename binary from `omnish-client` to `omnish` (issue #200)
- **plugin**: Sandbox external plugins with Landlock — restrict write access to plugin data dir and /tmp (issue #176)
- **plugin**: Add `omnish-plugin` binary crate for official plugins via subprocess + JSON-RPC (issue #198)
- **plugin**: Let plugins provide their own system prompt fragments (issue #199)
- **plugin**: Support customized prompts via `PROMPT.md` / `PROMPT_*.md` files (issue #209)
- **plugin**: Move tool status text into Plugin trait; forward via JSON-RPC `tool/status_text`
- **plugin**: Set process name to `omnish-plugin(<tool>)` for visibility (issue #208)
- **client**: Use `omnish-plugin` subprocess for client-side tool execution with Landlock sandbox
- **llm**: Add PromptManager for composable system prompt fragments (identity, chat_mode, commands, tool_status, guidelines)
- **llm**: Add optional Langfuse observability integration
- **daemon**: Forward LLM text blocks to client during tool_use as status messages
- **widgets**: Enhance LineStatus with truncation, append, and max lines

### Bug Fixes
- **llm**: Retry on 429/529 with exponential backoff and retry-after header (issue #207)
- **llm**: Only log chat messages to `logs/messages/` (issue #205)
- **client**: Fix `/context > file` redirect not working in chat mode (issue #210)
- **client**: Remove duplicate status line on ChatToolCall
- **daemon**: Remove debug log for skipping active session cleanup (issue #206)
- **langfuse**: Treat secret_key as direct value, not shell command

### Performance
- **daemon**: Increase worker threads from 16 to 30 (issue #204)

---

## v0.5.4 (2026-03-10)

### Features
- **protocol**: Add protocol version mismatch warning between client and daemon (issue #117)
- **plugin**: Add PluginType classification (DaemonTool/ClientTool) for client-side tool execution (issue #195)
- **daemon**: Built-in bash tool for chat agent — executes commands on the user's machine
- **daemon**: Agent loop pause/resume architecture for forwarding tool calls to client
- **protocol**: Add ChatToolCall and ChatToolResult messages for client-side tool forwarding

### Bug Fixes
- **protocol**: Use String instead of serde_json::Value in ChatToolCall for bincode compatibility

---

## v0.5.2 (2026-03-09)

### Features
- **llm**: Save full LLM request payloads to `~/.omnish/logs/messages/` with timestamp filenames, rolling to keep last 30 (issue #170)
- **client**: Paste blocks integrated into LineEditor as FFFC placeholders — cursor can navigate around paste blocks and insert text before/after them (issue #188)

### Bug Fixes
- **client**: Exec shell directly when stdin is not a tty (issue #193)
- **widgets**: Fix LineStatus off-by-one erase leaving residual text

### Build & CI
- **build**: Replace aws-lc-rs with ring for musl static binary support (issue #190)
- **ci**: Add GitLab CI configuration with check, test, build-release stages (issue #192)
- **ci**: Add release stage with downloadable static binary links on tag push
- **refactor**: Fix clippy warnings across workspace

---

## v0.5.1 (2026-03-09)

### Features
- **widgets**: `LineEditor` — full-featured chat input with cursor movement (←/→/↑/↓, Home/End, Ctrl-A/E), word jump (Ctrl-←/→), word delete, and multi-line editing (issue #180)
- **client**: Multi-line paste via bracketed paste mode; fast-paste detection as fallback; large pastes collapsed to `[pasted text N chars]` marker
- **client**: Shift+Enter / Ctrl+J inserts newline in chat input
- **widgets**: `LineStatus` — temporary single-line status display that erases itself completely on `clear()`, fixing residual `(thinking...)` and tool-status lines (issue #183)

### Bug Fixes
- **client**: Paste block backspace display and cursor positioning
- **client**: Track terminal cursor row for correct multi-line redraw
- **daemon**: Use char boundary for `/thread list` question truncation

### Refactoring
- **widgets**: Move picker into `widgets/` module alongside LineEditor and LineStatus
- **client**: Integrate paste blocks into LineEditor as placeholder characters

### Removals
- **tools**: Remove `omnish-commands` diagnostic binary

---

## v0.5.0 (2026-03-08)

### Features
- **daemon**: Agent loop with tool execution — LLM can call `command_query` tool to inspect command output, up to 5 iterations (issue #161)
- **daemon**: Rewrite ConversationManager to raw JSON storage format for KV cache-optimized conversation replay (issue #166)
- **protocol**: ChatToolStatus message type for streaming tool-use status to client
- **transport**: Streaming multi-message RPC responses with end-of-stream sentinel
- **client**: Stream ChatToolStatus messages during agent tool execution
- **client**: `/thread del` uses multi-select picker widget when no index given (issue #168)
- **template**: Add Tools section to CHAT_SYSTEM_PROMPT documenting command_query usage
- **template**: Move `/template chat` to daemon request, show actual tool definitions (issue #164)
- **context**: Wrap workingDirectory in `<system-reminder>` tags for auto-complete (issue #167)
- **daemon**: Include recent command list directly in chat context (issue #165)

### Bug Fixes
- **llm**: Use config `base_url` for Anthropic backend, upgrade API version to 2024-04-04
- **transport**: Fix stream memory leak — add Ack sentinel for multi-message RPC cleanup
- **daemon**: Don't persist `<system-reminder>` in thread JSONL files (issue #169)
- **daemon**: Replace newlines in `/thread list` question preview
- **client**: Sort picker indices numerically, not lexicographically

### Breaking Changes
- **storage**: Chat thread JSONL format changed from `{role, content, ts}` to raw Anthropic API JSON. Old thread files must be deleted (`rm ~/.local/share/omnish/threads/*.jsonl`).

---

## v0.4.1 (2026-03-06)

### Bug Fixes
- **client**: `/context` with pipe (e.g. `| tail -n 5`) now correctly uses thread-aware routing in chat mode after `/resume` (issue #144, #145)
- **client**: Backspace only exits chat mode before first message is sent; suppressed visual artifacts on empty buffer backspace (issue #127)
- **client**: Default to 10 lines when `| head` or `| tail` has no number argument
- **client**: Parse redirect before limit to support both together

### Features
- **client**: Support `| head`/`| tail` with `-n N` and `-nN` syntax for command output
- **client**: Add shell cwd to `/debug client` output (issue #146)

### Testing & Tooling
- **tools**: Add shared integration test library (`lib.sh`) with tmux helpers, assertions, and test runner
- **tools**: Add integration tests for issue #127 and #144
- **tools**: Fix test tmux config to use bash as default shell instead of installed omnish

---

## v0.4.0 (2026-03-04)

### Features
- **client**: Multi-turn chat mode with `/chat`, `/resume`, `/conversations`, `/threads` (issue #110, #111, #121, #128, #129)
- **client**: Deferred thread creation — thread only created on first message (issue #130)
- **client**: `/context` in chat mode shows current thread's conversation (issue #136)
- **client**: Ctrl-C interrupts pending chat LLM request (issue #123)
- **client**: Ctrl-D and backspace-on-empty exit chat mode (issue #120, #124)
- **client**: `/resume [N]` to select and resume conversations by index (issue #111, #133)
- **llm**: System prompt for chat mode with command awareness (issue #140)
- **llm**: Multi-turn conversation support in Anthropic and OpenAI backends (issue #110)
- **protocol**: ChatStart/ChatReady/ChatMessage/ChatResponse message types (issue #110)
- **daemon**: ConversationManager for thread storage and retrieval (issue #110)
- **daemon**: Load conversations into memory at startup (issue #131)
- **daemon**: Relative time display in `/conversations` (issue #139)
- **daemon**: JSON command responses with display field (issue #134)
- **client**: Ghost completion restored in chat mode (issue #119)
- **client**: `/debug client` restored in chat mode via closure (issue #115)
- **completion**: Disable thinking mode for auto-completion requests (issue #118)

### Bug Fixes
- **client**: Handle multi-byte UTF-8 in backspace (issue #141)
- **client**: Enter chat mode immediately on prefix match (issue #116)
- **client**: Process initial message from interceptor in chat loop (issue #114)
- **client**: Render /commands inline in chat mode without PTY cleanup (issue #114)
- **client**: Clear full readline on chat exit to prevent stale command execution (issue #125)
- **client**: Track isearch mode with dedicated flag, not timeout (issue #88)
- **client**: Skip readline trigger when user typed since completion request (issue #88)
- **client**: Discard completion suggestions that diverge from input (issue #113)
- **client**: Discard completion responses during emacs-isearch mode (issue #88)
- **client**: Remove debug state header/footer from output (issue #135)
- **client**: `/resume` shows last exchange of resumed conversation (issue #137)
- **client**: Auto-fetch conversations when `/resume N` cache is empty (issue #133)
- **daemon**: Resolve interrupted chat exchanges on load (issue #126)
- **daemon**: Suppress noisy rustls debug logs (issue #132)
- **llm**: Pass `enable_thinking=false` to vLLM (issue #118)

---

## v0.3.0 (2026-03-01)

### Features
- **context**: `/context <scenario>` command for viewing different context types (completion, chat, daily-notes, hourly-notes)
- **context**: `/template <name>` command for viewing prompt templates
- **client**: `/version` command
- **transport**: TLS support for TCP connections
- **transport**: Socket permissions (0600) and peer UID verification
- **transport**: Token authentication for RPC server
- **protocol**: Auth and AuthFailed message variants
- **common**: Auth token generation and loading utilities
- **context**: Reuse existing context building functions for hourly/daily notes
- **completion**: Proactive KV cache warmup on context prefix change
- **completion**: XML tags with hostname:cwd prompt format
- **completion**: Concurrent completion requests with intelligent filtering
- **completion**: Prefer full command for 2nd suggestion (issue #93, #95)
- **context**: Per-command output char limit and reduced max_line_width
- **daemon**: Daily notes include hourly notes as LLM context (issue #63)
- **daemon**: Completion sampling infrastructure and logic (issue #101)
- **store**: JSONL sample writer thread (issue #101)
- **client**: Completion enabled toggle in client config
- **debug**: Show version in `/debug client` output
- **version**: Auto-embed git version via build.rs
- **template**: Hourly-notes template

### Bug Fixes
- **completion**: Reset debounce on all input activity (issue #100)
- **completion**: Subtract input prefix length in issue #95 check (issue #99)
- **completion**: Suppress ghost text when cursor not at end of input
- **completion**: Freeze history section between elastic resets for KV cache
- **context**: Trim leading whitespace from command output
- **daemon**: Release sessions read lock before disk I/O in cleanup (issue #61)
- **daemon**: Eliminate lock contention causing all clients to freeze (issue #61)
- **daemon**: Use local timezone for cron scheduler
- **hook**: Bind readline trigger in emacs-isearch keymap (issue #88)
- **llm**: Unify completion prompt template for KV cache stability
- **llm**: Handle `<think>` tag at start of response without leading newline
- **client**: Discard completion that is a subset of current input
- **client**: Clear stale readline content on prompt return
- **client**: Clear pending_rl_report on prompt (issue #34)
- **client**: Auto-clear pending_rl_report after 1s timeout (issue #57)
- **context**: Replace home directory with ~ in paths
- **daily-notes**: Use local timezone, include hourly notes in LLM context only
- **shell-hook**: Suppress error when emacs-isearch keymap not available
- **completion**: Avoid suggesting && chained commands unless in history
- **daemon**: Truncate completion suggestions at && when input has none (issue #107)
- **chat**: Only include recent commands with output in chat context (issue #109)
- **macOS**: Full macOS support for shell probes

---

## v0.2.0 (2026-02-24)

### Features
- **version**: Auto-embed git version via build.rs and `--version` flag
- **daemon**: Highlight current session in `/sessions` output

---

## v0.1.1 (2026-02-22)

Initial tagged release with core functionality:

### Features
- **core**: PTY proxy with forkpty, raw mode, and window resize
- **core**: Client-daemon architecture with Unix socket transport
- **core**: Binary framed protocol with bincode serialization
- **core**: Session manager with metadata and binary stream storage
- **llm**: LlmBackend trait with Anthropic and OpenAI-compatible backends
- **context**: Two-tier context (history + detailed commands) with elastic window
- **context**: Strategy/formatter pattern for context building
- **tracker**: Command boundary detection via OSC 133 + regex fallback
- **client**: Ghost text completion with Tab accept
- **client**: Chat mode with `:` prefix and inline prompt UI
- **client**: `/debug`, `/sessions`, `/context` commands
- **client**: Input interceptor with alternate screen suppression
- **daemon**: Daily notes with command log and LLM summary
- **daemon**: Session eviction for inactive sessions
- **transport**: RPC client/server with reconnection and exponential backoff
- **store**: Command recording and persistence
