# 实时CWD（当前工作目录）跟踪实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**目标:** 修复context中的cwd问题，使其反映命令运行时的实际工作目录，而非会话创建时的工作目录

**架构:** 扩展OSC 133协议以支持cwd信息传递，修改shell hook在命令执行前发送cwd，更新CommandTracker使用运行时cwd而非会话初始cwd

**技术栈:** Rust, Bash shell hook, OSC 133终端协议

---

### Task 1: 扩展OSC 133协议支持cwd

**文件:**
- 修改: `crates/omnish-tracker/src/osc133_detector.rs:1-140`
- 修改: `crates/omnish-tracker/src/command_tracker.rs:149-195`
- 测试: `crates/omnish-tracker/src/osc133_detector.rs:164-290`
- 测试: `crates/omnish-tracker/src/command_tracker.rs:550-675`

**步骤1: 编写失败的测试**

```rust
#[test]
fn test_osc133_command_start_with_cwd() {
    use crate::osc133_detector::*;
    let mut detector = Osc133Detector::new();
    let events = detector.feed(b"\x1b]133;B;echo hello;cwd:/home/user/project\x07");
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].kind,
        Osc133EventKind::CommandStart {
            command: Some("echo hello".into()),
            cwd: Some("/home/user/project".into())
        }
    );
}
```

**步骤2: 运行测试验证失败**

运行: `cargo test -p omnish-tracker test_osc133_command_start_with_cwd`
预期: FAIL with "field `cwd` not found"

**步骤3: 修改Osc133EventKind枚举添加cwd字段**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc133EventKind {
    PromptStart,
    CommandStart { command: Option<String>, cwd: Option<String> },
    OutputStart,
    CommandEnd { exit_code: i32 },
}
```

**步骤4: 更新parse_osc133函数解析cwd**

```rust
fn parse_osc133(buf: &[u8]) -> Option<Osc133EventKind> {
    // ... 现有代码 ...

    match payload {
        b"A" => Some(Osc133EventKind::PromptStart),
        b"B" => Some(Osc133EventKind::CommandStart { command: None, cwd: None }),
        b"C" => Some(Osc133EventKind::OutputStart),
        _ => {
            if payload.len() >= 2 && payload[0] == b'B' && payload[1] == b';' {
                // B;command_text;cwd:/path
                let rest = &payload[2..];
                let parts: Vec<&[u8]> = rest.split(|&b| b == b';').collect();

                let command = if !parts.is_empty() && !parts[0].is_empty() {
                    std::str::from_utf8(parts[0]).ok().map(|s| s.trim().to_string())
                } else {
                    None
                };

                let mut cwd = None;
                for part in parts.iter().skip(1) {
                    if part.starts_with(b"cwd:") {
                        cwd = std::str::from_utf8(&part[4..])
                            .ok()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty());
                        break;
                    }
                }

                Some(Osc133EventKind::CommandStart { command, cwd })
            } else if payload.len() >= 2 && payload[0] == b'D' && payload[1] == b';' {
                // ... 现有代码 ...
            } else {
                None
            }
        }
    }
}
```

**步骤5: 运行测试验证通过**

运行: `cargo test -p omnish-tracker test_osc133_command_start_with_cwd`
预期: PASS

**步骤6: 提交**

```bash
git add crates/omnish-tracker/src/osc133_detector.rs
git commit -m "feat: extend OSC 133 protocol to support cwd in CommandStart events"
```

---

### Task 2: 更新shell hook发送cwd信息

**文件:**
- 修改: `crates/omnish-client/src/shell_hook.rs:1-101`

**步骤1: 编写测试验证hook包含cwd**

```rust
#[test]
fn test_hook_content_includes_cwd() {
    assert!(BASH_HOOK.contains("PWD"));
    assert!(BASH_HOOK.contains("cwd:"));
}
```

**步骤2: 运行测试验证失败**

运行: `cargo test -p omnish-client test_hook_content_includes_cwd`
预期: FAIL

**步骤3: 更新BASH_HOOK发送cwd**

```rust
const BASH_HOOK: &str = r#"
# omnish shell integration — OSC 133 semantic prompts
__omnish_preexec_fired=0
__omnish_in_precmd=0

