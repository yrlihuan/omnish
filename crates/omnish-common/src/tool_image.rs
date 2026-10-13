//! Image-in-tool-result contract between plugins and the daemon (issue #690).
//!
//! A tool that declares `"result_images": true` in its `tool.json` may return
//! one image by making its whole `content` string:
//!
//! ```text
//! <omnish-image media_type="image/png">
//! iVBORw0KGgo...            (standard base64, one line)
//! </omnish-image>
//! Image: /tmp/shot.png (1568x980, png, 412 KB)   (description, may be empty)
//! ```
//!
//! The daemon turns it into an image block for the LLM. Parsing is strict:
//! the content must start with the tag, use a supported media type, carry
//! valid base64, and the decoded bytes must really be that image format, so
//! ordinary text can never be mistaken for an image.

use base64::Engine;

const OPEN_PREFIX: &str = "<omnish-image media_type=\"";
const OPEN_SUFFIX: &str = "\">\n";
const CLOSE: &str = "\n</omnish-image>";

/// A parsed tool-result image.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolImage {
    pub media_type: String,
    /// Standard base64 of the image bytes, ready for an API payload.
    pub data_base64: String,
    /// Human/LLM readable text that accompanies the image.
    pub description: String,
}

/// Detect a supported image format from its magic bytes: png, jpeg, gif or
/// webp, the formats accepted by both Anthropic and DeepSeek.
pub fn media_type_of(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Build the tool-result content for one image.
pub fn encode(media_type: &str, bytes: &[u8], description: &str) -> String {
    format!(
        "{}{}{}{}{}\n{}",
        OPEN_PREFIX,
        media_type,
        OPEN_SUFFIX,
        base64::engine::general_purpose::STANDARD.encode(bytes),
        CLOSE,
        description
    )
}

/// Parse tool-result content produced by [`encode`]. `None` unless the whole
/// content is a valid image block (see the module docs).
pub fn parse(content: &str) -> Option<ToolImage> {
    let rest = content.strip_prefix(OPEN_PREFIX)?;
    let (media_type, rest) = rest.split_once(OPEN_SUFFIX)?;
    let (data, rest) = rest.split_once(CLOSE)?;
    let description = match rest {
        "" => "",
        r => r.strip_prefix('\n')?,
    };
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
    if media_type_of(&bytes)? != media_type {
        return None;
    }
    Some(ToolImage {
        media_type: media_type.to_string(),
        data_base64: data.to_string(),
        description: description.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";
    const JPEG: &[u8] = b"\xff\xd8\xff\xe0rest";
    const GIF: &[u8] = b"GIF89arest";
    const WEBP: &[u8] = b"RIFF\x10\x00\x00\x00WEBPrest";

    #[test]
    fn sniffs_supported_formats() {
        assert_eq!(media_type_of(PNG), Some("image/png"));
        assert_eq!(media_type_of(JPEG), Some("image/jpeg"));
        assert_eq!(media_type_of(GIF), Some("image/gif"));
        assert_eq!(media_type_of(b"GIF87a..."), Some("image/gif"));
        assert_eq!(media_type_of(WEBP), Some("image/webp"));
        assert_eq!(media_type_of(b"RIFF\x10\x00\x00\x00WAVE"), None);
        assert_eq!(media_type_of(b"hello"), None);
        assert_eq!(media_type_of(b""), None);
    }

    #[test]
    fn round_trip() {
        let content = encode("image/png", PNG, "Image: /tmp/a.png (1x1, png, 1 KB)");
        assert!(content.starts_with("<omnish-image media_type=\"image/png\">\n"));
        let img = parse(&content).expect("parses");
        assert_eq!(img.media_type, "image/png");
        assert_eq!(img.description, "Image: /tmp/a.png (1x1, png, 1 KB)");
        use base64::Engine;
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(&img.data_base64).unwrap(), PNG);
    }

    #[test]
    fn rejects_malformed_content() {
        let good = encode("image/jpeg", JPEG, "desc");
        assert!(parse(&good).is_some());
        // Plain text, e.g. a `read` result or a file that mentions the tag.
        assert!(parse("     1\t<omnish-image media_type=\"image/png\">").is_none());
        assert!(parse(&format!(" {}", good)).is_none(), "must start with the tag");
        // Unknown media type.
        assert!(parse(&good.replacen("image/jpeg", "image/bmp", 1)).is_none());
        // Media type does not match the decoded bytes.
        assert!(parse(&good.replacen("image/jpeg", "image/png", 1)).is_none());
        // Bad base64.
        assert!(parse("<omnish-image media_type=\"image/png\">\n!!!\n</omnish-image>\nd").is_none());
        // Missing closing tag.
        let open_only = good.replace("</omnish-image>", "");
        assert!(parse(&open_only).is_none());
    }

    #[test]
    fn description_may_be_empty() {
        let img = parse(&encode("image/gif", GIF, "")).unwrap();
        assert_eq!(img.description, "");
        let img = parse(&encode("image/webp", WEBP, "d")).unwrap();
        assert_eq!(img.media_type, "image/webp");
    }
}
