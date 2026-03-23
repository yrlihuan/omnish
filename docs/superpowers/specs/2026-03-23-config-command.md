# /config Command — Daemon Configuration via Menu Widget

Interactive `/config` command in chat mode that lets users view and modify daemon configuration through the multi-level menu widget. Changes are persisted to `daemon.toml` via protocol messages; a daemon restart is required for changes to take effect.

## Scope

Initial implementation covers:
- Proxy settings (HTTP proxy, no_proxy)
- LLM use_cases (completion, analysis, chat backend selection)

Other daemon config sections (tasks, context, plugins, sandbox) can be added later by extending the schema file.

## Schema File

**`crates/omnish-daemon/src/config_schema.toml`** — embedded via `include_str!()`.

Defines the mapping between menu items and daemon.toml keys:

```toml
[[items]]
path = "proxy.http_proxy"
label = "HTTP proxy"
kind = "text"
toml_key = "proxy"

[[items]]
path = "proxy.no_proxy"
label = "No proxy"
kind = "text"
toml_key = "no_proxy"

[[items]]
path = "llm.use_cases.completion"
label = "Completion backend"
kind = "select"
toml_key = "llm.use_cases.completion"
options_from = "llm.backends"

[[items]]
path = "llm.use_cases.analysis"
label = "Analysis backend"
kind = "select"
toml_key = "llm.use_cases.analysis"
options_from = "llm.backends"

[[items]]
path = "llm.use_cases.chat"
label = "Chat backend"
kind = "select"
toml_key = "llm.use_cases.chat"
options_from = "llm.backends"
```

**Fields:**
- `path` — dot-separated identifier. Determines menu hierarchy: `proxy.http_proxy` renders as Submenu "Proxy" containing item "HTTP proxy". Also used as the key in `ConfigChange` to identify which item changed.
- `label` — display name in the menu.
- `kind` — `text` (TextInput), `select` (Select), or `toggle` (Toggle).
- `toml_key` — the actual key path in `daemon.toml` for reading/writing. May differ from `path` (e.g., `proxy.http_proxy` maps to top-level key `proxy`).
- `options_from` — (select only) config field whose HashMap keys provide the option list at runtime. `llm.backends` extracts keys from `config.llm.backends`. If the backend list is empty, the select item is still shown with an empty options list (user sees a picker with no choices).

**Submenu label generation:** path segments are auto-capitalized with underscores replaced by spaces (`use_cases` → "Use Cases").

## Protocol Messages

Four new variants in `omnish-protocol/src/message.rs`:

```rust
ConfigQuery,

ConfigResponse {
    items: Vec<ConfigItem>,
},

ConfigUpdate {
    changes: Vec<ConfigChange>,
},

ConfigUpdateResult {
    ok: bool,
    error: Option<String>,
},
```

**Shared types** (in `omnish-protocol`):

```rust
pub struct ConfigItem {
    pub path: String,
    pub label: String,
    pub kind: ConfigItemKind,
}

pub enum ConfigItemKind {
    Toggle { value: bool },
    Select { options: Vec<String>, selected: usize },
    TextInput { value: String },
}

pub struct ConfigChange {
    pub path: String,
    pub value: String,
}
```

## Daemon: config_schema.rs

**`crates/omnish-daemon/src/config_schema.rs`** — new module.

Parses the embedded schema and provides two functions:

### build_config_items

```rust
pub fn build_config_items(config: &DaemonConfig) -> Vec<ConfigItem>
```

**Value reading via Serialize:** DaemonConfig (and all sub-types) derive `Serialize`. At query time, serialize config to `toml::Value` in memory, then use generic dot-path lookup to extract current values. This avoids per-field match statements — adding a new schema item only requires specifying the correct `toml_key`, no Rust code changes.

```rust
let config_as_value = toml::Value::try_from(config).unwrap();
let current = resolve_value(&config_as_value, &schema_item.toml_key);
let options = resolve_options(&config_as_value, &schema_item.options_from);
```

Generic helpers:
- `resolve_value(doc, path)` — traverses dot-separated path, returns string representation.
- `resolve_options(doc, path)` — traverses to a TOML table, returns its keys as `Vec<String>`.

For select items where the current value is not in the options list, it is appended to options and marked as selected.

### apply_config_changes

```rust
pub fn apply_config_changes(config_path: &Path, changes: &[ConfigChange]) -> Result<()>
```

