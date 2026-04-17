# Official Plugin Binary Design (Issue #198)

## Goal

Create a separate `omnish-plugin` binary that serves built-in plugins via JSON-RPC stdin/stdout, so official and third-party plugins use the same execution path (subprocess + JSON-RPC + Landlock sandbox).

## Plugin Taxonomy

| Type | Binary | Spawned by | Sandbox |
|------|--------|------------|---------|
| Internal | (in daemon process) | Direct call | None |
| Official | `omnish-plugin <name>` | `ExternalPlugin::spawn` | Landlock |
| Third-party | `~/.omnish/plugins/{name}/{name}` | `ExternalPlugin::spawn` | Landlock |

## Architecture

New crate `crates/omnish-plugin/` producing binary `omnish-plugin`.

```
omnish-plugin bash     →  serves BashTool via JSON-RPC
omnish-plugin <name>   →  serves named built-in plugin
```

The daemon spawns `omnish-plugin <name>` the same way it spawns third-party plugins - all go through `ExternalPlugin`, all get Landlock sandbox.

## JSON-RPC Server Loop

The `omnish-plugin` binary:
1. Reads plugin name from CLI arg
2. Instantiates the corresponding `Box<dyn Plugin>`
3. Enters a blocking stdin/stdout JSON-RPC loop:
   - `initialize` → returns `{ name, tools, plugin_type }`
   - `tool/execute` → calls `plugin.call_tool()`
   - `shutdown` → exits cleanly

## Files Changed

| File | Change |
|------|--------|
| `Cargo.toml` | Add `omnish-plugin` to workspace members |
| `crates/omnish-plugin/Cargo.toml` | New crate, depends on omnish-daemon for Plugin trait + BashTool |
| `crates/omnish-plugin/src/main.rs` | CLI arg + JSON-RPC server loop |
| `crates/omnish-daemon/src/plugin.rs` | `ExternalPlugin::spawn` accepts optional args |
| `crates/omnish-daemon/src/main.rs` | Register BashTool via `ExternalPlugin` spawning `omnish-plugin bash` |

## Not In Scope

- Client-side plugin mode
- Config for internal vs official per plugin
