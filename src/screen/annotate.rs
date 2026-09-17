use anyhow::{Context, Result};
use image::{DynamicImage, Rgba, RgbaImage};

use crate::screen::ui_element::UiElement;

/// Draws numbered red circles on interactive elements in a screenshot.
pub struct ScreenAnnotator {
    /// Max number of elements to annotate (default 50).
    max_elements: usize,
}

/// An annotated element with its number and tap coordinates.
#[derive(Debug, Clone)]
pub struct AnnotatedElement {
    /// Display number (1-based).
    pub number: usize,
    /// Center X coordinate for tapping.
    pub tap_x: i32,
    /// Center Y coordinate for tapping.
    pub tap_y: i32,
    /// The original UI element.
    pub element: UiElement,
}

/// One element recovered from disk, enough for a cold process to resolve or search.
pub struct PersistedElement {
    pub number: usize,
    pub label: String,
    pub tap_x: i32,
    pub tap_y: i32,
    /// Identity, so a cold process can hand this element the same number the
    /// process that wrote the file gave it. `None` for files written before
    /// numbers were identities.
    pub fingerprint: Option<u64>,
}

/// What an earlier process observed, with enough provenance to judge whether it
/// still describes the screen in front of you.
pub struct PersistedObservation {
    /// Fingerprint to number, for every element this session has numbered.
    pub ids: Vec<(u64, usize)>,
    pub device: String,
    pub package: String,
    pub activity: String,
    pub written_at: String,
    pub elements: Vec<PersistedElement>,
}

/// Where and when an observation happened. Stamped onto the persisted record.
pub struct ObservationContext {
    pub device: String,
    pub package: String,
    pub activity: String,
    pub written_at: String,
    /// Every fingerprint-to-number assignment this session has made.
    pub assignments: Vec<(u64, usize)>,
}

/// Result of annotating a screenshot.
#[derive(Clone)]
pub struct AnnotatedScreen {
    /// The annotated JPEG image data.
    pub image_data: Vec<u8>,
    /// The annotated interactive elements.
    pub elements: Vec<AnnotatedElement>,
    /// The frame this was drawn on, before any marker was added. Kept because
    /// comparing the annotated JPEG against a later raw PNG compares two
    /// encodings of two different images and can never match.
    pub source_frame: Vec<u8>,
}

impl ScreenAnnotator {
    pub fn new() -> Self {
        Self {
            max_elements: crate::screen::ui_element::max_addressable(),
        }
    }

    pub fn with_max_elements(mut self, max: usize) -> Self {
        self.max_elements = max;
        self
    }

    /// Annotate by auto-numbering 1..N. Use for one-shot callers and tests;
    /// OODA should use `annotate_with_ids` to keep numbers stable across steps.
    pub fn annotate(
        &self,
        screenshot_png: &[u8],
        elements: &[UiElement],
        logical_size: (u32, u32),
    ) -> Result<AnnotatedScreen> {
        let numbered: Vec<(usize, &UiElement)> =
            crate::screen::ui_element::addressable(elements, self.max_elements)
                .into_iter()
                .enumerate()
                .map(|(i, e)| (i + 1, e))
                .collect();
        self.annotate_with_ids(screenshot_png, &numbered, logical_size)
    }

    /// Annotate with pre-assigned stable IDs from `ElementRegistry`.
    pub fn annotate_with_ids(
        &self,
        screenshot_png: &[u8],
        numbered: &[(usize, &UiElement)],
        logical_size: (u32, u32),
    ) -> Result<AnnotatedScreen> {
        let img =
            image::load_from_memory(screenshot_png).context("Failed to decode screenshot PNG")?;

        let mut canvas = img.to_rgba8();

        // Element bounds are in the device's logical/native coordinate space
        // (iOS points, Android pixels); the screenshot may be at any physical
        // resolution (Retina scale, or downscaled to save vision cost). Derive
        // the overlay scale by MEASURING the actual image against the logical
        // size, so dots land correctly on any device and at any resolution —
        // no per-device assumptions. logical == image size ⇒ scale 1.0.
        let (logical_w, logical_h) = logical_size;
        let scale_x = if logical_w > 0 {
            canvas.width() as f32 / logical_w as f32
        } else {
            1.0
        };
        let scale_y = if logical_h > 0 {
            canvas.height() as f32 / logical_h as f32
        } else {
            1.0
        };
        // A device reporting a tiny logical size against a 4096-capped frame gave
        // radius ~81920, so radius*radius overflowed i32 and the glyph blitter ran
        // ~1e8 iterations per lit cell.
        let radius = (20.0 * scale_x.max(scale_y)).round().clamp(20.0, 200.0) as i32;
        // Glyphs are 7 cells tall; size them to sit inside the dot.
        let glyph_scale = (radius / 7).max(1);

        let interactive: Vec<(usize, &UiElement)> = numbered
            .iter()
            .filter(|(_, e)| e.is_relevant())
            .take(self.max_elements)
            .map(|(n, e)| (*n, *e))
            .collect();

        let mut annotated_elements = Vec::with_capacity(interactive.len());

        let mut placed: Vec<MarkerRect> = Vec::with_capacity(interactive.len());
        let (canvas_w, canvas_h) = (canvas.width() as i32, canvas.height() as i32);

        for (number, elem) in &interactive {
            // tap_x/tap_y stay in the logical/native space — that's what the tap
            // path feeds back to the device. Only the overlay is mapped into the
            // screenshot's pixel space, and the marker now sits BESIDE the element,
            // so its position is no longer the tap point.
            let cx = elem.bounds.center_x();
            let cy = elem.bounds.center_y();
            let b_left = (elem.bounds.left as f32 * scale_x).round() as i32;
            let b_top = (elem.bounds.top as f32 * scale_y).round() as i32;
            let b_right = (elem.bounds.right as f32 * scale_x).round() as i32;
            let (draw_x, draw_y) =
                place_marker(b_left, b_top, b_right, radius, canvas_w, canvas_h, &placed);
            placed.push(MarkerRect::around(draw_x, draw_y, radius));

            draw_filled_circle(&mut canvas, draw_x, draw_y, radius, Rgba([255, 0, 0, 200]));
            draw_number(&mut canvas, draw_x, draw_y, *number, glyph_scale);

            annotated_elements.push(AnnotatedElement {
                number: *number,
                tap_x: cx,
                tap_y: cy,
                element: (*elem).clone(),
            });
        }

        // image 0.25's JPEG encoder rejects RGBA (JPEG has no alpha). The
        // 0.24 encoder silently dropped alpha; we mirror that explicitly.
        let mut jpeg_data = Vec::new();
        let dynamic = DynamicImage::ImageRgba8(canvas).to_rgb8();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, 85);
        DynamicImage::ImageRgb8(dynamic)
            .write_with_encoder(encoder)
            .context("Failed to encode annotated JPEG")?;

