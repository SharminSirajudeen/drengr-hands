/// Downscale a JPEG screenshot before sending to a vision LLM.
/// Most models accept up to ~1024px on the long edge; UI inspection works
/// fine at 768px. Returns the downscaled JPEG bytes (re-encoded at q85),
/// or an error string the caller can map to a fallback.
pub fn downscale_for_vision(jpeg_bytes: &[u8]) -> Result<Vec<u8>, String> {
    const MAX_LONG_EDGE: u32 = 768;
    const JPEG_QUALITY: u8 = 85;

    let img = image::load_from_memory(jpeg_bytes).map_err(|e| format!("decode: {e}"))?;
    let (w, h) = (img.width(), img.height());
    let long_edge = w.max(h);
    if long_edge <= MAX_LONG_EDGE {
        // Already small — return original bytes unchanged so we don't pay
        // a re-encode pass for nothing.
        return Ok(jpeg_bytes.to_vec());
    }
    let scale = MAX_LONG_EDGE as f32 / long_edge as f32;
    let nw = (w as f32 * scale) as u32;
    let nh = (h as f32 * scale) as u32;
    // Triangle filter — fast + visually clean for UI screenshots.
    let resized = img.resize_exact(nw, nh, image::imageops::FilterType::Triangle);

    let mut out = Vec::with_capacity(jpeg_bytes.len() / 4);
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    image::DynamicImage::ImageRgb8(resized.to_rgb8())
        .write_with_encoder(encoder)
        .map_err(|e| format!("encode: {e}"))?;
    Ok(out)
}

/// Downscale for transport (long edge 768px, JPEG q85), falling back to the
/// original bytes on any decode/encode error. Accepts PNG or JPEG input.
/// Applied at the MCP/CLI image-encode sites so look/do payloads — and the
/// frames the CLI writes to disk — are 4-6x lighter without moving any tap
/// coordinate (taps are computed from screen_size, never the image bytes).
pub fn downscale_or_original(bytes: &[u8]) -> Vec<u8> {
    downscale_for_vision(bytes).unwrap_or_else(|_| bytes.to_vec())
}

/// JPEG compression + perceptual duplicate detection.
pub struct ImageOptimizer {
    last_hash: Option<u64>,
    duplicate_count: u32,
}

impl ImageOptimizer {
    pub fn new() -> Self {
        Self {
            last_hash: None,
            duplicate_count: 0,
        }
    }

    /// Check if an image is a duplicate of the previous one (by content hash).
    /// Returns (is_duplicate, consecutive_duplicate_count).
    pub fn check_duplicate(&mut self, image_data: &[u8]) -> (bool, u32) {
        let hash = compute_hash(image_data);

        let is_duplicate = self.last_hash.map(|last| last == hash).unwrap_or(false);

        if is_duplicate {
            self.duplicate_count += 1;
        } else {
            self.duplicate_count = 0;
        }

        self.last_hash = Some(hash);
        (is_duplicate, self.duplicate_count)
    }

    /// Number of consecutive duplicate images seen.
    pub fn consecutive_duplicates(&self) -> u32 {
        self.duplicate_count
    }

    /// Reset duplicate tracking (e.g. after navigation).
    pub fn reset(&mut self) {
        self.last_hash = None;
        self.duplicate_count = 0;
    }
}

impl Default for ImageOptimizer {
    fn default() -> Self {
        Self::new()
    }
}

/// Compare two PNG images and return the percentage of changed pixels.
/// Returns (diff_percentage, changed_pixel_count, total_pixels).
pub fn pixel_diff_percentage(
    img1_data: &[u8],
    img2_data: &[u8],
) -> Result<(f32, u32, u32), String> {
    use image::GenericImageView;

    let img1 = image::load_from_memory(img1_data)
        .map_err(|e| format!("Failed to decode baseline: {}", e))?;
    let img2 = image::load_from_memory(img2_data)
        .map_err(|e| format!("Failed to decode current: {}", e))?;

    let (w1, h1) = img1.dimensions();
    let (w2, h2) = img2.dimensions();

    // Use smaller dimensions if they differ
    let w = w1.min(w2);
    let h = h1.min(h2);
    let total = w * h;

    if total == 0 {
        return Ok((0.0, 0, 0));
    }

    let mut changed = 0u32;
    let threshold = 30u8; // Per-channel difference threshold

    for y in 0..h {
        for x in 0..w {
            let p1 = img1.get_pixel(x, y);
            let p2 = img2.get_pixel(x, y);
            let dr = (p1[0] as i16 - p2[0] as i16).unsigned_abs() as u8;
            let dg = (p1[1] as i16 - p2[1] as i16).unsigned_abs() as u8;
            let db = (p1[2] as i16 - p2[2] as i16).unsigned_abs() as u8;
            if dr > threshold || dg > threshold || db > threshold {
                changed += 1;
            }
        }
    }

    let percentage = (changed as f64 / total as f64 * 100.0) as f32;
    Ok((percentage, changed, total))
}