__omnish_prompt_cmd() {
  local ec=$?
  __omnish_in_precmd=0
  __omnish_preexec_fired=0
  printf '\033]133;D;%d\007' "$ec"
  printf '\033]133;A\007'
}
# Bracket PROMPT_COMMAND: prepend in_precmd=1 guard, append prompt_cmd.
# The guard assignment triggers DEBUG but matches __omnish_* so it's skipped,
# then the assignment executes, protecting all subsequent PROMPT_COMMAND entries
# (e.g. history -a) from being recorded as user commands.
# Strip trailing semicolons/whitespace to avoid ";;" syntax errors.
__omnish_pc="$PROMPT_COMMAND"
while [[ "$__omnish_pc" =~ [[:space:]\;]$ ]]; do __omnish_pc="${__omnish_pc%?}"; done
PROMPT_COMMAND="__omnish_in_precmd=1;${__omnish_pc:+$__omnish_pc;}__omnish_prompt_cmd"
unset __omnish_pc

__omnish_preexec() {
  [[ "$__omnish_in_precmd" == "1" ]] && return
  [[ "$__omnish_preexec_fired" == "1" ]] && return
  [[ "$BASH_COMMAND" == __omnish_* ]] && return
  __omnish_preexec_fired=1
  # Escape semicolons in command and PWD for OSC 133 payload
  local cmd_esc="${BASH_COMMAND//;/\\;}"
  local pwd_esc="${PWD//;/\\;}"
  printf '\033]133;B;%s;cwd:%s\007' "$cmd_esc" "$pwd_esc"
  printf '\033]133;C\007'
}
trap '__omnish_preexec' DEBUG
"#;
```

**步骤4: 运行测试验证通过**

运行: `cargo test -p omnish-client`
预期: PASS (所有测试)

**步骤5: 提交**

```bash
git add crates/omnish-client/src/shell_hook.rs
git commit -m "feat: update bash hook to send cwd with OSC 133 B command"
```

---

### Task 3: 修改CommandTracker使用运行时cwd

**文件:**
- 修改: `crates/omnish-tracker/src/command_tracker.rs:1-81`
- 修改: `crates/omnish-tracker/src/command_tracker.rs:149-195`
- 测试: `crates/omnish-tracker/src/command_tracker.rs:550-600`

**步骤1: 更新PendingCommand结构体存储cwd**

```rust
struct PendingCommand {
    seq: u32,
    started_at: u64,
    stream_offset: u64,
    input_buf: Vec<u8>,
    output_lines: Vec<String>,
    /// True once we've seen \r or \n in the input (user pressed Enter).
    /// Output before this point is shell echo and should be excluded from the summary.
    entered: bool,
    /// Command line text from OSC 133;B payload (shell's $BASH_COMMAND).
    osc_command_line: Option<String>,
    /// Current working directory from OSC 133;B payload.
    osc_cwd: Option<String>,
}
```

**步骤2: 更新finalize_command使用osc_cwd优先**

```rust
fn finalize_command(
    &self,
    pending: PendingCommand,
    timestamp_ms: u64,
    stream_pos: u64,
    exit_code: Option<i32>,
) -> CommandRecord {
    let command_line = pending
        .osc_command_line
        .or_else(|| extract_command_line(&pending.input_buf));

    // Use runtime cwd if available, otherwise fall back to session cwd
    let cwd = pending.osc_cwd.or_else(|| self.cwd.clone());

    let output_summary = make_summary(&pending.output_lines);
    let stream_length = stream_pos - pending.stream_offset;
    CommandRecord {
        command_id: format!("{}:{}", self.session_id, pending.seq),
        session_id: self.session_id.clone(),
        command_line,
        cwd,
        started_at: pending.started_at,
        ended_at: Some(timestamp_ms),
        output_summary,
        stream_offset: pending.stream_offset,
        stream_length,
        exit_code,
    }
}
```

**步骤3: 更新feed_osc133处理cwd**

```rust
pub fn feed_osc133(
    &mut self,
    event: Osc133Event,
    timestamp_ms: u64,
    stream_pos: u64,
) -> Vec<CommandRecord> {
    self.osc133_mode = true;
    let mut completed = Vec::new();

    match event.kind {
        Osc133EventKind::PromptStart => {
            // ... 现有代码 ...
            self.pending = Some(PendingCommand {
                seq: self.next_seq,
                started_at: timestamp_ms,
                stream_offset: stream_pos,
                input_buf: Vec::new(),
                output_lines: Vec::new(),
                entered: false,
                osc_command_line: None,
                osc_cwd: None,
            });
            // ... 现有代码 ...
        }
        Osc133EventKind::CommandStart { command, cwd } => {
            if let Some(ref mut pending) = self.pending {
                pending.entered = true;
                pending.osc_command_line = command;
                pending.osc_cwd = cwd;
            }
        }
        // ... 现有代码 ...
    }

    completed
}
```

**步骤4: 更新所有PendingCommand创建点**

查找并更新所有`PendingCommand { ... }`初始化，添加`osc_cwd: None`字段。

**步骤5: 编写测试验证cwd优先级**

```rust
#[test]
fn test_cwd_from_osc133_overrides_session_cwd() {
    use crate::osc133_detector::*;
    let mut tracker = CommandTracker::new(
        "sess1".into(),
        Some("/initial/cwd".into()), // Session cwd
    );

    tracker.feed_osc133(
        Osc133Event { kind: Osc133EventKind::PromptStart, start: 0, end: 8 },
        1000, 0,
    );

    tracker.feed_osc133(
        Osc133Event {
            kind: Osc133EventKind::CommandStart {
                command: Some("ls".into()),
                cwd: Some("/runtime/cwd".into())
            },
            start: 0, end: 20,
        },
        1001, 50,
    );

    tracker.feed_input(b"\r", 1001);

    let cmds = tracker.feed_osc133(
        Osc133Event { kind: Osc133EventKind::CommandEnd { exit_code: 0 }, start: 0, end: 10 },
        1003, 100,
    );

    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].cwd.as_deref(), Some("/runtime/cwd")); // Should use runtime cwd
}
```

**步骤6: 运行测试验证通过**

运行: `cargo test -p omnish-tracker`
预期: PASS (所有测试)

**步骤7: 提交**

```bash
git add crates/omnish-tracker/src/command_tracker.rs
git commit -m "feat: CommandTracker uses runtime cwd from OSC 133, falls back to session cwd"
```

---

### Task 4: 更新测试以反映新行为

**文件:**
- 修改: `crates/omnish-tracker/src/command_tracker.rs:550-600`
- 修改: `crates/omnish-tracker/src/osc133_detector.rs:260-290`

**步骤1: 更新现有测试使用新的Osc133EventKind格式**

查找所有使用`Osc133EventKind::CommandStart { command: ... }`的地方，更新为`Osc133EventKind::CommandStart { command: ..., cwd: None }`。

**步骤2: 运行测试确保通过**

运行: `cargo test -p omnish-tracker`
预期: PASS (所有测试)

**步骤3: 提交**

```bash
git add crates/omnish-tracker/src/
git commit -m "test: update tests for new OSC 133 cwd format"
```

---

### Task 5: 更新context模块测试

**文件:**
- 修改: `crates/omnish-context/src/recent.rs:100-200`
- 修改: `crates/omnish-context/src/format_utils.rs:130-180`
- 测试: `crates/omnish-context/tests/`

**步骤1: 检查context是否正确显示cwd**

运行现有测试确保cwd显示正常:
运行: `cargo test -p omnish-context`
预期: PASS

**步骤2: 添加测试验证cwd在格式化输出中**

```rust
#[test]
fn test_format_includes_cwd() {
    let record = CommandRecord {
        command_id: "sess1:0".into(),
        session_id: "sess1".into(),
        command_line: Some("ls -la".into()),
        cwd: Some("/home/user/project".into()),
        started_at: 1000,
        ended_at: Some(1002),
        output_summary: "total 0\nfile.txt".into(),
        stream_offset: 0,
        stream_length: 100,
        exit_code: Some(0),
    };

    let formatted = format_command(&record);
    assert!(formatted.contains("/home/user/project $ ls -la"),
            "Formatted output should include cwd: {}", formatted);
}
```

**步骤3: 更新format_command函数确保包含cwd**

检查`crates/omnish-context/src/format_utils.rs`中的`format_command`函数是否包含cwd前缀。

**步骤4: 运行测试验证通过**

运行: `cargo test -p omnish-context test_format_includes_cwd`
预期: PASS

**步骤5: 提交**

```bash
git add crates/omnish-context/src/
git commit -m "feat: ensure cwd is included in formatted command output"
```

---

### Task 6: 集成测试和验证

**文件:**
- 创建: `crates/omnish-tracker/tests/integration_cwd.rs`
- 运行: `cargo test --workspace`

**步骤1: 创建集成测试**

```rust
// crates/omnish-tracker/tests/integration_cwd.rs
use omnish_tracker::{CommandTracker, osc133_detector::{Osc133Detector, Osc133Event, Osc133EventKind}};

