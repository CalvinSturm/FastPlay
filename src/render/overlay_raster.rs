//! CPU rasterization for the on-screen overlays: straight-alpha compositing and
//! the filled shapes the overlays are built from.
//!
//! Extracted from `ffi/d3d11.rs` (see `docs/audits/codebase-review.md` §10
//! Stage 3). None of this is FFI: it operates on a plain BGRA byte buffer, calls
//! no COM and no GDI, and contains no `unsafe`. It lived in the D3D11 seam only
//! because that is where the texture-upload code that consumes the pixels lives,
//! and it was untestable there.
//!
//! The seam keeps everything that genuinely needs Win32 — the GDI text
//! measurement and glyph rasterization in `render_*_bitmap` /
//! `draw_timeline_label` — and calls in here for the geometry.
//!
//! Buffers are tightly packed BGRA8, `width * height * 4` bytes, row-major with
//! no padding, which is what `CreateTexture2D` is handed directly.

use crate::render::timeline::{self, TimelineOverlayModel, TIMELINE_HEIGHT_PX};

/// Narrowest timeline the layout math is valid for.
///
/// Below this the track's side padding exceeds the viewport, so
/// [`timeline::layout`] returns `track_left > track_right` and the clamps in
/// [`render_timeline_shapes`] would panic (`std::clamp` panics when `min > max`).
/// The window enforces a 640px minimum client width through `WM_GETMINMAXINFO`,
/// so this is unreachable in the running app — it is asserted rather than
/// clamped so that a future change breaking that guarantee fails loudly in debug
/// instead of silently rendering a degenerate overlay.
pub(crate) const MIN_TIMELINE_WIDTH_PX: u32 = 32;

/// Composite `src` over `dest` (both straight-alpha BGRA8, 4 bytes each).
pub(crate) fn blend_pixel(dest: &mut [u8], src: [u8; 4]) {
    let sa = src[3] as u32;
    if sa == 0 {
        return;
    }
    if sa == 255 || dest[3] == 0 {
        dest.copy_from_slice(&src);
        return;
    }
    let da = dest[3] as u32;
    let out_a = sa + da - (sa * da / 255);
    if out_a == 0 {
        return;
    }
    for i in 0..3 {
        dest[i] = ((src[i] as u32 * sa + dest[i] as u32 * da * (255 - sa) / 255) / out_a) as u8;
    }
    dest[3] = out_a as u8;
}

/// Fill the half-open rect `[left, right) x [top, bottom)` with `color`,
/// replacing rather than blending. Coordinates are clipped to the buffer.
pub(crate) fn fill_rect(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
    color: [u8; 4],
) {
    let left = left.min(width);
    let right = right.min(width);
    let top = top.min(height);
    let bottom = bottom.min(height);

    for y in top..bottom {
        for x in left..right {
            let offset = ((y * width + x) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
}

/// Blend an anti-aliased filled circle, with a one-pixel coverage fringe.
pub(crate) fn fill_circle_aa(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    cx: f32,
    cy: f32,
    radius: f32,
    color: [u8; 4],
) {
    let r_outer = radius + 0.5;
    let min_x = (cx - r_outer).floor().max(0.0) as u32;
    let max_x = ((cx + r_outer).ceil() as u32).min(width.saturating_sub(1));
    let min_y = (cy - r_outer).floor().max(0.0) as u32;
    let max_y = ((cy + r_outer).ceil() as u32).min(height.saturating_sub(1));

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > r_outer {
                continue;
            }
            // Smooth edge: 1px anti-alias fringe.
            let coverage = (radius - dist + 0.5).clamp(0.0, 1.0);
            let alpha = (color[3] as f32 * coverage) as u8;
            let offset = ((y * width + x) * 4) as usize;
            blend_pixel(
                &mut pixels[offset..offset + 4],
                [color[0], color[1], color[2], alpha],
            );
        }
    }
}

/// Blend a filled rounded rectangle (a pill when `radius` reaches half the
/// height), anti-aliased on all four corners and edges.
pub(crate) fn fill_rounded_rect(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
    radius: f32,
    color: [u8; 4],
) {
    let left = left.min(width);
    let right = right.min(width);
    let top = top.min(height);
    let bottom = bottom.min(height);
    let rect_h = bottom.saturating_sub(top) as f32;
    let rect_w = right.saturating_sub(left) as f32;
    let r = radius.min(rect_h / 2.0).min(rect_w / 2.0);

    let min_x = left.saturating_sub(1);
    let max_x = (right + 1).min(width);
    let min_y = top.saturating_sub(1);
    let max_y = (bottom + 1).min(height);

    for y in min_y..max_y {
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            let inner_x = px >= left as f32 + r && px <= right as f32 - r;
            let inner_y = py >= top as f32 + r && py <= bottom as f32 - r;

            let coverage = if inner_x && inner_y {
                // Fully inside.
                1.0
            } else if inner_x {
                // Top or bottom edge.
                let cy = if py < top as f32 + r {
                    top as f32 + r
                } else {
                    bottom as f32 - r
                };
                let dist = (py - cy).abs();
                (r - dist + 0.5).clamp(0.0, 1.0)
            } else if inner_y {
                // Left or right edge.
                let cx = if px < left as f32 + r {
                    left as f32 + r
                } else {
                    right as f32 - r
                };
                let dist = (px - cx).abs();
                (r - dist + 0.5).clamp(0.0, 1.0)
            } else {
                // Corner — distance from corner circle center.
                let cx = if px < left as f32 + r {
                    left as f32 + r
                } else {
                    right as f32 - r
                };
                let cy = if py < top as f32 + r {
                    top as f32 + r
                } else {
                    bottom as f32 - r
                };
                let dist = ((px - cx) * (px - cx) + (py - cy) * (py - cy)).sqrt();
                (r - dist + 0.5).clamp(0.0, 1.0)
            };

            if coverage <= 0.0 {
                continue;
            }

            let alpha = (color[3] as f32 * coverage) as u8;
            let offset = ((y * width + x) * 4) as usize;
            blend_pixel(
                &mut pixels[offset..offset + 4],
                [color[0], color[1], color[2], alpha],
            );
        }
    }
}

