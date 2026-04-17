# Plugin Write Sandbox Design (Issue #176)

## Goal

Restrict external plugin subprocess write access to designated directories only, using Linux Landlock LSM.

## Design Decisions

1. **Landlock via `pre_exec`** - applied between fork and exec of plugin subprocess
2. **Write-only restriction** - plugins can read the entire filesystem, writes limited to:
   - `~/.omnish/data/{plugin_name}/` (plugin data directory, auto-created)
   - `/tmp` (temporary files)
3. **External plugins only** - built-in plugins share the daemon process; Landlock is process-level so cannot isolate them
4. **Refuse on unsupported kernel** - if Landlock is unavailable (kernel < 5.13 or disabled), refuse to load the plugin with an error log
5. **Convention-based data path** - plugins infer their data directory as `~/.omnish/data/{name}/`; no explicit passing needed

## Implementation

In `ExternalPlugin::spawn()`, before `Command::spawn()`:
1. Create `~/.omnish/data/{name}/` directory
2. Use `CommandExt::pre_exec` to apply Landlock ruleset:
   - `AccessFs::ReadFile | ReadDir` on `/`
   - `AccessFs::WriteFile | RemoveFile | RemoveDir | MakeDir | MakeReg | MakeSym` on `~/.omnish/data/{name}/` and `/tmp`
3. If Landlock creation fails in `pre_exec`, abort (process exits, spawn returns error)
4. If Landlock is compile-time unavailable, check at startup and refuse to load external plugins

## Crate

`landlock` - safe Rust bindings for Landlock LSM.

## Files Changed

| File | Change |
|------|--------|
| `crates/omnish-daemon/src/plugin.rs` | Add Landlock sandbox in `ExternalPlugin::spawn` via `pre_exec` |
| `crates/omnish-daemon/Cargo.toml` | Add `landlock` dependency |

## Not In Scope

- Network restriction
- Read restriction
- Built-in plugin sandboxing
- Configurable sandbox policy
