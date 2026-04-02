## Updating index.md
Update index.md. This document describes the main components in each module doc along with their line number ranges.
For each component, focus on its functional responsibilities. The description should cover the key points of all sub-features.
Avoid redundant information and keep the file small — index.md is loaded as a reference by the LLM in every task.
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

Use `split_doc_sections.sh` to split a module doc by `## ` headings into independent sections, making it easy to read and update index.md section by section:

```bash
# List all sections with line number ranges
bash docs/implementation/split_doc_sections.sh docs/implementation/omnish-daemon.md list

# Get a section by name (substring match)
bash docs/implementation/split_doc_sections.sh docs/implementation/omnish-daemon.md get "插件系统"

# Get a section by number (0=preamble, 1-N=sections)
bash docs/implementation/split_doc_sections.sh docs/implementation/omnish-daemon.md get 3
```