/// True when two consecutive frames are visually settled: byte-identical, or
/// differing only by tiny persistent animators (blinking cursor, clock tick).
/// A >1% encoded-size delta is treated as motion without paying a decode.
pub fn frames_settled(prev: &[u8], curr: &[u8]) -> bool {
    if prev == curr {
        return true;
    }
    if prev.is_empty() || curr.is_empty() {
        return false;
    }
    // Size gate only means anything on real screenshot-sized files, where a
    // cursor/clock delta is well under 1% of the encoded bytes.
    const SIZE_GATE_MIN_BYTES: usize = 16 * 1024;
    if prev.len().min(curr.len()) > SIZE_GATE_MIN_BYTES {
        let (lp, lc) = (prev.len() as f64, curr.len() as f64);
        if (lp - lc).abs() / lp.max(lc) > 0.01 {
            return false;
        }
    }
    match pixel_diff_percentage(prev, curr) {
        Ok((pct, _, _)) => pct < 0.5,
        Err(_) => false,
    }
}

/// Fast non-cryptographic hash (std SipHash) for detecting byte-identical
/// consecutive frames. Exact dedup, not perceptual.
fn compute_hash(data: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_first_image_not_duplicate() {
        let mut opt = ImageOptimizer::new();
        let (is_dup, count) = opt.check_duplicate(b"image1");
        assert!(!is_dup);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_same_image_is_duplicate() {
        let mut opt = ImageOptimizer::new();
        opt.check_duplicate(b"image1");
        let (is_dup, count) = opt.check_duplicate(b"image1");
        assert!(is_dup);
        assert_eq!(count, 1);
    }

    #[test]
    fn test_different_image_not_duplicate() {
        let mut opt = ImageOptimizer::new();
        opt.check_duplicate(b"image1");
        let (is_dup, count) = opt.check_duplicate(b"image2");
        assert!(!is_dup);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_consecutive_duplicates_count() {
        let mut opt = ImageOptimizer::new();
        opt.check_duplicate(b"same");
        opt.check_duplicate(b"same");
        opt.check_duplicate(b"same");
        let (is_dup, count) = opt.check_duplicate(b"same");
        assert!(is_dup);
        assert_eq!(count, 3);
        assert_eq!(opt.consecutive_duplicates(), 3);
    }

    #[test]
    fn test_duplicate_resets_on_different() {
        let mut opt = ImageOptimizer::new();
        opt.check_duplicate(b"same");
        opt.check_duplicate(b"same");
        assert_eq!(opt.consecutive_duplicates(), 1);

        opt.check_duplicate(b"different");
        assert_eq!(opt.consecutive_duplicates(), 0);
    }

    #[test]
    fn test_pixel_diff_identical() {
        // Create a minimal 1x1 red PNG for testing
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        let (pct, changed, total) = pixel_diff_percentage(&buf, &buf).unwrap();
        assert_eq!(pct, 0.0);
        assert_eq!(changed, 0);
        assert_eq!(total, 1);
    }

    #[test]
    fn test_pixel_diff_different() {
        // Create two 1x1 PNGs with very different colors
        let img1 = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255])); // red
        let mut buf1 = Vec::new();
        img1.write_to(
            &mut std::io::Cursor::new(&mut buf1),
            image::ImageFormat::Png,
        )
        .unwrap();

        let img2 = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 255, 255])); // blue
        let mut buf2 = Vec::new();
        img2.write_to(
            &mut std::io::Cursor::new(&mut buf2),
            image::ImageFormat::Png,
        )
        .unwrap();

        let (pct, changed, total) = pixel_diff_percentage(&buf1, &buf2).unwrap();
        assert_eq!(pct, 100.0);
        assert_eq!(changed, 1);
        assert_eq!(total, 1);
    }

    fn png_bytes(img: image::RgbaImage) -> Vec<u8> {
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn frames_settled_identical_bytes() {
        let buf = png_bytes(image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([255, 0, 0, 255]),
        ));
        assert!(frames_settled(&buf, &buf));
    }

    #[test]
    fn frames_settled_rejects_full_change() {
        let red = png_bytes(image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([255, 0, 0, 255]),
        ));
        let blue = png_bytes(image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([0, 0, 255, 255]),
        ));
        assert!(!frames_settled(&red, &blue));
    }

    #[test]
    fn frames_settled_tolerates_cursor_blink() {
        // 100x100 white, second frame has a 1x8 "cursor" — 0.08% of pixels.
        let white = image::RgbaImage::from_pixel(100, 100, image::Rgba([255, 255, 255, 255]));
        let mut cursor = white.clone();
        for y in 10..18 {
            cursor.put_pixel(50, y, image::Rgba([0, 0, 0, 255]));
        }
        assert!(frames_settled(&png_bytes(white), &png_bytes(cursor)));
    }

    #[test]
    fn frames_settled_size_gate_rejects_without_decode() {
        // Screenshot-sized buffers with a big length delta — must reject
        // before decoding (inputs aren't even valid images).
        assert!(!frames_settled(&vec![0u8; 20_000], &vec![0u8; 30_000]));
    }

    #[test]
    fn test_manual_reset() {
        let mut opt = ImageOptimizer::new();
        opt.check_duplicate(b"image");
        opt.check_duplicate(b"image");
        assert_eq!(opt.consecutive_duplicates(), 1);

        opt.reset();
        assert_eq!(opt.consecutive_duplicates(), 0);

        let (is_dup, _) = opt.check_duplicate(b"image");
        assert!(!is_dup); // After reset, even same image is not a duplicate
    }
}
