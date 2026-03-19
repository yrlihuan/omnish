# /debug Command Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `/debug context` and `/debug template` commands to omnish chat mode (debug builds only), with `> file` redirect support, using an extensible client-side command dispatch system.

**Architecture:** Client parses `/` commands from chat input before sending to daemon. `/debug template` is pure client-side. `/debug context` reuses existing Request/Response with `__debug:` query prefix so daemon returns raw data instead of calling LLM. Redirect (`> path`) is stripped client-side.

**Tech Stack:** Rust, `#[cfg(debug_assertions)]` for debug-only gating

---

### Task 1: Extract prompt template into shared function (omnish-llm)

Both Anthropic and OpenAI backends duplicate the same prompt template. Extract it into a shared function that both backends call, and that `/debug template` can also use.

**Files:**
- Create: `crates/omnish-llm/src/template.rs`
- Modify: `crates/omnish-llm/src/lib.rs:1-5`
- Modify: `crates/omnish-llm/src/anthropic.rs:15-25`
- Modify: `crates/omnish-llm/src/openai_compat.rs:16-26`

**Step 1: Create template module with function and test**

Create `crates/omnish-llm/src/template.rs`:

```rust
/// Build the user-content prompt sent to the LLM.
///
/// When `query` is Some, the template includes the user question.
/// When `query` is None, the template asks the LLM to analyze errors.
pub fn build_user_content(context: &str, query: Option<&str>) -> String {
    if let Some(q) = query {
        format!(
            "Here is the terminal session context:\n\n```\n{}\n```\n\nUser question: {}",
            context, q
        )
    } else {
        format!(
            "Analyze this terminal session output and explain any errors or issues:\n\n```\n{}\n```",
            context
        )
    }
}

/// Return the prompt template with `{context}` and `{query}` placeholders.
pub fn prompt_template(has_query: bool) -> &'static str {
    if has_query {
        "Here is the terminal session context:\n\n```\n{context}\n```\n\nUser question: {query}"
    } else {
        "Analyze this terminal session output and explain any errors or issues:\n\n```\n{context}\n```"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_user_content_with_query() {
        let result = build_user_content("$ ls\nfoo bar", Some("what files are here?"));
        assert!(result.contains("$ ls\nfoo bar"));
        assert!(result.contains("User question: what files are here?"));
    }

    #[test]
    fn test_build_user_content_without_query() {
        let result = build_user_content("$ exit 1", None);
        assert!(result.contains("$ exit 1"));
        assert!(result.contains("Analyze this terminal session"));
        assert!(!result.contains("User question"));
    }

    #[test]
    fn test_prompt_template_with_query() {
        let t = prompt_template(true);
        assert!(t.contains("{context}"));
        assert!(t.contains("{query}"));
    }

    #[test]
    fn test_prompt_template_without_query() {
        let t = prompt_template(false);
        assert!(t.contains("{context}"));
        assert!(!t.contains("{query}"));
    }
}
```

**Step 2: Register module in lib.rs**

Add `pub mod template;` to `crates/omnish-llm/src/lib.rs`.

**Step 3: Update Anthropic backend to use shared function**

In `crates/omnish-llm/src/anthropic.rs`, replace lines 15-25:

```rust
        let user_content = crate::template::build_user_content(
            &req.context,
            req.query.as_deref(),
        );
```

**Step 4: Update OpenAI compat backend identically**

In `crates/omnish-llm/src/openai_compat.rs`, replace lines 16-26 with the same call.

**Step 5: Run tests and commit**

Run: `cargo test -p omnish-llm`
Expected: all pass

```bash
git add crates/omnish-llm/
git commit -m "refactor(llm): extract prompt template into shared module"
```

---

### Task 2: Daemon handles `__debug:` query prefix

Add debug-gated handling in the daemon server so `__debug:context` queries return raw session context instead of calling the LLM.

**Files:**
- Modify: `crates/omnish-daemon/src/server.rs:69-93`
- Test: `crates/omnish-daemon/tests/daemon_test.rs`

**Step 1: Add debug handler in server.rs**

In `handle_connection`, modify the `Message::Request` arm (lines 69-93). Before the existing LLM dispatch, add a debug-gated check:

```rust
            Message::Request(req) => {
                #[cfg(debug_assertions)]
                if req.query.starts_with("__debug:") {
                    let content = handle_debug_request(&req, &mgr).await;
                    let resp = Message::Response(Response {
                        request_id: req.request_id,
                        content,
                        is_streaming: false,
                        is_final: true,
                    });
                    conn.send(&resp).await?;
                    continue;
                }

                // existing LLM handling below...
```

Add the handler function:

