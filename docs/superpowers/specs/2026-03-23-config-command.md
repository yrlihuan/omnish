# /config Command - Daemon Configuration via Menu Widget

Interactive `/config` command in chat mode that lets users view and modify daemon configuration through the multi-level menu widget. Changes are persisted to `daemon.toml` via protocol messages; a daemon restart is required for changes to take effect.

## Scope

Initial implementation covers:
- Proxy settings (HTTP proxy, no_proxy)
- LLM use_cases (completion, analysis, chat backend selection)
- LLM backends (view existing, add new)

Other daemon config sections (tasks, context, plugins, sandbox) can be added later by extending the schema file.

## Schema File

**`crates/omnish-daemon/src/config_schema.toml`** - embedded via `include_str!()`.

Defines the mapping between menu items and daemon.toml keys:

```toml
# ── Proxy ──────────────────────────────────────────────
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

# ── LLM use cases ─────────────────────────────────────
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

# ── Add new LLM backend ───────────────────────────────
# handler on submenu node: all child changes are grouped
# and passed to the handler function on ESC back
[[items]]
path = "llm.backends.__new__"
label = "Add backend"
kind = "submenu"
handler = "add_backend"

[[items]]
path = "llm.backends.__new__.name"
label = "Name"
kind = "text"

[[items]]
path = "llm.backends.__new__.backend_type"
label = "Backend type"
kind = "select"
options = ["anthropic", "openai_compat"]

[[items]]
path = "llm.backends.__new__.model"
label = "Model"
kind = "text"

[[items]]
path = "llm.backends.__new__.api_key_cmd"
label = "API key command"
kind = "text"

[[items]]
path = "llm.backends.__new__.base_url"
label = "Base URL"
kind = "text"
```

**Fields:**
- `path` - dot-separated identifier. Determines menu hierarchy: `proxy.http_proxy` renders as Submenu "Proxy" containing item "HTTP proxy". Also used as the key in `ConfigChange` to identify which item changed.
- `label` - display name in the menu.
- `kind` - `text` (TextInput), `select` (Select), `toggle` (Toggle), or `submenu` (explicit submenu node with handler).
- `toml_key` - (leaf items without handler) the actual key path in `daemon.toml` for reading/writing. May differ from `path` (e.g., `proxy.http_proxy` maps to top-level key `proxy`). Not needed for items under a handler submenu.
- `options_from` - (select only) references a TOML table in the serialized config. Resolved at runtime via generic path lookup: `llm.backends` extracts keys from the backends table as option values. If the table is empty, the select shows an empty option list.
- `options` - (select only) static option list defined in the schema. Used when choices are fixed (e.g., backend_type).
- `handler` - (submenu only) name of a Rust function that handles all changes from child items as a group. When present, child items do not need `toml_key` - the handler is fully responsible for reading initial values and writing changes.

**Handler vs generic behavior:**
- Items **without** `handler`: generic TOML read (via `resolve_value` on serialized config) and write (via `set_toml_value_nested`). Adding new items only requires schema changes. Changes are collected in memory and written when the user exits the top-level menu.
- Submenu **with** `handler`: all child changes are grouped and sent to the daemon **immediately when the user presses ESC to leave the handler submenu**. The daemon executes the handler, reloads config, and the client re-fetches items via `ConfigQuery` to refresh the menu (e.g., a newly added backend appears in use_cases select options). Adding a new handler requires Rust code.

