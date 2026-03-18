# New User Onboarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Guide new users through omnish's key features via install.sh summary and a first-launch welcome message.

**Architecture:** Two-phase approach — install.sh prints post-install summary (directory, deployed clients, startup command), and the omnish client prints a welcome message on first launch. The `onboarded` flag is tracked in `client.toml` using `toml_edit` for format-preserving writes.

**Tech Stack:** Rust, toml_edit, bash (install.sh)

---

### Task 1: Add `onboarded` field to ClientConfig

**Files:**
- Modify: `crates/omnish-common/src/config.rs:28-53` (ClientConfig struct + Default impl)

- [ ] **Step 1: Add `onboarded` field to `ClientConfig`**

Add the field with `serde(default)` so existing config files without it default to `false`:

```rust
#[derive(Debug, Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub shell: ShellConfig,
    #[serde(default = "default_socket_path")]
    pub daemon_addr: String,
    #[serde(default = "default_true")]
    pub completion_enabled: bool,
    #[serde(default)]
    pub auto_update: bool,
    #[serde(default)]
    pub onboarded: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            shell: ShellConfig::default(),
            daemon_addr: default_socket_path(),
            completion_enabled: true,
            auto_update: false,
            onboarded: false,
        }
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p omnish-common`
Expected: compiles with no errors

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-common/src/config.rs
git commit -m "feat(config): add onboarded field to ClientConfig (#317)"
```

---

### Task 2: Add `toml_edit` dependency and onboarding helper module

**Files:**
- Modify: `crates/omnish-client/Cargo.toml` (add `toml_edit` dep)
- Create: `crates/omnish-client/src/onboarding.rs` (welcome message + mark_onboarded logic)
- Modify: `crates/omnish-client/src/main.rs:2` (add `mod onboarding;`)

- [ ] **Step 1: Add `toml_edit` dependency**

Add to `crates/omnish-client/Cargo.toml` under `[dependencies]`:

```toml
toml_edit = "0.22"
```

- [ ] **Step 2: Create `onboarding.rs` with welcome message and mark function**

Create `crates/omnish-client/src/onboarding.rs`:

```rust
use std::path::PathBuf;

/// Return the resolved client.toml path.
fn client_toml_path() -> PathBuf {
    std::env::var("OMNISH_CLIENT_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| omnish_common::config::omnish_dir().join("client.toml"))
}

/// Print the welcome message to stdout (before shell prompt appears).
pub fn print_welcome() {
    let config_path = client_toml_path();
    let config_display = config_path.display();
    let msg = format!(
        "\x1b[1mWelcome to omnish!\x1b[0m\n\
         \n\
         \x1b[36m  :  <query>\x1b[0m    Chat with AI about your terminal activity\n\
         \x1b[36m  :: <query>\x1b[0m    Resume your last conversation\n\
         \x1b[36m  Tab\x1b[0m           Accept ghost completion suggestion\n\
         \n\
         \x1b[2m  Config: {}\x1b[0m\n",
        config_display,
    );
    print!("{}", msg);
}

/// Write `onboarded = true` to client.toml, preserving existing formatting.
pub fn mark_onboarded() {
    let path = client_toml_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("cannot read client.toml for onboarding flag: {}", e);
            return;
        }
    };
    let mut doc = match content.parse::<toml_edit::DocumentMut>() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("cannot parse client.toml: {}", e);
            return;
        }
    };
    doc["onboarded"] = toml_edit::value(true);
    if let Err(e) = std::fs::write(&path, doc.to_string()) {
        tracing::warn!("cannot write onboarded flag to client.toml: {}", e);
    }
}
```

- [ ] **Step 3: Register the module in main.rs**

Add `mod onboarding;` near the top of `crates/omnish-client/src/main.rs` (after the other `mod` declarations, around line 15):

```rust
mod onboarding;
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p omnish-client`
Expected: compiles with no errors (may have unused warnings, that's fine)

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-client/Cargo.toml crates/omnish-client/src/onboarding.rs crates/omnish-client/src/main.rs
git commit -m "feat: add onboarding module with welcome message and mark_onboarded (#317)"
```

---

### Task 3: Print welcome message on first launch

**Files:**
- Modify: `crates/omnish-client/src/main.rs:326-345` (normal startup branch)

- [ ] **Step 1: Add welcome message print after proxy spawn**

In `main()`, after the proxy is spawned in the normal startup branch (around line 343-344, after `let proxy = PtyProxy::spawn_with_env(...)`) and before the session_id tuple is returned, print the welcome message if not onboarded:

```rust
        // Print welcome message for first-time users
        if !config.onboarded {
            onboarding::print_welcome();
        }
```

Insert this right after line 343 (`let proxy = PtyProxy::spawn_with_env(...)`) and before line 344 (`(session_id, proxy, osc133_hook_installed)`). This ensures it prints only on normal startup (not resume) and before the shell prompt appears.

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p omnish-client`
Expected: compiles with no errors

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "feat: print welcome message on first omnish launch (#317)"
```

---

### Task 4: Mark onboarded after first chat entry

**Files:**
- Modify: `crates/omnish-client/src/main.rs` (pass `onboarded` state to chat session entry points)
- Modify: `crates/omnish-client/src/chat_session.rs:157-167` (ChatSession::run signature and first-chat logic)

- [ ] **Step 1: Add `onboarded` flag to ChatSession::run**

In `chat_session.rs`, add an `onboarded: &AtomicBool` parameter to `ChatSession::run()`. At the start of the first successful chat interaction (after the user submits a non-empty query and before sending to daemon), check if `onboarded` is false, set it to true, and call `mark_onboarded()`:

In the `run()` method, after the `trimmed.is_empty()` check (around line 190), before pushing to scroll history:

```rust
            // Mark onboarded on first chat entry
            if !onboarded.load(Ordering::Relaxed) {
                onboarded.store(true, Ordering::Relaxed);
                crate::onboarding::mark_onboarded();
            }
```

- [ ] **Step 2: Update all call sites of `ChatSession::run` in main.rs**

Pass `&onboarded` (an `Arc<AtomicBool>` created from `config.onboarded`) to each `session.run(...)` call. There are 3 call sites in main.rs (search for `session.run(`).

In main.rs, create the `AtomicBool` after loading config:

```rust
let onboarded = Arc::new(AtomicBool::new(config.onboarded));
```

Then at each `session.run(...)` call, add `&onboarded` parameter.

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p omnish-client`
Expected: compiles with no errors

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-client/src/main.rs crates/omnish-client/src/chat_session.rs
git commit -m "feat: mark user as onboarded after first chat entry (#317)"
```

---

### Task 5: Update install.sh completion message

**Files:**
- Modify: `install.sh:706-713` (end of script)

- [ ] **Step 1: Replace the completion message**

Replace the current ending (lines 706-713) with:

```bash
echo ""
if [[ "$UPGRADE" == true ]] || { [[ -n "${OLD_VERSION:-}" ]] && [[ "$OLD_VERSION" != "${VERSION#v}" ]]; }; then
    info "Upgrade complete! (v${OLD_VERSION} → ${VERSION})"
else
    info "Installation complete! (omnish ${VERSION})"
    info "Installed to: ${OMNISH_DIR}"
    if [[ ${#DEPLOYED_CLIENTS[@]} -gt 0 ]]; then
        info "Deployed clients: $(IFS=', '; echo "${DEPLOYED_CLIENTS[*]}")"
    fi
    echo ""
    if echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
        info "Run 'omnish' to get started."
    else
        info "Run '${BIN_DIR}/omnish' to get started."
    fi
fi
```

Note: `DEPLOYED_CLIENTS` array is only set in the TCP deployment section. For non-TCP installs, it won't exist, so we need to guard with `${#DEPLOYED_CLIENTS[@]}` which will be 0 if unset (due to `DEPLOYED_CLIENTS=()` being inside the TCP block). Initialize `DEPLOYED_CLIENTS=()` earlier in the script (before the TCP block) to avoid unbound variable errors.

- [ ] **Step 2: Initialize DEPLOYED_CLIENTS at script top level**

Add `DEPLOYED_CLIENTS=()` near the top of the script, after the variable declarations (around line 37, after `BIN_DIR=`):

```bash
DEPLOYED_CLIENTS=()
```

- [ ] **Step 3: Verify install.sh syntax**

Run: `bash -n install.sh`
Expected: no syntax errors

- [ ] **Step 4: Commit**

```bash
git add install.sh
git commit -m "feat: improve install.sh completion message with directory and client info (#317)"
```

---

### Task 6: Build and manual verification

- [ ] **Step 1: Run full workspace build**

Run: `cargo build -p omnish-client`
Expected: builds successfully

- [ ] **Step 2: Run workspace tests**

Run: `cargo test -p omnish-client -p omnish-common`
Expected: all tests pass

- [ ] **Step 3: Commit all remaining changes (if any)**

```bash
git add -A
git commit -m "feat: new user onboarding (#317)"
```
