## Updating index.md
Update index.md. This document describes the main components in each module doc and their functional responsibilities.
For each component, focus on its functional responsibilities. The description should cover the key points of all sub-features.
Avoid redundant information and keep the file small - index.md is loaded as a reference by the LLM in every task.
For incremental updates, do NOT include every change in the module docs. Ignore implementation details.

### What belongs in index.md
- Data structure type changes (e.g. TasksConfig changed from a concrete struct to a HashMap type alias)
- Protocol version updates
- New fields on protocol messages (brief mention)
- Important new state fields on core structs (e.g. AgentLoopState adding cancel_flag)
- Public API signature changes (e.g. return type changes)

### What does NOT belong in index.md
- Specific function names (e.g. `sanitize_orphaned_tool_use()`, `unregister_by_plugin()`)
- Internal mechanism details (e.g. inotify event types, RwLock usage, specific trait method signatures)
- Helper types or wrapper types do not need their own entries (e.g. ConfigMap)
- Error handling / diagnostics improvements
- Internal logic of config menu generation
- Appending implementation details to existing component descriptions (preserve the original granularity)

## Using split_doc_sections.sh for section-by-section updates

Use `split_doc_sections.sh` to list sections by `## ` headings with line ranges, then use Read tool with offset/limit to read only the relevant sections:

```bash
bash docs/implementation/split_doc_sections.sh docs/implementation/omnish-daemon.md
```