        Ok(AnnotatedScreen {
            image_data: jpeg_data,
            elements: annotated_elements,
            source_frame: screenshot_png.to_vec(),
        })
    }

    /// Compute just the numbered tap-coordinate mapping — no image decode, draw, or
    /// encode. tap_x/tap_y are in the device's logical space (identical to what
    /// `annotate_with_ids` produces), which is all the tap path and fast-path need.
    /// Use this on text-only steps, where the drawn overlay is never sent anywhere.
    pub fn elements_for_ids(&self, numbered: &[(usize, &UiElement)]) -> Vec<AnnotatedElement> {
        numbered
            .iter()
            .filter(|(_, e)| e.is_relevant())
            .take(self.max_elements)
            .map(|(number, elem)| AnnotatedElement {
                number: *number,
                tap_x: elem.bounds.center_x(),
                tap_y: elem.bounds.center_y(),
                element: (*elem).clone(),
            })
            .collect()
    }

    /// Get tap coordinates for a numbered element.
    pub fn tap_coordinates(
        annotated: &AnnotatedScreen,
        element_number: usize,
    ) -> Option<(i32, i32)> {
        annotated
            .elements
            .iter()
            .find(|e| e.number == element_number)
            .map(|e| (e.tap_x, e.tap_y))
    }

    /// Where `look` leaves its observation for a LATER `do` process. Takes the
    /// drengr dir rather than $HOME so DRENGR_HOME is honoured and the 0700 that
    /// ensure_drengr_dir applies is not bypassed.
    pub fn tap_targets_path_in(drengr_dir: &std::path::Path) -> std::path::PathBuf {
        drengr_dir.join("cli/last_elements.json")
    }

    /// Persist the observation so a later one-shot process can resolve the numbers
    /// this one handed out. Provenance rides along because a bare list of
    /// coordinates cannot say which screen, which device, or how long ago: without
    /// it a stale file resolves just as confidently as a fresh one and the tap
    /// lands on whatever now occupies that pixel.
    pub fn persist_observation_in(
        home: &std::path::Path,
        ctx: &ObservationContext,
        elements: &[AnnotatedElement],
    ) {
        let record = serde_json::json!({
            // The whole id map, not just this screen's elements: a later process
            // that navigates back to an earlier screen must give those elements
            // the numbers they already had.
            "ids": ctx.assignments.iter()
                .map(|(fp, id)| (fp.to_string(), serde_json::json!(id)))
                .collect::<serde_json::Map<String, serde_json::Value>>(),
            "device": ctx.device,
            "package": ctx.package,
            "activity": ctx.activity,
            "written_at": ctx.written_at,
            "elements": elements.iter().map(|e| serde_json::json!({
                "n": e.number,
                "x": e.tap_x,
                "y": e.tap_y,
                "label": e.element.label_or_empty(),
                "fp": e.element.fingerprint().to_string(),
            })).collect::<Vec<_>>(),
        });
        let p = Self::tap_targets_path_in(home);
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        if std::fs::write(&p, record.to_string()).is_ok() {
            // This file holds the rendered label of every element on the last
            // screen: OTP codes, balances, message bodies. Every other ~/.drengr
            // writer is 0600 and this one was 0644.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
            }
        }
    }

    pub fn persist_observation(ctx: &ObservationContext, elements: &[AnnotatedElement]) {
        if let Ok(dir) = crate::paths::ensure_drengr_dir() {
            Self::persist_observation_in(&dir, ctx, elements);
        }
    }

    /// The observation an earlier process recorded, provenance included. An
    /// unreadable or absent file yields `None`, which callers must treat as "no
    /// screen observed", never as "no matches".
    pub fn persisted_observation_in(home: &std::path::Path) -> Option<PersistedObservation> {
        let raw = std::fs::read_to_string(Self::tap_targets_path_in(home)).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        // Files written before provenance existed are a bare array. They are still
        // valid tap targets, so read them rather than discarding the screen.
        let (meta, items) = match v.as_array() {
            Some(arr) => (None, arr.clone()),
            None => (Some(&v), v.get("elements")?.as_array()?.clone()),
        };
        let field = |k: &str| -> String {
            meta.and_then(|m| m.get(k))
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string()
        };
        Some(PersistedObservation {
            ids: meta
                .and_then(|m| m.get("ids"))
                .and_then(|v| v.as_object())
                .map(|o| {
                    o.iter()
                        .filter_map(|(fp, id)| Some((fp.parse().ok()?, id.as_u64()? as usize)))
                        .collect()
                })
                .unwrap_or_default(),
            device: field("device"),
            package: field("package"),
            activity: field("activity"),
            written_at: field("written_at"),
            elements: items
                .iter()
                .filter_map(|e| {
                    Some(PersistedElement {
                        number: e.get("n")?.as_u64()? as usize,
                        tap_x: e.get("x")?.as_i64()? as i32,
                        tap_y: e.get("y")?.as_i64()? as i32,
                        label: e
                            .get("label")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        fingerprint: e
                            .get("fp")
                            .and_then(|v| v.as_str())
                            .and_then(|v| v.parse().ok()),
                    })
                })
                .collect(),
        })
    }

    /// Resolve a tap target written by an earlier `look` process.
    pub fn tap_coordinates_from_disk_in(
        drengr_dir: &std::path::Path,
        element_number: usize,
    ) -> Option<(i32, i32)> {
        Self::persisted_observation_in(drengr_dir)?
            .elements
            .iter()
            .find(|e| e.number == element_number)
            .map(|e| (e.tap_x, e.tap_y))
    }

    /// The last observation, but only when it demonstrably describes the device we
    /// are driving. Coordinates live in each device's own logical space, so
    /// resolving a record written for another device always succeeds and is always
    /// wrong. A record with no device (written before provenance existed) cannot be
    /// verified, so it is refused rather than trusted; the next `look` rewrites it.
    pub fn observation_for_device_in(
        drengr_dir: &std::path::Path,
        device: &str,
    ) -> Option<PersistedObservation> {
        let o = Self::persisted_observation_in(drengr_dir)?;
        (!o.device.is_empty() && o.device == device).then_some(o)
    }

    pub fn observation_for_device(device: &str) -> Option<PersistedObservation> {
        Self::observation_for_device_in(&crate::paths::drengr_dir()?, device)
    }
}

