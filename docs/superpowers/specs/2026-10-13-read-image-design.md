# `read_image` tool: let the LLM view image files (issue #690)

## Goal

Let the chat model look at image files on the client machine (screenshots,
diagrams, photos). The model calls a new client-side tool, `read_image`, with an
absolute path; the image reaches the model as a real image input, not as text.

Success: on `deepseek-flash` (Anthropic-compatible endpoint) the model reads a
screenshot and describes it correctly; on a backend configured without vision
the conversation keeps working with a placeholder instead of failing.

## Decisions (agreed)

- Images enter the conversation only through a tool the model calls. Attaching
  images to the user's chat input is out of scope.
- A dedicated tool `read_image` in the builtin plugin. `read` keeps its code,
  parameters and output untouched; only its description gains one sentence
  pointing image files to `read_image`.
- The client plugin decodes, resizes and re-encodes images so any screenshot
  fits provider limits.
- No protocol change. The image travels inside the tool result's `content`
  string in a fixed marker format that the daemon converts into an image block.
- Each LLM backend has a `supports_vision` switch (default true). When false the
  daemon replaces images with a placeholder sentence in the request.
- Plugin side produces images; everything that represents and sends images is
  generic infrastructure keyed on a tool metadata flag, not on the tool name.

## Current behavior (baseline)

- `read` decodes any file as text; an image comes back as garbage characters.
- `ChatToolResult.content` is a `String`; the daemon stores every tool result as
  `{"type": "tool_result", "content": "<string>"}` in the thread JSONL
  (Anthropic message format, used for all backends).
- No image handling exists anywhere (no base64, no image blocks).
- Builtin tool definitions (`crates/omnish-plugin/assets/tool.json`) are embedded
  in the daemon binary, so the daemon decides which tools the model sees.

## Components

### 1. Plugin: `read_image` tool (`crates/omnish-plugin/src/tools/read_image.rs`)

Input: `{"file_path": "<absolute path>"}`.

1. Reject relative paths and files over 50 MB (same error style as `read`).
2. Detect the format from magic bytes, not the extension: PNG, JPEG, GIF, WebP.
   Anything else returns an error; a non-image file suggests using `read`.
3. Size rules (Anthropic limit: 5 MB per image after base64, long edge above
   about 1568 px is downscaled server-side anyway):
   - Long edge <= 1568 px and file <= 3.75 MB (5 MB after base64): send the
     original bytes unchanged.
   - Otherwise decode with the `image` crate (only the png/jpeg/gif/webp
     features, decoder limits set against decompression bombs), resize so the
     long edge is 1568 px, and encode as PNG when the source is PNG
     (screenshots: lossless keeps text sharp) or has an alpha channel, else
     JPEG quality 85. If still over 3.75 MB, retry JPEG at quality
     70, 55, 40 (alpha flattened onto white). Animated GIFs use the first frame.
4. Output `content` (one image per call):

   ```
   <omnish-image media_type="image/png">
   iVBORw0KGgo... (standard base64, single line)
   </omnish-image>
   Image: /tmp/shot.png (1568x980, resized from 3136x1960, png, 412 KB)
   ```

Errors (`is_error: true`): unsupported format, not an image, decode failure,
too large, unreadable file.

### 2. Tool metadata (`crates/omnish-plugin/assets/tool.json`)