**Submenu label generation:** path segments are auto-capitalized with underscores replaced by spaces (`use_cases` → "Use Cases"). The special segment `__new__` renders as "Add Backend" (uses the submenu's `label` field).

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

**`crates/omnish-daemon/src/config_schema.rs`** - new module.

Parses the embedded schema and provides two functions:

### build_config_items

```rust
pub fn build_config_items(config: &DaemonConfig) -> Vec<ConfigItem>
```

**Value reading via Serialize:** DaemonConfig (and all sub-types) derive `Serialize`. At query time, serialize config to `toml::Value` in memory, then use generic dot-path lookup to extract current values. This avoids per-field match statements - adding a new schema item only requires specifying the correct `toml_key`, no Rust code changes.

```rust
let config_as_value = toml::Value::try_from(config).unwrap();
let current = resolve_value(&config_as_value, &schema_item.toml_key);
let options = resolve_options(&config_as_value, &schema_item.options_from);
```

Generic helpers:
- `resolve_value(doc, path)` - traverses dot-separated path, returns string representation.
- `resolve_options(doc, path)` - traverses to a TOML table, returns its keys as `Vec<String>`.

For select items where the current value is not in the options list, it is appended to options and marked as selected.

### apply_config_changes

```rust
pub fn apply_config_changes(config_path: &Path, changes: &[ConfigChange]) -> Result<()>
```

Splits changes into two groups based on the schema:

**1. Generic changes** (items without handler): maps `ConfigChange.path` back to `toml_key` via the schema, then writes to daemon.toml using `toml_edit` for format-preserving edits. Only the changed keys are modified. Handles dotted `toml_key` paths by traversing/creating nested TOML tables. Value type is determined by the schema item's `kind`: `toggle` → TOML boolean, `text`/`select` → TOML string.

**2. Handler changes** (items under a handler submenu): groups changes by their parent handler name, then dispatches to the corresponding Rust function:

```rust
match handler_name {
    "add_backend" => handle_add_backend(config_path, &grouped_changes),
    _ => Err(anyhow!("unknown handler: {}", handler_name)),
}
```

**`add_backend` handler:** extracts `name` field to determine table key, validates required fields, writes `[llm.backends.<name>]` section to daemon.toml via `toml_edit`.

If `daemon.toml` does not exist, the function creates it before writing.

Returns error if any write fails.

## Client: /config Command Flow

In `chat_session.rs`:

```
1. Send ConfigQuery to daemon via RPC
2. Receive ConfigResponse { items }
3. Build MenuItem tree from flat items (group by path prefix)
   - Also build a path_map: HashMap<display_label_path, schema_path> for reverse lookup
   - Track which paths belong to handler submenus
4. Call run_menu("Config", &mut tree) → MenuResult
5. If Done(changes) and non-empty:
   a. Convert each MenuChange to ConfigChange using path_map
   b. Send ConfigUpdate { changes } to daemon (generic items only;
      handler items were already sent in step 4)
   c. Receive ConfigUpdateResult
   d. Display "Config saved. Restart daemon to apply." or error message
6. If Cancelled or no changes: do nothing
```

### Handler submenu callback

The menu widget needs a way to notify the client when the user leaves a handler submenu. Two approaches:

**A. Callback-based:** `run_menu` accepts an optional callback that fires when leaving a handler submenu. The callback sends changes to daemon, re-fetches config, and rebuilds the affected subtree.

**B. Event-based:** `run_menu` returns an intermediate event (`MenuEvent::HandlerExit { changes }`) instead of only returning at widget close. The caller processes the event and re-enters the menu.

Approach A keeps the menu loop simple. The callback signature:

```rust
pub fn run_menu(
    title: &str,
    items: &mut [MenuItem],
    on_handler_exit: Option<&mut dyn FnMut(&str, Vec<MenuChange>) -> Vec<MenuItem>>,
) -> MenuResult
```

When the user ESC-exits a handler submenu, `on_handler_exit(handler_name, changes)` is called. The callback:
1. Sends `ConfigUpdate { changes }` to daemon
2. Waits for `ConfigUpdateResult`
3. Sends `ConfigQuery`, receives fresh `ConfigResponse`
4. Returns new `Vec<MenuItem>` to replace the current menu tree

The menu widget then refreshes its view with the updated items, navigating back to the parent level.

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

Config > LLM
────────────────────────────────
> Use Cases                    >
  Backends                     >
────────────────────────────────
↑↓ move  Enter select  ESC back  ^C quit

Config > LLM > Backends > New
────────────────────────────────
> Name                    (empty)
  Backend type          anthropic
  Model                   (empty)
  API key command         (empty)
  Base URL                (empty)
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
  │   ┌─ on handler submenu ESC ──┐   │
  │   │  ConfigUpdate (handler) ──┼─► │
  │   │                           │   ├── handler writes daemon.toml
  │   │                           │   ├── reload DaemonConfig
  │   │  ◄── ConfigUpdateResult ──┼── │
  │   │  ConfigQuery ─────────────┼─► │
  │   │  ◄── ConfigResponse ──────┼── │
  │   │  rebuild menu tree        │   │
  │   └───────────────────────────┘   │
  │                                   │
  │   (user continues editing...)     │
  │                                   │
  ├── ConfigUpdate (generic) ──────►  │
  │                                   ├── set_toml_value_nested for each
  │  ◄────────── ConfigUpdateResult ─ │
  │                                   │
  ├── "Config saved. Restart daemon   │
  │    to apply."                     │
```

## File Changes

| File | Action |
|------|--------|
| `crates/omnish-daemon/src/config_schema.toml` | Create - schema mapping |
| `crates/omnish-daemon/src/config_schema.rs` | Create - parse schema, build items, apply changes |
| `crates/omnish-daemon/src/main.rs` | Add `mod config_schema;` + handle ConfigQuery/ConfigUpdate |
| `crates/omnish-protocol/src/message.rs` | Add 4 message variants + ConfigItem/ConfigItemKind/ConfigChange types; bump PROTOCOL_VERSION to 9, EXPECTED_VARIANT_COUNT to 28 |
| `crates/omnish-common/src/config.rs` | Add `Serialize` derive to DaemonConfig and all sub-types |
| `crates/omnish-common/src/config_edit.rs` | Add `set_toml_value_nested()` for dotted key paths |
| `crates/omnish-client/src/widgets/menu.rs` | Add `on_handler_exit` callback to `run_menu`, handler submenu detection |
| `crates/omnish-client/src/chat_session.rs` | Add `/config` command + `build_menu_tree()` + handler callback |