impl Default for ScreenAnnotator {
    fn default() -> Self {
        Self::new()
    }
}

/// Draw a filled circle on the image.
fn draw_filled_circle(canvas: &mut RgbaImage, cx: i32, cy: i32, radius: i32, color: Rgba<u8>) {
    let (w, h) = (canvas.width() as i32, canvas.height() as i32);
    let r2 = radius * radius;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy > r2 {
                continue;
            }
            let x = cx + dx;
            let y = cy + dy;
            if x < 0 || y < 0 || x >= w || y >= h {
                continue;
            }
            canvas.put_pixel(x as u32, y as u32, color); // bounds-checked above, never panics
        }
    }
}

/// An axis-aligned box in canvas pixel space, used to keep markers off each other.
#[derive(Clone, Copy)]
struct MarkerRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl MarkerRect {
    fn around(cx: i32, cy: i32, radius: i32) -> Self {
        Self {
            left: cx - radius,
            top: cy - radius,
            right: cx + radius,
            bottom: cy + radius,
        }
    }

    fn intersects(&self, other: &MarkerRect) -> bool {
        self.left < other.right
            && other.left < self.right
            && self.top < other.bottom
            && other.top < self.bottom
    }

    fn within(&self, w: i32, h: i32) -> bool {
        self.left >= 0 && self.top >= 0 && self.right <= w && self.bottom <= h
    }
}

/// Where element `n`'s marker goes. Drawn at the element's centre it covered the
/// text it labels, which made typography unjudgeable, so try the margins first and
/// only fall inward when every side is taken or off screen. Deterministic: elements
/// are placed in number order, so the same screen annotates identically every time.
fn place_marker(
    bounds_left: i32,
    bounds_top: i32,
    bounds_right: i32,
    radius: i32,
    canvas_w: i32,
    canvas_h: i32,
    placed: &[MarkerRect],
) -> (i32, i32) {
    let gap = radius / 2;
    let free = |cx: i32, cy: i32| {
        let rect = MarkerRect::around(cx, cy, radius);
        rect.within(canvas_w, canvas_h) && !placed.iter().any(|p| rect.intersects(p))
    };

    // Preferred spots, best first: the left margin is where a reader already looks
    // for a list marker and is almost always empty, then above, then the right.
    let preferred = [
        (bounds_left - radius - gap, bounds_top + radius),
        (bounds_left + radius, bounds_top - radius - gap),
        (bounds_right + radius + gap, bounds_top + radius),
    ];
    for (cx, cy) in preferred {
        if free(cx, cy) {
            return (cx, cy);
        }
    }

    // Overlapping elements exhaust the preferred spots, so stack downward instead
    // of piling markers on one point. Without this, six elements sharing bounds
    // drew six markers at the same pixel and none of them could be read.
    let step = radius * 2 + 2;
    for cx in [
        bounds_left - radius - gap,
        bounds_left + radius,
        bounds_right + radius + gap,
    ] {
        // Clamped and saturating: a device reporting bounds_top of -2e9 made this
        // scan from far off-canvas, measured at 28.6ms per element in release.
        let mut cy = bounds_top.saturating_add(radius).max(radius);
        while cy + radius <= canvas_h {
            if free(cx, cy) {
                return (cx, cy);
            }
            cy += step;
        }
    }

    (
        (bounds_left + radius).clamp(radius, (canvas_w - radius).max(radius)),
        (bounds_top + radius).clamp(radius, (canvas_h - radius).max(radius)),
    )
}