/// A rasterized overlay layer: tightly packed BGRA8, `width * height * 4` bytes.
pub(crate) struct RasterizedShapes {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Rasterize the timeline overlay's shapes — gradient backdrop, track, in/out
/// range, played fill, marker ticks, and handle — but **not** its text labels,
/// which need GDI and stay in the D3D11 seam (`draw_timeline_label`).
///
/// Returns `None` for a degenerate model, matching the previous behavior.
/// Callers draw the labels on top of the returned buffer, so the compositing
/// order is unchanged: shapes first, then text.
pub(crate) fn render_timeline_shapes(model: &TimelineOverlayModel) -> Option<RasterizedShapes> {
    if model.viewport_width == 0 || model.viewport_height == 0 || model.duration_secs == 0 {
        return None;
    }

    let width = model.viewport_width;
    debug_assert!(
        width >= MIN_TIMELINE_WIDTH_PX,
        "timeline width {width}px is below the {MIN_TIMELINE_WIDTH_PX}px the layout math \
         supports; the window enforces a 640px minimum client width, so reaching here means \
         that guarantee broke"
    );
    let height = TIMELINE_HEIGHT_PX;
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let layout = timeline::layout(model.viewport_width, model.viewport_height);
    let track_top = (layout.track_top - layout.top).max(0) as u32;
    let track_bottom = (layout.track_bottom - layout.top).max(track_top as i32 + 1) as u32;
    let track_left = layout.track_left.max(0) as u32;
    let track_right = layout.track_right.max(layout.track_left + 1) as u32;
    let track_cy = track_top + (track_bottom - track_top) / 2;
    let track_half_h = ((track_bottom - track_top) as f32) / 2.0;

    // Gradient background: transparent at top, semi-opaque at bottom.
    for y in 0..height {
        let t = y as f32 / height.max(1) as f32;
        let alpha = (t * t * 180.0) as u8;
        for x in 0..width {
            let offset = ((y * width + x) * 4) as usize;
            pixels[offset] = 0;
            pixels[offset + 1] = 0;
            pixels[offset + 2] = 0;
            pixels[offset + 3] = alpha;
        }
    }

    // Unplayed track — rounded pill shape, dim.
    fill_rounded_rect(
        &mut pixels,
        width,
        height,
        track_left,
        track_top,
        track_right,
        track_bottom,
        track_half_h,
        [255, 255, 255, 60],
    );

    // In/out range fill — drawn before the played track so the bright played portion
    // sits on top of it; only shown when both markers are set.
    if let (Some(ix), Some(ox)) = (model.in_point_marker_x, model.out_point_marker_x) {
        let range_left = ix.max(0) as u32;
        let range_right = ox.max(0) as u32;
        if range_right > range_left {
            fill_rounded_rect(
                &mut pixels,
                width,
                height,
                range_left,
                track_top,
                range_right,
                track_bottom,
                track_half_h,
                [60, 160, 255, 130],
            );
        }
    }

    // Played track — bright pill starting at the in-point (if set) so the region
    // before I reads as dim/excluded rather than as played content.
    let played_left = model
        .in_point_marker_x
        .map_or(track_left, |ix| (ix.max(0) as u32).max(track_left));
    let played_right = (track_left + model.played_px).min(track_right);
    if played_right > played_left {
        fill_rounded_rect(
            &mut pixels,
            width,
            height,
            played_left,
            track_top,
            played_right,
            track_bottom,
            track_half_h,
            [255, 255, 255, 230],
        );
    }

    // In/out marker ticks — 2px-wide white vertical bars slightly taller than the track.
    let marker_top = track_top.saturating_sub(3);
    let marker_bottom = (track_bottom + 3).min(height);
    if let Some(x) = model.in_point_marker_x {
        let mx = x.clamp(0, width as i32 - 2) as u32;
        fill_rect(
            &mut pixels,
            width,
            height,
            mx,
            marker_top,
            mx + 2,
            marker_bottom,
            [255, 255, 255, 220],
        );
    }
    if let Some(x) = model.out_point_marker_x {
        let mx = (x - 1).clamp(0, width as i32 - 2) as u32;
        fill_rect(
            &mut pixels,
            width,
            height,
            mx,
            marker_top,
            mx + 2,
            marker_bottom,
            [255, 255, 255, 220],
        );
    }

    // Handle — anti-aliased white circle.
    let handle_cx = model
        .handle_center_x
        .clamp(layout.track_left, layout.track_right) as u32;
    fill_circle_aa(
        &mut pixels,
        width,
        height,
        handle_cx as f32,
        track_cy as f32,
        6.0,
        [255, 255, 255, 255],
    );

    Some(RasterizedShapes {
        width,
        height,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPAQUE_WHITE: [u8; 4] = [255, 255, 255, 255];
    const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];

    fn buffer(width: u32, height: u32) -> Vec<u8> {
        vec![0u8; (width * height * 4) as usize]
    }

    fn px(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let o = ((y * width + x) * 4) as usize;
        [pixels[o], pixels[o + 1], pixels[o + 2], pixels[o + 3]]
    }

    // ── blend_pixel ────────────────────────────────────────────────────────

    #[test]
    fn blending_a_fully_transparent_source_is_a_no_op() {
        let mut dest = [10, 20, 30, 40];
        blend_pixel(&mut dest, [255, 255, 255, 0]);
        assert_eq!(dest, [10, 20, 30, 40]);
    }

    #[test]
    fn an_opaque_source_replaces_the_destination() {
        let mut dest = [10, 20, 30, 40];
        blend_pixel(&mut dest, OPAQUE_WHITE);
        assert_eq!(dest, OPAQUE_WHITE);
    }

    #[test]
    fn blending_over_a_transparent_destination_copies_the_source() {
        // Straight alpha: an empty destination has no color to preserve, so the
        // source is taken verbatim rather than being scaled toward black.
        let mut dest = TRANSPARENT;
        blend_pixel(&mut dest, [90, 120, 200, 128]);
        assert_eq!(dest, [90, 120, 200, 128]);
    }

    #[test]
    fn partial_alpha_over_opaque_stays_between_the_two_colors() {
        let mut dest = [0, 0, 0, 255];
        blend_pixel(&mut dest, [200, 200, 200, 128]);
        assert_eq!(
            dest[3], 255,
            "over an opaque destination alpha stays opaque"
        );
        for (channel, &value) in dest.iter().take(3).enumerate() {
            assert!(
                value > 0 && value < 200,
                "channel {channel} = {value} should land between the two colors"
            );
        }
    }

    #[test]
    fn alpha_accumulates_monotonically_across_repeated_blends() {
        let mut dest = TRANSPARENT;
        let mut previous = 0u8;
        for _ in 0..8 {
            blend_pixel(&mut dest, [255, 255, 255, 64]);
            assert!(
                dest[3] >= previous,
                "alpha must never decrease: {} -> {}",
                previous,
                dest[3]
            );
            previous = dest[3];
        }
        assert!(previous > 64, "repeated blends should build up alpha");
    }

    // ── fill_rect ──────────────────────────────────────────────────────────

    #[test]
    fn fill_rect_covers_exactly_the_half_open_range() {
        let (w, h) = (8, 8);
        let mut pixels = buffer(w, h);
        fill_rect(&mut pixels, w, h, 2, 3, 5, 6, OPAQUE_WHITE);
        for y in 0..h {
            for x in 0..w {
                let inside = (2..5).contains(&x) && (3..6).contains(&y);
                let expected = if inside { OPAQUE_WHITE } else { TRANSPARENT };
                assert_eq!(px(&pixels, w, x, y), expected, "at ({x},{y})");
            }
        }
    }

    #[test]
    fn fill_rect_clips_at_every_edge_without_panicking() {
        let (w, h) = (8, 8);
        for (l, t, r, b) in [
            (0, 0, 100, 100), // past right and bottom
            (6, 6, 8, 8),     // exactly the corner
            (5, 5, 2, 2),     // inverted: nothing drawn
            (8, 8, 12, 12),   // entirely outside
        ] {
            let mut pixels = buffer(w, h);
            fill_rect(&mut pixels, w, h, l, t, r, b, OPAQUE_WHITE);
            assert_eq!(pixels.len(), (w * h * 4) as usize, "buffer must not resize");
        }
    }

    #[test]
    fn fill_rect_with_an_inverted_rect_draws_nothing() {
        let (w, h) = (4, 4);
        let mut pixels = buffer(w, h);
        fill_rect(&mut pixels, w, h, 3, 3, 1, 1, OPAQUE_WHITE);
        assert!(pixels.iter().all(|&b| b == 0));
    }

    // ── fill_rounded_rect ──────────────────────────────────────────────────

    #[test]
    fn rounded_rect_center_is_fully_covered_and_corners_are_not() {
        let (w, h) = (40, 20);
        let mut pixels = buffer(w, h);
        fill_rounded_rect(&mut pixels, w, h, 4, 4, 36, 16, 6.0, OPAQUE_WHITE);

        assert_eq!(
            px(&pixels, w, 20, 10),
            OPAQUE_WHITE,
            "the middle of the pill is fully covered"
        );
        let corner = px(&pixels, w, 4, 4);
        assert!(
            corner[3] < 255,
            "the extreme corner is outside the rounding, alpha = {}",
            corner[3]
        );
    }

    #[test]
    fn rounded_rect_coverage_increases_toward_the_center_of_a_corner() {
        let (w, h) = (40, 20);
        let mut pixels = buffer(w, h);
        fill_rounded_rect(&mut pixels, w, h, 4, 4, 36, 16, 6.0, OPAQUE_WHITE);
        // Walking diagonally inward from the corner, coverage must not decrease.
        let mut previous = 0u8;
        for step in 0..6 {
            let alpha = px(&pixels, w, 4 + step, 4 + step)[3];
            assert!(
                alpha >= previous,
                "coverage dropped walking inward at step {step}: {previous} -> {alpha}"
            );
            previous = alpha;
        }
    }

    #[test]
    fn rounded_rect_stays_inside_the_buffer_when_flush_with_the_edges() {
        let (w, h) = (16, 16);
        let mut pixels = buffer(w, h);
        // Right/bottom flush with the buffer: the +1 expansion must still clip.
        fill_rounded_rect(&mut pixels, w, h, 0, 0, 16, 16, 4.0, OPAQUE_WHITE);
        assert_eq!(pixels.len(), (w * h * 4) as usize);
    }

    // ── fill_circle_aa ─────────────────────────────────────────────────────

    #[test]
    fn circle_is_opaque_at_the_center_and_clear_well_outside() {
        let (w, h) = (32, 32);
        let mut pixels = buffer(w, h);
        fill_circle_aa(&mut pixels, w, h, 16.0, 16.0, 6.0, OPAQUE_WHITE);
        assert_eq!(px(&pixels, w, 16, 16)[3], 255, "center is solid");
        assert_eq!(px(&pixels, w, 0, 0)[3], 0, "far corner untouched");
    }

    #[test]
    fn circle_clipped_against_an_edge_does_not_panic() {
        let (w, h) = (16, 16);
        for (cx, cy) in [(0.0, 0.0), (15.0, 15.0), (-5.0, 8.0), (20.0, 8.0)] {
            let mut pixels = buffer(w, h);
            fill_circle_aa(&mut pixels, w, h, cx, cy, 6.0, OPAQUE_WHITE);
            assert_eq!(pixels.len(), (w * h * 4) as usize);
        }
    }

    // ── render_timeline_shapes ─────────────────────────────────────────────

    fn model(width: u32, played_px: u32, handle_x: i32) -> TimelineOverlayModel {
        TimelineOverlayModel {
            viewport_width: width,
            viewport_height: 720,
            current_position_secs: 10,
            preview_position_secs: None,
            duration_secs: 100,
            played_px,
            handle_center_x: handle_x,
            loop_enabled: false,
            in_point_marker_x: None,
            out_point_marker_x: None,
        }
    }

    #[test]
    fn degenerate_models_produce_no_bitmap() {
        let mut m = model(1280, 100, 200);
        m.viewport_width = 0;
        assert!(render_timeline_shapes(&m).is_none());

        let mut m = model(1280, 100, 200);
        m.viewport_height = 0;
        assert!(render_timeline_shapes(&m).is_none());

        let mut m = model(1280, 100, 200);
        m.duration_secs = 0;
        assert!(
            render_timeline_shapes(&m).is_none(),
            "no duration, no track"
        );
    }

    #[test]
    fn buffer_is_tightly_packed_bgra_at_the_timeline_height() {
        let shapes = render_timeline_shapes(&model(1280, 400, 500)).expect("model is valid");
        assert_eq!(shapes.width, 1280);
        assert_eq!(shapes.height, TIMELINE_HEIGHT_PX);
        assert_eq!(
            shapes.pixels.len(),
            (shapes.width * shapes.height * 4) as usize
        );
    }

    #[test]
    fn played_region_is_brighter_than_the_unplayed_region() {
        // The played fill is alpha 230, the unplayed track alpha 60, so a point
        // inside the played span must be markedly more opaque than one past it.
        let shapes = render_timeline_shapes(&model(1280, 400, 400)).expect("valid");
        let layout = timeline::layout(1280, 720);
        let row = ((layout.track_top - layout.top).max(0) as u32) + 1;
        let played = px(
            &shapes.pixels,
            shapes.width,
            layout.track_left as u32 + 10,
            row,
        );
        let unplayed = px(
            &shapes.pixels,
            shapes.width,
            layout.track_left as u32 + 800,
            row,
        );
        assert!(
            played[3] > unplayed[3],
            "played alpha {} should exceed unplayed alpha {}",
            played[3],
            unplayed[3]
        );
    }

    #[test]
    fn a_zero_length_played_span_leaves_the_track_dim() {
        let shapes = render_timeline_shapes(&model(1280, 0, 0)).expect("valid");
        let layout = timeline::layout(1280, 720);
        let row = ((layout.track_top - layout.top).max(0) as u32) + 1;
        let sample = px(
            &shapes.pixels,
            shapes.width,
            layout.track_left as u32 + 40,
            row,
        );
        assert!(
            sample[3] < 200,
            "nothing is played, so the track must stay dim (alpha {})",
            sample[3]
        );
    }

    #[test]
    fn markers_are_drawn_where_the_model_places_them() {
        let mut m = model(1280, 400, 400);
        m.in_point_marker_x = Some(300);
        m.out_point_marker_x = Some(900);
        let shapes = render_timeline_shapes(&m).expect("valid");
        let layout = timeline::layout(1280, 720);
        let marker_row = ((layout.track_top - layout.top).max(0) as u32).saturating_sub(1);

        assert!(
            px(&shapes.pixels, shapes.width, 300, marker_row)[3] > 0,
            "in-point tick should be drawn at its x"
        );
        assert!(
            px(&shapes.pixels, shapes.width, 899, marker_row)[3] > 0,
            "out-point tick is drawn one pixel left of its x"
        );
    }

    #[test]
    fn shapes_render_at_the_minimum_supported_width() {
        // The debug assertion's boundary: this must not panic.
        let shapes = render_timeline_shapes(&model(MIN_TIMELINE_WIDTH_PX, 4, 16));
        assert!(shapes.is_some());
    }

    #[test]
    fn out_of_range_marker_and_handle_positions_are_clamped_not_panicked() {
        let mut m = model(1280, 400, 100_000);
        m.in_point_marker_x = Some(-500);
        m.out_point_marker_x = Some(99_999);
        let shapes = render_timeline_shapes(&m).expect("valid");
        assert_eq!(
            shapes.pixels.len(),
            (shapes.width * shapes.height * 4) as usize
        );
    }
}