#[test]
fn test_end_to_end_cwd_tracking() {
    // Simulate session starting in /home/user
    let mut tracker = CommandTracker::new("sess1".into(), Some("/home/user".into()));

    // First command in /home/user
    let mut detector = Osc133Detector::new();
    let events = detector.feed(b"\x1b]133;A\x07");
    for event in events {
        tracker.feed_osc133(event, 1000, 0);
    }

    let events = detector.feed(b"\x1b]133;B;ls -la;cwd:/home/user\x07");
    for event in events {
        tracker.feed_osc133(event, 1001, 50);
    }

    tracker.feed_input(b"\r", 1001);

    let events = detector.feed(b"\x1b]133;D;0\x07");
    let mut cmds1 = Vec::new();
    for event in events {
        cmds1.extend(tracker.feed_osc133(event, 1003, 100));
    }

    assert_eq!(cmds1.len(), 1);
    assert_eq!(cmds1[0].cwd.as_deref(), Some("/home/user"));

    // User changes directory
    let events = detector.feed(b"\x1b]133;A\x07");
    for event in events {
        tracker.feed_osc133(event, 2000, 100);
    }

    // Second command in /home/user/project
    let events = detector.feed(b"\x1b]133;B;make;cwd:/home/user/project\x07");
    for event in events {
        tracker.feed_osc133(event, 2001, 150);
    }

    tracker.feed_input(b"\r", 2001);

    let events = detector.feed(b"\x1b]133;D;0\x07");
    let mut cmds2 = Vec::new();
    for event in events {
        cmds2.extend(tracker.feed_osc133(event, 2003, 200));
    }

    assert_eq!(cmds2.len(), 1);
    assert_eq!(cmds2[0].cwd.as_deref(), Some("/home/user/project"));
    assert_ne!(cmds1[0].cwd, cmds2[0].cwd, "CWD should change between commands");
}
```

**步骤2: 运行集成测试**

运行: `cargo test -p omnish-tracker test_end_to_end_cwd_tracking`
预期: PASS

**步骤3: 运行完整测试套件**

运行: `cargo test --workspace`
预期: PASS (所有测试)

**步骤4: 提交**

```bash
git add crates/omnish-tracker/tests/integration_cwd.rs
git commit -m "test: add integration test for cwd tracking across commands"
```

---

计划完成并保存到 `docs/plans/2026-02-24-real-time-cwd-tracking.md`。两个执行选项：

**1. 子代理驱动（本次会话）** - 我为每个任务分派新的子代理，在任务之间进行代码审查，快速迭代

**2. 并行会话（独立）** - 在新工作树中打开新会话，使用executing-plans进行批量执行和检查点

**哪种方法？**