/// Overlay a normalized coordinate grid (lines every 10%, labeled 0–100) so a
/// vision model can aim taps on screens with no element tree (Flutter, webview,
/// games, custom canvas). Labels are percentages — pass them to drengr_do as
/// fractions (a button at the "50" vertical / "80" horizontal → x=0.5, y=0.8).
pub fn grid_overlay(screenshot_png: &[u8]) -> Result<Vec<u8>> {
    let img = image::load_from_memory(screenshot_png).context("decode screenshot for grid")?;
    let mut canvas = img.to_rgba8();
    let (w, h) = (canvas.width() as i32, canvas.height() as i32);
    let line = Rgba([0, 220, 255, 255]); // cyan, 1px
                                         // 5x7 glyphs are unreadable on a 1080-wide frame; size them to the image.
    let glyph_scale = (w / 360).max(1);
    for i in 1..10 {
        let gx = w * i / 10;
        let gy = h * i / 10;
        for y in 0..h {
            put(&mut canvas, gx, y, line);
        }
        for x in 0..w {
            put(&mut canvas, x, gy, line);
        }
        let pct = (i * 10).to_string();
        draw_label(&mut canvas, gx + 2, 2, &pct, glyph_scale); // top edge: X %
        draw_label(&mut canvas, 2, gy + 2, &pct, glyph_scale); // left edge: Y %
    }
    let rgb = DynamicImage::ImageRgba8(canvas).to_rgb8();
    let mut jpeg = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85);
    rgb.write_with_encoder(encoder)
        .context("encode grid JPEG")?;
    Ok(jpeg)
}

#[inline]
fn put(canvas: &mut RgbaImage, x: i32, y: i32, color: Rgba<u8>) {
    if x >= 0 && y >= 0 && x < canvas.width() as i32 && y < canvas.height() as i32 {
        canvas.put_pixel(x as u32, y as u32, color); // bounds-checked above, never panics
    }
}

/// Draw a short numeric label with a 1px black shadow for legibility.
fn draw_label(canvas: &mut RgbaImage, x: i32, y: i32, text: &str, scale: i32) {
    let yellow = Rgba([255, 255, 0, 255]);
    let black = Rgba([0, 0, 0, 255]);
    let scale = scale.max(1);
    for (i, ch) in text.chars().enumerate() {
        let cx = x + i as i32 * 7 * scale;
        draw_digit(canvas, cx + scale, y + scale, ch, black, scale);
        draw_digit(canvas, cx, y, ch, yellow, scale);
    }
}

/// Draw a number centered at (cx, cy) in white.
fn draw_number(canvas: &mut RgbaImage, cx: i32, cy: i32, number: usize, scale: i32) {
    let text = number.to_string();
    let white = Rgba([255, 255, 255, 255]);

    // The dot scales with resolution but the glyphs used to stay 5x7 pixels, so
    // on a 1080-wide screenshot two-digit numbers were unreadable and 9 could
    // not be told from 11. Scale the text by the same factor as the dot.
    let scale = scale.max(1);
    let char_width = 8 * scale;
    let text_width = text.len() as i32 * char_width;
    let start_x = cx - text_width / 2;
    let start_y = cy - 6 * scale;

    for (i, ch) in text.chars().enumerate() {
        let x = start_x + i as i32 * char_width;
        draw_digit(canvas, x, start_y, ch, white, scale);
    }
}