Maps each `ConfigChange.path` back to `toml_key` via the schema, then writes to daemon.toml using `toml_edit` for format-preserving edits. Only the changed keys are modified; existing comments, formatting, and unrelated config are preserved.

Handles dotted `toml_key` paths (e.g., `llm.use_cases.completion`) by traversing/creating nested TOML tables. The value type is determined by the schema item's `kind`: `toggle` → TOML boolean, `text`/`select` → TOML string.

If `daemon.toml` does not exist, the function creates it before writing.

Returns error if any write fails.

## Client: /config Command Flow

In `chat_session.rs`:

```
1. Send ConfigQuery to daemon via RPC
2. Receive ConfigResponse { items }
3. Build MenuItem tree from flat items (group by path prefix)
   - Also build a path_map: HashMap<display_label_path, schema_path> for reverse lookup
4. Call run_menu("Config", &mut tree) → MenuResult
5. If Done(changes) and non-empty:
   a. Convert each MenuChange to ConfigChange using path_map
      (MenuChange.path uses display labels from build_path();
       path_map translates back to the schema path used by daemon)
   b. Send ConfigUpdate { changes } to daemon
   c. Receive ConfigUpdateResult
   d. Display "Config saved. Restart daemon to apply." or error message
6. If Cancelled or no changes: do nothing
```

### Tree building

```rust
fn build_menu_tree(items: &[ConfigItem]) -> (Vec<MenuItem>, HashMap<String, String>)
```

Groups `ConfigItem` entries by path prefix segments to create nested `MenuItem::Submenu` nodes. Leaf items become `MenuItem::Select`, `MenuItem::Toggle`, or `MenuItem::TextInput` based on `ConfigItemKind`.

Returns the menu tree and a path_map that maps display-label paths (produced by `menu::build_path()`, e.g. `"Proxy.HTTP proxy"`) back to schema paths (e.g. `"proxy.http_proxy"`).

Submenu labels: first letter capitalized, underscores replaced with spaces.

## Menu Rendering Example

```
Config
────────────────────────────────
> Proxy                        >
  LLM                          >
────────────────────────────────
↑↓ move  Enter select  ESC back  ^C quit

Config > Proxy
────────────────────────────────
> HTTP proxy    http://proxy:8080
  No proxy      localhost,127.0.0.1
────────────────────────────────
↑↓ move  Enter select  ESC back  ^C quit

Config > LLM > Use Cases
────────────────────────────────
> Completion backend       claude
  Analysis backend         claude
  Chat backend             claude
────────────────────────────────
↑↓ move  Enter select  ESC back  ^C quit
```

## Data Flow

```
Client                              Daemon
  │                                   │
  ├── ConfigQuery ─────────────────►  │
  │                                   ├── parse schema (include_str!)
  │                                   ├── read current DaemonConfig
  │                                   ├── build_config_items()
  │  ◄──────────── ConfigResponse ──  │
  │                                   │
  ├── build_menu_tree()               │
  ├── run_menu("Config", tree)        │
  │   (user navigates, edits)         │
  │                                   │
  ├── ConfigUpdate { changes } ────►  │
  │                                   ├── apply_config_changes()
  │                                   │   (set_toml_value for each)
  │  ◄────────── ConfigUpdateResult ─ │
  │                                   │
  ├── "Config saved. Restart daemon   │
  │    to apply."                     │
```

## File Changes

| File | Action |
|------|--------|
| `crates/omnish-daemon/src/config_schema.toml` | Create — schema mapping |
| `crates/omnish-daemon/src/config_schema.rs` | Create — parse schema, build items, apply changes |
| `crates/omnish-daemon/src/main.rs` | Add `mod config_schema;` + handle ConfigQuery/ConfigUpdate |
| `crates/omnish-protocol/src/message.rs` | Add 4 message variants + ConfigItem/ConfigItemKind/ConfigChange types; bump PROTOCOL_VERSION to 9, EXPECTED_VARIANT_COUNT to 28 |
| `crates/omnish-common/src/config.rs` | Add `Serialize` derive to DaemonConfig and all sub-types |
| `crates/omnish-common/src/config_edit.rs` | Add `set_toml_value_nested()` for dotted key paths |
| `crates/omnish-client/src/chat_session.rs` | Add `/config` command + `build_menu_tree()` |
