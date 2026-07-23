pub mod bash;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod read;
pub mod write;

/// Decode raw file bytes into a UTF-8 `String`.
///
/// Fast path: if the bytes are already valid UTF-8, return them directly.
/// Otherwise use `chardetng` to detect the source charset (GBK/Big5/Shift_JIS,
/// etc.) and transcode with `encoding_rs`. `encoding_rs::Encoding::decode`
/// itself falls back to lossy replacement for any bytes that remain undecodable,
/// so this never errors. Files edited this way are rewritten as UTF-8.
pub(crate) fn decode_bytes(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(bytes, true);
    let encoding = detector.guess(None, true);
    let (cow, _, _) = encoding.decode(bytes);
    cow.into_owned()
}