```rust
#[cfg(debug_assertions)]
async fn handle_debug_request(req: &Request, mgr: &SessionManager) -> String {
    let sub = req.query.strip_prefix("__debug:").unwrap_or("");
    match sub {
        "context" => {
            match mgr.get_session_context(&req.session_id).await {
                Ok(ctx) => ctx,
                Err(e) => format!("Error: {}", e),
            }
        }
        other => format!("Unknown debug subcommand: {}", other),
    }
}
```

**Step 2: Add test**

In `crates/omnish-daemon/tests/daemon_test.rs`, add:

```rust
#[cfg(debug_assertions)]
#[tokio::test]
async fn test_debug_context_request() {
    // Setup: register session, write some output data, then send __debug:context Request
    // Assert: Response contains the session context (output data, ANSI stripped)
}
```

The test should:
1. Create a SessionManager with a temp dir
2. Register a session
3. Write some IoData (direction=1, output) via `mgr.write_io`
4. Call `handle_debug_request` directly (or set up a full server+client loop if feasible)
5. Assert the response content matches expected stripped output

**Step 3: Run tests and commit**

Run: `cargo test -p omnish-daemon`
Expected: all pass

```bash
git add crates/omnish-daemon/
git commit -m "feat(daemon): handle __debug: query prefix in debug builds"
```

---

### Task 3: Client-side command dispatch and /debug handling

Add the extensible command dispatch system in the client. Parse `/` commands, strip `> path` redirect, and handle `/debug template` locally or `/debug context` via daemon.

**Files:**
- Create: `crates/omnish-client/src/command.rs`
- Modify: `crates/omnish-client/src/main.rs:159-168` (Chat arm)
- Modify: `crates/omnish-client/src/main.rs:502-550` (handle_omnish_query)

**Step 1: Create command.rs with parsing and dispatch**

Create `crates/omnish-client/src/command.rs`:

```rust
/// Result of parsing a chat message for `/` commands.
pub enum ChatAction {
    /// A `/` command was recognized. Contains the result text and optional redirect path.
    Command { result: String, redirect: Option<String> },
    /// Not a command — forward as normal LLM query.
    LlmQuery(String),
    /// A `/` command that needs daemon data. Contains the debug query to send and optional redirect.
    DaemonDebug { query: String, redirect: Option<String> },
}

/// Parse redirect suffix: "some text > /path/to/file" → ("some text", Some("/path/to/file"))
fn parse_redirect(input: &str) -> (&str, Option<&str>) {
    // Find last " > " that's followed by a path (not inside the command)
    if let Some(pos) = input.rfind(" > ") {
        let path = input[pos + 3..].trim();
        if !path.is_empty() {
            return (&input[..pos], Some(path));
        }
    }
    (input, None)
}

/// Dispatch a chat message. Returns ChatAction describing what to do.
#[cfg(debug_assertions)]
pub fn dispatch(msg: &str) -> ChatAction {
    if !msg.starts_with('/') {
        return ChatAction::LlmQuery(msg.to_string());
    }

    let (cmd_str, redirect) = parse_redirect(msg);
    let redirect = redirect.map(|s| s.to_string());
    let parts: Vec<&str> = cmd_str.split_whitespace().collect();

    match parts.first().map(|s| *s) {
        Some("/debug") => handle_debug(&parts[1..], redirect),
        _ => ChatAction::LlmQuery(msg.to_string()), // unknown /cmd → LLM
    }
}

/// In release builds, all chat messages go to LLM.
#[cfg(not(debug_assertions))]
pub fn dispatch(msg: &str) -> ChatAction {
    ChatAction::LlmQuery(msg.to_string())
}

#[cfg(debug_assertions)]
fn handle_debug(args: &[&str], redirect: Option<String>) -> ChatAction {
    match args.first().map(|s| *s) {
        Some("context") => ChatAction::DaemonDebug {
            query: "__debug:context".to_string(),
            redirect,
        },
        Some("template") => {
            let result = omnish_llm::template::prompt_template(true).to_string();
            ChatAction::Command { result, redirect }
        }
        Some(other) => ChatAction::Command {
            result: format!("Unknown debug subcommand: {}", other),
            redirect: None,
        },
        None => ChatAction::Command {
            result: "Usage: /debug <context|template> [> file.txt]".to_string(),
            redirect: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_redirect() {
        assert_eq!(parse_redirect("/debug context"), ("/debug context", None));
        assert_eq!(
            parse_redirect("/debug context > /tmp/out.txt"),
            ("/debug context", Some("/tmp/out.txt"))
        );
    }

    #[test]
    fn test_non_command_is_llm_query() {
        match dispatch("what is this error?") {
            ChatAction::LlmQuery(q) => assert_eq!(q, "what is this error?"),
            _ => panic!("expected LlmQuery"),
        }
    }

    #[test]
    fn test_debug_context_dispatches_to_daemon() {
        match dispatch("/debug context") {
            ChatAction::DaemonDebug { query, redirect } => {
                assert_eq!(query, "__debug:context");
                assert!(redirect.is_none());
            }
            _ => panic!("expected DaemonDebug"),
        }
    }

    #[test]
    fn test_debug_context_with_redirect() {
        match dispatch("/debug context > /tmp/ctx.txt") {
            ChatAction::DaemonDebug { query, redirect } => {
                assert_eq!(query, "__debug:context");
                assert_eq!(redirect.as_deref(), Some("/tmp/ctx.txt"));
            }
            _ => panic!("expected DaemonDebug"),
        }
    }

    #[test]
    fn test_debug_template_is_local() {
        match dispatch("/debug template") {
            ChatAction::Command { result, redirect } => {
                assert!(result.contains("{context}"));
                assert!(redirect.is_none());
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_debug_no_args_shows_usage() {
        match dispatch("/debug") {
            ChatAction::Command { result, .. } => {
                assert!(result.contains("Usage"));
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_unknown_slash_command_falls_through() {
        match dispatch("/unknown foo") {
            ChatAction::LlmQuery(q) => assert_eq!(q, "/unknown foo"),
            _ => panic!("expected LlmQuery"),
        }
    }
}
```

