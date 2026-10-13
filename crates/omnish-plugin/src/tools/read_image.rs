//! `read_image`: let the LLM view an image file (issue #690).
//!
//! Returns the image through the `omnish_common::tool_image` content contract
//! (the tool declares `"result_images": true` in tool.json); the daemon turns
//! it into an image block for the model. Images are kept within provider
//! limits here, on the client, so any screenshot can be viewed.

use std::io::Cursor;

use image::{imageops::FilterType, DynamicImage, ImageFormat, ImageReader, Limits, RgbImage};
use omnish_common::tool_image;
use omnish_llm::tool::ToolResult;

/// Refuse files larger than this before reading them.
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// Longest edge sent to the model; providers downscale larger images anyway.
const MAX_EDGE: u32 = 1568;
/// Largest encoded image: 5 MB (Anthropic's per-image limit) after base64.
pub(crate) const MAX_IMAGE_BYTES: usize = 3_750_000;
/// JPEG qualities tried in order until the image fits `MAX_IMAGE_BYTES`.
const JPEG_QUALITIES: &[u8] = &[85, 70, 55, 40];

#[derive(Default)]
pub struct ReadImageTool;

/// An image ready to send, plus what to tell the model about it.
struct Prepared {
    bytes: Vec<u8>,
    media_type: &'static str,
    width: u32,
    height: u32,
    /// Original dimensions when the image was downscaled.
    resized_from: Option<(u32, u32)>,
}

fn error(content: String) -> ToolResult {
    ToolResult { tool_use_id: String::new(), content, is_error: true }
}

fn format_label(media_type: &str) -> &str {
    media_type.strip_prefix("image/").unwrap_or(media_type)
}

fn human_size(bytes: usize) -> String {
    if bytes < 1024 * 1024 {
        format!("{} KB", bytes.div_ceil(1024))
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    // Guard against decompression bombs.
    let mut limits = Limits::default();
    limits.max_image_width = Some(30_000);
    limits.max_image_height = Some(30_000);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader)
}

/// Composite onto white and drop the alpha channel (for JPEG).
fn flatten(img: &DynamicImage) -> RgbImage {
    if !img.color().has_alpha() {
        return img.to_rgb8();
    }
    let rgba = img.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y).0;
        let a = p[3] as u32;
        let blend = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
        image::Rgb([blend(p[0]), blend(p[1]), blend(p[2])])
    })
}

fn encode_png(img: &DynamicImage) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

fn encode_jpeg(rgb: &RgbImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
        .encode_image(rgb)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Keep the original bytes when they already fit, else downscale to
/// `MAX_EDGE` and re-encode: PNG when the source was PNG (typically a
/// screenshot, where lossless keeps text sharp and often compresses better
/// than JPEG) or has alpha; otherwise, or when the PNG is still too big, JPEG
/// at decreasing quality.
fn prepare(bytes: Vec<u8>, media_type: &'static str) -> Result<Prepared, String> {
    let (width, height) = reader(&bytes)?.into_dimensions().map_err(|e| e.to_string())?;
    if width.max(height) <= MAX_EDGE && bytes.len() <= MAX_IMAGE_BYTES {
        return Ok(Prepared { bytes, media_type, width, height, resized_from: None });
    }

    // Animated GIFs decode to their first frame.
    let mut img = reader(&bytes)?.decode().map_err(|e| e.to_string())?;
    let mut resized_from = None;
    if width.max(height) > MAX_EDGE {
        img = img.resize(MAX_EDGE, MAX_EDGE, FilterType::Triangle);
        resized_from = Some((width, height));
    }
    let (w, h) = (img.width(), img.height());
    let done = |bytes: Vec<u8>, media_type: &'static str| Prepared {
        bytes, media_type, width: w, height: h, resized_from,
    };

    if media_type == "image/png" || img.color().has_alpha() {
        let png = encode_png(&img)?;
        if png.len() <= MAX_IMAGE_BYTES {
            return Ok(done(png, "image/png"));
        }
    }
    let rgb = flatten(&img);
    for &quality in JPEG_QUALITIES {
        let jpeg = encode_jpeg(&rgb, quality)?;
        if jpeg.len() <= MAX_IMAGE_BYTES {
            return Ok(done(jpeg, "image/jpeg"));
        }
    }
    Err("image is too large even after downscaling and compression".to_string())
}