/// Draw a single digit as a 5x7 pixel bitmap.
fn draw_digit(canvas: &mut RgbaImage, x: i32, y: i32, digit: char, color: Rgba<u8>, scale: i32) {
    let patterns: &[&[u8]] = match digit {
        '0' => &[
            &[0, 1, 1, 1, 0],
            &[1, 0, 0, 0, 1],
            &[1, 0, 0, 0, 1],
            &[1, 0, 0, 0, 1],
            &[1, 0, 0, 0, 1],
            &[1, 0, 0, 0, 1],
            &[0, 1, 1, 1, 0],
        ],
        '1' => &[
            &[0, 0, 1, 0, 0],
            &[0, 1, 1, 0, 0],
            &[0, 0, 1, 0, 0],
            &[0, 0, 1, 0, 0],
            &[0, 0, 1, 0, 0],
            &[0, 0, 1, 0, 0],
            &[0, 1, 1, 1, 0],
        ],
        '2' => &[
            &[0, 1, 1, 1, 0],
            &[1, 0, 0, 0, 1],
            &[0, 0, 0, 0, 1],
            &[0, 0, 0, 1, 0],
            &[0, 0, 1, 0, 0],
            &[0, 1, 0, 0, 0],
            &[1, 1, 1, 1, 1],
        ],
        '3' => &[
            &[0, 1, 1, 1, 0],
            &[1, 0, 0, 0, 1],
            &[0, 0, 0, 0, 1],
            &[0, 0, 1, 1, 0],
            &[0, 0, 0, 0, 1],
            &[1, 0, 0, 0, 1],
            &[0, 1, 1, 1, 0],
        ],
        '4' => &[
            &[0, 0, 0, 1, 0],
            &[0, 0, 1, 1, 0],
            &[0, 1, 0, 1, 0],
            &[1, 0, 0, 1, 0],
            &[1, 1, 1, 1, 1],
            &[0, 0, 0, 1, 0],
            &[0, 0, 0, 1, 0],
        ],
        '5' => &[
            &[1, 1, 1, 1, 1],
            &[1, 0, 0, 0, 0],
            &[1, 1, 1, 1, 0],
            &[0, 0, 0, 0, 1],
            &[0, 0, 0, 0, 1],
            &[1, 0, 0, 0, 1],
            &[0, 1, 1, 1, 0],
        ],
        '6' => &[
            &[0, 1, 1, 1, 0],
            &[1, 0, 0, 0, 0],
            &[1, 1, 1, 1, 0],
            &[1, 0, 0, 0, 1],
            &[1, 0, 0, 0, 1],
            &[1, 0, 0, 0, 1],
            &[0, 1, 1, 1, 0],
        ],
        '7' => &[
            &[1, 1, 1, 1, 1],
            &[0, 0, 0, 0, 1],
            &[0, 0, 0, 1, 0],
            &[0, 0, 1, 0, 0],
            &[0, 0, 1, 0, 0],
            &[0, 0, 1, 0, 0],
            &[0, 0, 1, 0, 0],
        ],
        '8' => &[
            &[0, 1, 1, 1, 0],
            &[1, 0, 0, 0, 1],
            &[1, 0, 0, 0, 1],
            &[0, 1, 1, 1, 0],
            &[1, 0, 0, 0, 1],
            &[1, 0, 0, 0, 1],
            &[0, 1, 1, 1, 0],
        ],
        '9' => &[
            &[0, 1, 1, 1, 0],
            &[1, 0, 0, 0, 1],
            &[1, 0, 0, 0, 1],
            &[0, 1, 1, 1, 1],
            &[0, 0, 0, 0, 1],
            &[0, 0, 0, 0, 1],
            &[0, 1, 1, 1, 0],
        ],
        _ => return,
    };

    for (row, pattern) in patterns.iter().enumerate() {
        for (col, &pixel) in pattern.iter().enumerate() {
            if pixel != 1 {
                continue;
            }
            // One bitmap cell becomes a scale x scale block.
            for dy in 0..scale {
                for dx in 0..scale {
                    let px = x + col as i32 * scale + dx;
                    let py = y + row as i32 * scale + dy;
                    if px >= 0
                        && py >= 0
                        && (px as u32) < canvas.width()
                        && (py as u32) < canvas.height()
                    {
                        canvas.put_pixel(px as u32, py as u32, color);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_digits_scale_with_the_marker() {
        // The dot scaled with resolution but the glyphs stayed 5x7 pixels, so
        // on a 1080-wide frame 9 could not be told from 11.
        fn lit_pixels(scale: i32) -> usize {
            let mut canvas = RgbaImage::new(200, 200);
            draw_number(&mut canvas, 100, 100, 11, scale);
            canvas.pixels().filter(|p| p.0[3] > 0).count()
        }
        let small = lit_pixels(1);
        let large = lit_pixels(4);
        assert!(small > 0, "scale 1 must still draw");
        assert!(
            large > small * 8,
            "scale 4 should cover roughly 16x the area, got {large} vs {small}"
        );
    }

    use crate::screen::ui_element::Bounds;

    #[test]
    fn numbering_is_contiguous_and_capped_the_same_as_the_text_scene() {
        // Numbering the raw tree first produced 9, 11, 12 with no 10: the gaps
        // were elements the relevance filter had already dropped. And the scene
        // used to run past the annotator's cap, listing numbers with no target.
        let png = make_test_png(400, 4000);
        let mut elems = Vec::new();
        for i in 0..60 {
            elems.push(make_test_element(
                &format!("E{i}"),
                Bounds::new(0, i * 60, 100, i * 60 + 50),
            ));
            // An invisible element: relevant() drops it, so it must not consume a number.
            let mut hidden = make_test_element("", Bounds::new(0, 0, 0, 0));
            hidden.visible = false;
            elems.push(hidden);
        }
        let result = ScreenAnnotator::new()
            .annotate(&png, &elems, (400, 4000))
            .unwrap();

        let numbers: Vec<usize> = result.elements.iter().map(|e| e.number).collect();
        assert_eq!(
            numbers,
            (1..=numbers.len()).collect::<Vec<_>>(),
            "numbers must be 1..N with no gaps"
        );
        assert_eq!(numbers.len(), 50, "capped at max_addressable");

        let highest = |scene: &crate::screen::text_scene::TextScene| -> usize {
            scene
                .description
                .lines()
                .filter_map(|l| l.strip_prefix('[')?.split(']').next()?.parse().ok())
                .max()
                .unwrap_or(0)
        };
        let builder = crate::screen::text_scene::TextSceneBuilder::new(400, 4000);
        assert_eq!(
            highest(&builder.build(&elems)),
            50,
            "build() must not out-run the annotator"
        );

        // build_with_ids is the OODA path: it receives pre-numbered elements that
        // were never capped upstream, so the cap has to live here too. Numbers are
        // caller-assigned there (registry ids are not 1..N), so the invariant is
        // how many the scene lists, not the highest number it prints.
        let uncapped: Vec<(usize, &UiElement)> =
            elems.iter().enumerate().map(|(i, e)| (i + 1, e)).collect();
        let listed = builder
            .build_with_ids(&uncapped)
            .description
            .lines()
            .filter(|l| l.starts_with('['))
            .count();
        assert_eq!(
            listed, 50,
            "build_with_ids must cap too, or the scene lists elements with no tap target"
        );
    }

    #[test]
    fn the_unmarked_frame_is_kept_for_comparison() {
        // Swipe stuck-detection hashed the annotated JPEG and compared it to a
        // later raw PNG: two encodings of two different images, so it never
        // matched and every swipe claimed the screen had changed.
        let png = make_test_png(200, 200);
        let elems = vec![make_test_element("Go", Bounds::new(10, 10, 60, 40))];
        let a = ScreenAnnotator::new()
            .annotate(&png, &elems, (200, 200))
            .unwrap();

        assert_eq!(
            a.source_frame, png,
            "the frame must be kept exactly as captured"
        );
        assert_ne!(
            a.source_frame, a.image_data,
            "the annotated image is a different encoding of a different picture"
        );
        assert!(
            crate::screen::optimize::frames_settled(&a.source_frame, &png),
            "an unchanged screen must compare equal"
        );
    }

    #[test]
    fn marker_is_drawn_beside_the_element_not_on_its_text() {
        // End-to-end on the DRAWN pixels: the algorithm tests below call
        // place_marker directly, so they stay green if annotate stops calling it.
        // This one fails the moment the marker lands back on the text.
        let png = make_test_png(800, 800);
        // Two elements sharing bounds: if annotate stops calling place_marker they
        // both land on the centre, which the assertions below catch.
        let elems = vec![
            make_test_element("Label", Bounds::new(200, 300, 600, 360)),
            make_test_element("Other", Bounds::new(200, 300, 600, 360)),
        ];
        let result = ScreenAnnotator::new()
            .annotate(&png, &elems, (800, 800))
            .unwrap();
        let out = image::load_from_memory(&result.image_data)
            .unwrap()
            .to_rgb8();

        let is_red = |x: i32, y: i32| {
            x >= 0 && y >= 0 && (x as u32) < out.width() && (y as u32) < out.height() && {
                let p = out.get_pixel(x as u32, y as u32).0;
                p[0] > 150 && p[1] < 110 && p[2] < 110
            }
        };
        let red_near = |cx: i32, cy: i32, r: i32| {
            (-r..=r).any(|dy| (-r..=r).any(|dx| is_red(cx + dx, cy + dy)))
        };

        // The element centre is where the text is. Nothing may be painted there.
        assert!(
            !red_near(400, 330, 10),
            "marker must not cover the element's text"
        );
        // It should be off to the left, outside the element box.
        assert!(
            (0..200).any(|x| (300..360).any(|y| is_red(x, y))),
            "marker should be drawn beside the element"
        );
    }

    #[test]
    fn place_marker_spreads_identical_boxes() {
        // Six elements sharing bounds exhaust every preferred position, and the
        // first design piled all six markers on one pixel.
        let radius = 20;
        let mut placed: Vec<MarkerRect> = Vec::new();
        for _ in 0..6 {
            let (x, y) = place_marker(100, 100, 300, radius, 400, 800, &placed);
            let rect = MarkerRect::around(x, y, radius);
            assert!(
                !placed.iter().any(|p| rect.intersects(p)),
                "every marker must find its own space"
            );
            placed.push(rect);
        }
    }

    #[test]
    fn marker_avoids_the_element_content_box() {
        // A marker drawn at the centre covers the text it labels, which is what
        // made typography unjudgeable. With room on the left it must go there.
        let radius = 20;
        let (cx, _cy) = place_marker(200, 200, 400, radius, 1000, 1000, &[]);
        assert!(
            cx < 200,
            "marker should sit left of the element, got x={cx}"
        );
    }

    #[test]
    fn marker_stays_on_canvas_when_every_side_is_blocked() {
        // An element hugging the top-left with no room on any side must still get
        // a marker: dropping it would silently lose the element's number.
        let radius = 20;
        let (cx, cy) = place_marker(0, 0, 10, radius, 100, 100, &[]);
        assert!(cx >= radius && cy >= radius, "marker fell off canvas");
        assert!(
            cx <= 100 - radius && cy <= 100 - radius,
            "marker fell off canvas"
        );
    }

    fn ctx(device: &str) -> ObservationContext {
        ObservationContext {
            device: device.to_string(),
            package: "com.app".to_string(),
            activity: "com.app/.Main".to_string(),
            written_at: "2026-08-25T10:00:00Z".to_string(),
            assignments: Vec::new(),
        }
    }

    #[test]
    fn persisted_observation_round_trip_carries_provenance() {
        let dir = std::env::temp_dir().join(format!("drengr-pe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let png = make_test_png(200, 200);
        let elems = vec![make_test_element("Submit", Bounds::new(10, 20, 60, 50))];
        let annotated = ScreenAnnotator::new()
            .annotate(&png, &elems, (200, 200))
            .unwrap();

        ScreenAnnotator::persist_observation_in(&dir, &ctx("emulator-5554"), &annotated.elements);
        let back = ScreenAnnotator::persisted_observation_in(&dir).expect("record");

        // Without these a stale file resolves as confidently as a fresh one.
        assert_eq!(back.device, "emulator-5554");
        assert_eq!(back.activity, "com.app/.Main");
        assert_eq!(back.written_at, "2026-08-25T10:00:00Z");

        assert_eq!(back.elements.len(), 1);
        assert_eq!(back.elements[0].label, "Submit");
        assert_eq!(back.elements[0].number, annotated.elements[0].number);
        assert_eq!(back.elements[0].tap_x, annotated.elements[0].tap_x);
        assert_eq!(
            back.elements[0].fingerprint,
            Some(elems[0].fingerprint()),
            "identity must survive so a cold process can reuse the number"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_record_only_answers_for_the_device_it_was_written_on() {
        // Coordinates are in each device's own logical space, so resolving a
        // record from another device always succeeds and is always wrong. That is
        // the failure that cannot be noticed: the tap lands somewhere plausible.
        let dir = std::env::temp_dir().join(format!("drengr-dev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let png = make_test_png(200, 200);
        let elems = vec![make_test_element("Submit", Bounds::new(10, 20, 60, 50))];
        let annotated = ScreenAnnotator::new()
            .annotate(&png, &elems, (200, 200))
            .unwrap();
        ScreenAnnotator::persist_observation_in(&dir, &ctx("emulator-5554"), &annotated.elements);

        assert!(
            ScreenAnnotator::observation_for_device_in(&dir, "emulator-5554").is_some(),
            "the device that wrote it must be able to read it"
        );
        assert!(
            ScreenAnnotator::observation_for_device_in(&dir, "RF8WC0SEPKF").is_none(),
            "another device must never resolve these coordinates"
        );
        assert!(
            ScreenAnnotator::observation_for_device_in(&dir, "").is_none(),
            "an unverifiable device must be refused, not trusted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persisted_observation_reads_a_file_written_before_provenance() {
        // A bare array is what earlier versions wrote. Those are still valid tap
        // targets, so read them rather than discarding the screen.
        let dir = std::env::temp_dir().join(format!("drengr-old-{}", std::process::id()));
        let path = ScreenAnnotator::tap_targets_path_in(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"[{"n":1,"x":2,"y":3}]"#).unwrap();

        let back = ScreenAnnotator::persisted_observation_in(&dir).expect("record");
        assert_eq!(back.elements.len(), 1);
        assert_eq!(back.elements[0].label, "");
        assert_eq!(back.elements[0].fingerprint, None);
        assert_eq!((back.elements[0].tap_x, back.elements[0].tap_y), (2, 3));
        assert_eq!(
            back.device, "",
            "no provenance to report, and it must not invent any"
        );
        assert_eq!(
            ScreenAnnotator::tap_coordinates_from_disk_in(&dir, 1),
            Some((2, 3))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn make_test_element(text: &str, bounds: Bounds) -> UiElement {
        UiElement {
            class: "android.widget.Button".to_string(),
            text: text.to_string(),
            content_desc: String::new(),
            resource_id: String::new(),
            bounds,
            clickable: true,
            editable: false,
            is_password: false,
            focused: false,
            scrollable: false,
            enabled: true,
            visible: true,
            checked: false,
            selected: false,
            package: "com.test".to_string(),
        }
    }

    fn make_test_png(width: u32, height: u32) -> Vec<u8> {
        let img = RgbaImage::from_pixel(width, height, Rgba([200, 200, 200, 255]));
        let mut buf = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn test_annotate_basic() {
        let png = make_test_png(200, 400);
        let elements = vec![
            make_test_element("Login", Bounds::new(50, 100, 150, 140)),
            make_test_element("Cancel", Bounds::new(50, 200, 150, 240)),
        ];

        let annotator = ScreenAnnotator::new();
        let result = annotator.annotate(&png, &elements, (0, 0)).unwrap();

        assert_eq!(result.elements.len(), 2);
        assert_eq!(result.elements[0].number, 1);
        assert_eq!(result.elements[0].tap_x, 100);
        assert_eq!(result.elements[0].tap_y, 120);
        assert_eq!(result.elements[1].number, 2);
        assert!(!result.image_data.is_empty());
    }

    #[test]
    fn test_annotate_filters_non_interactive() {
        let png = make_test_png(200, 400);
        // Non-interactive WITH text is now included (read-only context)
        let mut labeled_non_interactive = make_test_element("Label", Bounds::new(0, 0, 100, 50));
        labeled_non_interactive.clickable = false;

        // Non-interactive WITHOUT text is still filtered out
        let mut empty_non_interactive = make_test_element("", Bounds::new(0, 60, 100, 80));
        empty_non_interactive.clickable = false;

        let elements = vec![
            make_test_element("Login", Bounds::new(50, 100, 150, 140)),
            labeled_non_interactive,
            empty_non_interactive,
        ];

        let annotator = ScreenAnnotator::new();
        let result = annotator.annotate(&png, &elements, (0, 0)).unwrap();

        // Login (interactive) + Label (read-only with text) = 2; empty excluded
        assert_eq!(result.elements.len(), 2);
    }

    #[test]
    fn test_annotate_respects_max_elements() {
        let png = make_test_png(200, 2000);
        let elements: Vec<UiElement> = (0..100)
            .map(|i| {
                make_test_element(
                    &format!("Btn{}", i),
                    Bounds::new(0, i * 20, 100, i * 20 + 18),
                )
            })
            .collect();

        let annotator = ScreenAnnotator::new().with_max_elements(5);
        let result = annotator.annotate(&png, &elements, (0, 0)).unwrap();

        assert_eq!(result.elements.len(), 5);
    }

    #[test]
    fn test_tap_coordinates_found() {
        let png = make_test_png(200, 400);
        let elements = vec![
            make_test_element("A", Bounds::new(10, 10, 90, 50)),
            make_test_element("B", Bounds::new(10, 100, 90, 140)),
        ];

        let annotator = ScreenAnnotator::new();
        let result = annotator.annotate(&png, &elements, (0, 0)).unwrap();

        let coords = ScreenAnnotator::tap_coordinates(&result, 2).unwrap();
        assert_eq!(coords, (50, 120));
    }

    #[test]
    fn test_tap_coordinates_not_found() {
        let png = make_test_png(200, 400);
        let elements = vec![make_test_element("A", Bounds::new(10, 10, 90, 50))];

        let annotator = ScreenAnnotator::new();
        let result = annotator.annotate(&png, &elements, (0, 0)).unwrap();

        assert!(ScreenAnnotator::tap_coordinates(&result, 99).is_none());
    }

    #[test]
    fn test_annotate_empty_elements() {
        let png = make_test_png(200, 400);
        let annotator = ScreenAnnotator::new();
        let result = annotator.annotate(&png, &[], (0, 0)).unwrap();

        assert!(result.elements.is_empty());
        assert!(!result.image_data.is_empty()); // Still produces a JPEG
    }

    #[test]
    fn test_output_is_jpeg() {
        let png = make_test_png(100, 100);
        let annotator = ScreenAnnotator::new();
        let result = annotator.annotate(&png, &[], (0, 0)).unwrap();

        // JPEG magic bytes: FF D8 FF
        assert!(result.image_data.len() > 3);
        assert_eq!(result.image_data[0], 0xFF);
        assert_eq!(result.image_data[1], 0xD8);
    }

    #[test]
    fn test_overlay_scales_to_image_but_tap_coords_stay_native() {
        // Bounds are in logical/native space (e.g. iOS points: 100x200 screen).
        // The screenshot is 3x physical pixels (300x600) — a Retina capture, or
        // it could be a downscaled image. The overlay must follow the image
        // resolution, but tap_x/tap_y MUST stay in native space for the device.
        let png = make_test_png(300, 600);
        let elements = vec![make_test_element("Buy", Bounds::new(20, 40, 60, 80))];
        let annotator = ScreenAnnotator::new();
        let result = annotator.annotate(&png, &elements, (100, 200)).unwrap();

        // Native center is (40, 60) — unchanged by the 3x image.
        assert_eq!(result.elements[0].tap_x, 40);
        assert_eq!(result.elements[0].tap_y, 60);
        assert!(!result.image_data.is_empty());
    }

    #[test]
    fn test_dots_land_scaled_not_clustered_top_left() {
        // Reproduces the real-device bug: element bounds in 402x874 POINTS, but
        // the screenshot is 3x physical pixels (1206x2622). Before the fix, dots
        // were drawn at raw point coords → all clustered in the top-left third.
        // After, they map into pixel space (x3) and spread correctly.
        let png = make_test_png(1206, 2622);
        let elems = vec![
            make_test_element("TL", Bounds::new(20, 20, 60, 60)), // center (40,40)
            make_test_element("BR", Bounds::new(340, 810, 380, 850)), // center (360,830)
            make_test_element("C", Bounds::new(181, 417, 221, 457)), // center (201,437)
        ];
        let annotator = ScreenAnnotator::new();
        let result = annotator.annotate(&png, &elems, (402, 874)).unwrap();

        // tap coords stay native (points), unaffected by the 3x image.
        let br = result
            .elements
            .iter()
            .find(|e| e.element.text == "BR")
            .unwrap();
        assert_eq!((br.tap_x, br.tap_y), (360, 830));

        // Decode and confirm the BR dot is at the SCALED pixel (1080, 2490),
        // and NOT at the old un-scaled (360, 830) which sat in the top-left.
        let out = image::load_from_memory(&result.image_data)
            .unwrap()
            .to_rgb8();
        let red_near = |cx: u32, cy: u32| {
            for dy in -12i32..=12 {
                for dx in -12i32..=12 {
                    let (x, y) = (cx as i32 + dx, cy as i32 + dy);
                    if x >= 0 && y >= 0 && (x as u32) < out.width() && (y as u32) < out.height() {
                        let p = out.get_pixel(x as u32, y as u32).0;
                        if p[0] > 150 && p[1] < 110 && p[2] < 110 {
                            return true;
                        }
                    }
                }
            }
            false
        };
        // The marker sits beside its element rather than on it, so assert it lands
        // within reach of the element's SCALED box rather than at one exact point.
        // The scaling invariant is what this test exists to protect.
        let red_in = |l: i32, t: i32, r: i32, b: i32| {
            (t..b).step_by(4).any(|y| {
                (l..r).step_by(4).any(|x| {
                    x >= 0 && y >= 0 && (x as u32) < out.width() && (y as u32) < out.height() && {
                        let p = out.get_pixel(x as u32, y as u32).0;
                        p[0] > 150 && p[1] < 110 && p[2] < 110
                    }
                })
            })
        };
        // BR bounds scaled 3x are (1020,2430)-(1140,2550); allow a marker on any side.
        assert!(
            red_in(900, 2310, 1260, 2670),
            "BR marker must land beside its 3x-scaled box"
        );
        assert!(
            !red_near(360, 830),
            "BR dot must NOT be at the old top-left position"
        );

        // Visual before/after when DRENGR_DEMO_OUT is set (off by default).
        if let Ok(dir) = std::env::var("DRENGR_DEMO_OUT") {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(format!("{dir}/fixed.jpg"), &result.image_data);
            let old = annotator.annotate(&png, &elems, (0, 0)).unwrap(); // (0,0)=no scale=old bug
            let _ = std::fs::write(format!("{dir}/old_bug.jpg"), &old.image_data);
        }
    }

    #[test]
    fn test_zero_logical_size_means_no_rescale() {
        // (0, 0) = "logical size unknown" → 1:1, identical to the pre-scale path.
        let png = make_test_png(200, 400);
        let elements = vec![make_test_element("X", Bounds::new(10, 10, 90, 50))];
        let annotator = ScreenAnnotator::new();
        let result = annotator.annotate(&png, &elements, (0, 0)).unwrap();
        assert_eq!(result.elements[0].tap_x, 50);
        assert_eq!(result.elements[0].tap_y, 30);
    }
}