**Step 2: Run tests**

Run: `cargo test -p omnish-client`
Expected: all pass

**Step 3: Integrate into main.rs**

Add `mod command;` to top of `main.rs`.

Modify the `InterceptAction::Chat(msg)` arm in the main loop (lines 159-168) to use the new dispatch:

```rust
                    InterceptAction::Chat(msg) => {
                        match command::dispatch(&msg) {
                            command::ChatAction::Command { result, redirect } => {
                                handle_command_result(&result, redirect.as_deref(), &proxy);
                            }
                            command::ChatAction::DaemonDebug { query, redirect } => {
                                if let Some(ref conn) = daemon_conn {
                                    handle_debug_query(&query, &session_id, conn, &proxy, redirect.as_deref()).await;
                                } else {
                                    let err = display::render_error("Daemon not connected");
                                    nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                                    proxy.write_all(b"\r").ok();
                                }
                            }
                            command::ChatAction::LlmQuery(query) => {
                                if let Some(ref conn) = daemon_conn {
                                    handle_omnish_query(&query, &session_id, conn, &proxy).await;
                                } else {
                                    let err = display::render_error("Daemon not connected");
                                    nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                                    proxy.write_all(b"\r").ok();
                                }
                            }
                        }
                    }
```

**Step 4: Add helper functions in main.rs**

```rust
/// Display a command result or write to file if redirected.
fn handle_command_result(content: &str, redirect: Option<&str>, proxy: &PtyProxy) {
    if let Some(path) = redirect {
        match std::fs::write(path, content) {
            Ok(_) => {
                let msg = display::render_response(&format!("Written to {}", path));
                nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
            }
            Err(e) => {
                let err = display::render_error(&format!("Write failed: {}", e));
                nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
            }
        }
    } else {
        let output = display::render_response(content);
        nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();
    }
    proxy.write_all(b"\r").ok();
}

/// Send a debug query to daemon and display/redirect the result.
async fn handle_debug_query(
    query: &str,
    session_id: &str,
    conn: &Box<dyn Connection>,
    proxy: &PtyProxy,
    redirect: Option<&str>,
) {
    let request_id = Uuid::new_v4().to_string()[..8].to_string();
    let request = Message::Request(Request {
        request_id: request_id.clone(),
        session_id: session_id.to_string(),
        query: query.to_string(),
        scope: RequestScope::CurrentSession,
    });

    if conn.send(&request).await.is_err() {
        let err = display::render_error("Failed to send request");
        nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
        proxy.write_all(b"\r").ok();
        return;
    }

    match conn.recv().await {
        Ok(Message::Response(resp)) if resp.request_id == request_id => {
            handle_command_result(&resp.content, redirect, proxy);
        }
        _ => {
            let err = display::render_error("Failed to receive debug response");
            nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
            proxy.write_all(b"\r").ok();
        }
    }
}
```

**Step 5: Build and test**

Run: `cargo build -p omnish-client && cargo test -p omnish-client`
Expected: build succeeds, all tests pass

```bash
git add crates/omnish-client/
git commit -m "feat(client): add extensible /command dispatch with /debug support"
```

---

### Task 4: Verify end-to-end and commit

**Step 1: Run full workspace tests**

Run: `cargo test --workspace`
Expected: all tests pass

**Step 2: Manual smoke test (optional)**

1. `cargo run -p omnish-daemon` in one terminal
2. `cargo run -p omnish-client` in another
3. Type some commands (`ls`, `echo hello`)
4. Type `:/debug context` — should show ANSI-stripped session output
5. Type `:/debug template` — should show template with `{context}` placeholder
6. Type `:/debug context > /tmp/test_ctx.txt` — should write to file
7. Verify with `cat /tmp/test_ctx.txt`