impl ReadImageTool {
    pub fn new() -> Self {
        Self
    }

    pub fn execute(&self, input: &serde_json::Value) -> ToolResult {
        let file_path = input["file_path"].as_str().unwrap_or("");
        if file_path.is_empty() {
            return error("Error: 'file_path' is required".to_string());
        }
        if !file_path.starts_with('/') {
            return error(format!("Error: file_path must be an absolute path, got: {}", file_path));
        }
        match std::fs::metadata(file_path) {
            Ok(m) if m.len() > MAX_FILE_BYTES => {
                return error(format!(
                    "Error: {} is {}, larger than the {} limit for images",
                    file_path,
                    human_size(m.len() as usize),
                    human_size(MAX_FILE_BYTES as usize)
                ));
            }
            Ok(_) => {}
            Err(e) => return error(format!("Error reading {}: {}", file_path, e)),
        }
        let bytes = match std::fs::read(file_path) {
            Ok(b) => b,
            Err(e) => return error(format!("Error reading {}: {}", file_path, e)),
        };
        let Some(media_type) = tool_image::media_type_of(&bytes) else {
            return error(format!(
                "Error: {} is not a supported image (png, jpeg, gif, webp). Use the read tool for text files.",
                file_path
            ));
        };
        let prepared = match prepare(bytes, media_type) {
            Ok(p) => p,
            Err(e) => return error(format!("Error: cannot read image {}: {}", file_path, e)),
        };

        let resized = prepared
            .resized_from
            .map(|(w, h)| format!(", resized from {}x{}", w, h))
            .unwrap_or_default();
        let description = format!(
            "Image: {} ({}x{}{}, {}, {})",
            file_path,
            prepared.width,
            prepared.height,
            resized,
            format_label(prepared.media_type),
            human_size(prepared.bytes.len())
        );
        ToolResult {
            tool_use_id: String::new(),
            content: tool_image::encode(prepared.media_type, &prepared.bytes, &description),
            is_error: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

    fn save(img: &image::DynamicImage, dir: &std::path::Path, name: &str, fmt: ImageFormat) -> String {
        let path = dir.join(name);
        img.save_with_format(&path, fmt).unwrap();
        path.to_str().unwrap().to_string()
    }

    fn run(path: &str) -> omnish_llm::tool::ToolResult {
        ReadImageTool::new().execute(&serde_json::json!({ "file_path": path }))
    }

    /// Deterministic noise so encoders cannot compress it away.
    fn noise_rgba(w: u32, h: u32) -> RgbaImage {
        let mut x: u32 = 12345;
        RgbaImage::from_fn(w, h, |_, _| {
            let mut next = || {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                (x >> 16) as u8
            };
            Rgba([next(), next(), next(), next()])
        })
    }

    #[test]
    fn small_png_passes_through_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let img = image::DynamicImage::ImageRgb8(RgbImage::from_pixel(40, 30, Rgb([200, 10, 10])));
        let path = save(&img, dir.path(), "a.png", ImageFormat::Png);
        let r = run(&path);
        assert!(!r.is_error, "{}", r.content);
        let parsed = omnish_common::tool_image::parse(&r.content).expect("image content");
        assert_eq!(parsed.media_type, "image/png");
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode(&parsed.data_base64).unwrap();
        assert_eq!(bytes, std::fs::read(&path).unwrap(), "original bytes, no re-encode");
        assert!(parsed.description.starts_with(&format!("Image: {} (40x30, png, ", path)));
        assert!(!parsed.description.contains("resized"));
    }

    #[test]
    fn small_gif_passes_through_as_gif() {
        let dir = tempfile::tempdir().unwrap();
        let img = image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 8, Rgba([0, 0, 255, 255])));
        let path = save(&img, dir.path(), "a.gif", ImageFormat::Gif);
        let parsed = omnish_common::tool_image::parse(&run(&path).content).unwrap();
        assert_eq!(parsed.media_type, "image/gif");
    }

    fn decode_output(r: &omnish_llm::tool::ToolResult) -> (omnish_common::tool_image::ToolImage, image::DynamicImage) {
        assert!(!r.is_error, "{}", r.content);
        let parsed = omnish_common::tool_image::parse(&r.content).unwrap();
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode(&parsed.data_base64).unwrap();
        let img = image::load_from_memory(&bytes).unwrap();
        (parsed, img)
    }

    #[test]
    fn large_png_is_resized_and_stays_png() {
        // Screenshots are usually PNG; keep them lossless so text stays sharp.
        let dir = tempfile::tempdir().unwrap();
        let img = image::DynamicImage::ImageRgb8(RgbImage::from_pixel(3000, 2000, Rgb([10, 200, 10])));
        let path = save(&img, dir.path(), "big.png", ImageFormat::Png);
        let (parsed, out) = decode_output(&run(&path));
        assert_eq!(parsed.media_type, "image/png");
        assert!(parsed.description.contains("(1568x1045, resized from 3000x2000, png, "), "{}", parsed.description);
        assert_eq!((out.width(), out.height()), (1568, 1045));
    }

    #[test]
    fn large_jpeg_is_resized_and_stays_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let img = image::DynamicImage::ImageRgb8(RgbImage::from_pixel(3000, 2000, Rgb([10, 200, 10])));
        let path = save(&img, dir.path(), "big.jpg", ImageFormat::Jpeg);
        let (parsed, out) = decode_output(&run(&path));
        assert_eq!(parsed.media_type, "image/jpeg");
        assert_eq!((out.width(), out.height()), (1568, 1045));
    }

    #[test]
    fn png_too_large_even_resized_falls_back_to_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let rgb = image::DynamicImage::ImageRgba8(noise_rgba(1568, 1568)).to_rgb8();
        let path = save(&image::DynamicImage::ImageRgb8(rgb), dir.path(), "noise_rgb.png", ImageFormat::Png);
        assert!(std::fs::metadata(&path).unwrap().len() as usize > MAX_IMAGE_BYTES);
        let (parsed, _) = decode_output(&run(&path));
        assert_eq!(parsed.media_type, "image/jpeg");
    }

    #[test]
    fn large_image_with_alpha_stays_png() {
        let dir = tempfile::tempdir().unwrap();
        let img = image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(2000, 1000, Rgba([0, 0, 0, 100])));
        let path = save(&img, dir.path(), "alpha.png", ImageFormat::Png);
        let parsed = omnish_common::tool_image::parse(&run(&path).content).unwrap();
        assert_eq!(parsed.media_type, "image/png");
        assert!(parsed.description.contains("(1568x784, resized from 2000x1000, png, "), "{}", parsed.description);
    }

    #[test]
    fn oversized_output_falls_back_to_smaller_jpeg() {
        // Noise with alpha: PNG at 1568px is far above the limit, so the alpha
        // is flattened and JPEG quality steps down until it fits.
        let dir = tempfile::tempdir().unwrap();
        let img = image::DynamicImage::ImageRgba8(noise_rgba(1568, 1568));
        let path = save(&img, dir.path(), "noise.png", ImageFormat::Png);
        assert!(std::fs::metadata(&path).unwrap().len() as usize > MAX_IMAGE_BYTES);
        let r = run(&path);
        assert!(!r.is_error, "{}", r.content);
        let parsed = omnish_common::tool_image::parse(&r.content).unwrap();
        assert_eq!(parsed.media_type, "image/jpeg");
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode(&parsed.data_base64).unwrap();
        assert!(bytes.len() <= MAX_IMAGE_BYTES);
    }

    #[test]
    fn errors() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, "hello").unwrap();
        let r = run(text.to_str().unwrap());
        assert!(r.is_error && r.content.contains("read tool"), "{}", r.content);

        let r = run("relative.png");
        assert!(r.is_error && r.content.contains("absolute"));

        let r = run(dir.path().join("missing.png").to_str().unwrap());
        assert!(r.is_error);

        // PNG magic but no valid image behind it.
        let corrupt = dir.path().join("corrupt.png");
        std::fs::write(&corrupt, b"\x89PNG\r\n\x1a\ngarbage").unwrap();
        let r = run(corrupt.to_str().unwrap());
        assert!(r.is_error, "{}", r.content);

        let r = ReadImageTool::new().execute(&serde_json::json!({}));
        assert!(r.is_error && r.content.contains("file_path"));
    }
}