- New entry `read_image`: description ("View an image file: png, jpeg, gif or
  webp. Use this instead of read for images."), input schema with `file_path`,
  display name, status template, formatter `default`, sandboxed like `read`,
  and `"result_images": true`.
- `read` description gains: "For image files (png, jpeg, gif, webp) use
  read_image instead."
- `omnish-plugin/src/main.rs` dispatches `"read_image"`.

### 3. Marker format (`omnish-common::tool_image`)

The single definition of the plugin-to-host image contract, used by the plugin
(encode) and the daemon (parse):

- `encode(media_type, bytes, description) -> String`
- `parse(content) -> Option<ToolImage { media_type, data_base64, description }>`

`parse` accepts only content that starts with `<omnish-image media_type="`, has
one of the four media types, a base64 line that decodes, decoded bytes whose
magic matches the media type, and the closing tag; the rest is the description.
Documented as part of the plugin interface next to the plugin JSON response.

### 4. Daemon: intake and storage

- `ToolMeta` gains `result_images: bool` from `tool.json` (default false).
- `handle_tool_result`: for a tool with `result_images`, parse the content. On
  success, record the `tool_use_id` as an image result, skip LLM summarization,
  and pass only the description to the display formatter (the live status shows
  `Image: ... (1568x980 ...)`, never base64). Other tools are never parsed.
- A shared builder `tool_result_block(id, content, is_error, is_image)` replaces
  the inline `json!` at every site that stores real tool results (normal
  completion and interrupt persistence). For an image result it emits:

  ```json
  {"type": "tool_result", "tool_use_id": "...", "is_error": false, "content": [
    {"type": "text", "text": "Image: /tmp/shot.png (1568x980, ...)"},
    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "..."}}
  ]}
  ```

  Stored as-is in the thread JSONL (about 100 to 500 KB per image).

### 5. Images for a backend without vision

Applied in the backends at request time (implementation note: the agent loop
appends a tool_result with an image to the request and calls the LLM again in
the same turn without passing any daemon-side view, so only the backend layer
covers every path):

- Backend without vision: each image block becomes the text
  `[Image omitted: the current model does not accept image input]`.
- `/compact mini` leaves images unchanged: its view still only truncates bulky
  text tool results and drops old-round thinking.

Deterministic, so the prompt cache keeps hitting between turns.

### 6. Backend capability

- `LlmBackendConfig` gains `supports_vision: Option<bool>` (absent = true), set
  per backend in `daemon.toml` (`[llm.backends.X] supports_vision = false`).
- `LlmBackend` trait gains `fn supports_vision(&self) -> bool { true }`; the
  Anthropic and OpenAI-compatible backends return the configured value and the
  Langfuse wrapper delegates.

### 7. Backend conversion

- Anthropic-compatible: tool_result with an image array is sent as-is (Claude
  supports images inside tool_result). DeepSeek's docs confirm image support
  but not inside tool_result; verified before implementation (see Open items).
  Fallback if rejected: tool_result keeps the text, and the image blocks follow
  the tool_result blocks in the same user message, preceded by the text
  `Image returned by tool call <id>:`. Also valid Anthropic format.
- OpenAI-compatible (`convert_extra_messages`): the `tool` message carries the
  text only (Chat Completions tool messages are text); the images follow in one
  `user` message with parts `{"type": "text", "text": "Image returned by tool
  call <id>:"}` and `{"type": "image_url", "image_url": {"url":
  "data:<media_type>;base64,<data>"}}`. Text the user merged into the same
  stored message is appended to that user message after the images.

### 8. Other readers of history

- `/resume` history (`reconstruct_history`): tool_result content that is an
  array is shown as its joined text blocks (the description line). Today it
  reads only string content and would show nothing.
- Full `/compact` transcript, `/compact mini` truncation, daily notes and
  exchanges read text blocks only, so images are skipped and the description
  line keeps the fact that an image was viewed.

## Data flow

model calls `read_image` -> daemon forwards `ChatToolCall` -> client runs
omnish-plugin -> content = marker + description -> `ChatToolResult` (unchanged)
-> daemon parses (tool declares `result_images`) -> stores image tool_result ->
request view (vision / mini boundary) -> backend conversion -> model sees image.

## Compatibility

- No protocol change, no client code change; only omnish-plugin (shipped with
  the client package) and the daemon change.
- Old daemon + new client: the daemon embeds tool definitions, so it never
  offers `read_image` and no marker is ever produced.
- New daemon + old client: the old plugin answers `Unknown tool: read_image`
  as a tool error; the conversation continues.

## Error handling

- Plugin errors are ordinary tool errors (`is_error: true`) with a hint.
- Content from a `result_images` tool that fails to parse is stored as plain
  text (and logged), never dropped.
- A provider rejecting images is avoided by `supports_vision = false`.

## Testing

- `omnish-common`: encode/parse round trip; parse rejects wrong prefix, unknown
  media type, bad base64, magic/media-type mismatch, missing closing tag.
- omnish-plugin `read_image`: small generated PNG/JPEG pass through unchanged;
  oversized image resized to a 1568 px long edge; alpha keeps PNG; oversize
  output steps JPEG quality down; corrupt file, non-image, relative path, too
  large file errors.
- Daemon: tool metadata flag; `tool_result_block` text vs image; only
  `result_images` tools are parsed; summarization skipped and formatter gets the
  description; request view for vision off and mini boundary (idempotent);
  `reconstruct_history` shows the description.
- omnish-llm: OpenAI conversion of an image tool_result (alone and with merged
  user text); Anthropic pass-through (or fallback layout).
- Manual: DeepSeek experiment (below); end-to-end on `deepseek-flash` reading a
  real screenshot (needs a daemon restart by the user).

## Open items

1. Resolved by experiment: DeepSeek's Anthropic-compatible endpoint accepts
   both layouts, but only the image inside tool_result reaches the model (a
   red 64x64 PNG was answered "Red"; with the fallback layout "Orange").
   Section 7 uses pass-through; the fallback layout is not implemented.
2. Resolved in review: `/compact mini` does not touch images.

## Scope / YAGNI

- No images in the user's chat input, no clipboard paste.
- One image per `read_image` call; no PDFs or other documents.
- `supports_vision` only in `daemon.toml`, not in the config menu.
- Images stored inline in the thread JSONL; moving them to side files can come
  later without touching the marker or the wire format.
