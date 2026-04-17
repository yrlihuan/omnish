# New User Onboarding - Design Spec

**Issue:** #317
**Date:** 2026-03-18

## Overview

Two-phase onboarding for new omnish users:
1. `install.sh` prints a brief summary with installation directory, deployed clients, and how to start
2. First `omnish` client launch shows a welcome message with key features

## Phase 1: install.sh Completion Message

### Current behavior

Prints only `"Installation complete! (omnish vX.Y.Z)"` or `"Upgrade complete!"`.

### New behavior

**Fresh install:**
```
[omnish] Installed to: ~/.omnish
[omnish] Deployed clients: user@host1, user@host2
[omnish]
[omnish] Run 'omnish' to get started.
```

- If no clients were deployed, skip the "Deployed clients" line.
- If `$BIN_DIR` is not in `$PATH`, use the full path: `Run '~/.omnish/bin/omnish' to get started.`
- **Upgrade mode**: no change, keep existing `"Upgrade complete! (vX.Y.Z)"`.

## Phase 2: Client Welcome Message

### Trigger condition

`client.toml` does not contain `onboarded = true`.

### Display timing

After `omnish` client starts, before the shell prompt appears. Printed directly to the terminal (stdout), similar to motd.

### Content

```
Welcome to omnish!

  :  <query>    Chat with AI about your terminal activity
  :: <query>    Resume your last conversation
  Tab            Accept ghost completion suggestion

  Config: ~/.omnish/client.toml

```

### Onboarded flag

- **When to set:** After the user enters chat mode for the first time (`:` prefix intercepted).
- **How to set:** Use `toml_edit` to read `client.toml`, set `onboarded = true` at the top level, write back. This preserves existing comments and formatting.
- **Fallback:** If `client.toml` doesn't exist or write fails, log a warning and continue (non-fatal).

## File Changes

| File | Change |
|------|--------|
| `install.sh` | Modify completion message at the end |
| `crates/omnish-common/src/config.rs` | Add `onboarded: bool` field to `ClientConfig` |
| `crates/omnish-client/Cargo.toml` | Add `toml_edit` dependency |
| `crates/omnish-client/src/main.rs` | Check `onboarded` on startup, print welcome message |
| `crates/omnish-client/src/chat_session.rs` | After first chat entry, write `onboarded = true` via `toml_edit` |

## Non-goals

- No interactive tutorial or step-by-step walkthrough.
- No progressive/contextual hints during usage.
- No daemon-side state tracking.
