use std::time::Duration;

pub const TIMELINE_ACTIVATION_ZONE_PX: i32 = 48;
pub const TIMELINE_HEIGHT_PX: u32 = 44;

const TIMELINE_BOTTOM_MARGIN_PX: i32 = 0;
const TRACK_SIDE_PADDING_PX: i32 = 16;
const TRACK_TOP_PX: i32 = 28;
const TRACK_HEIGHT_PX: i32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelineLayout {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub track_left: i32,
    pub track_top: i32,
    pub track_right: i32,
    pub track_bottom: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelineOverlayModel {
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub current_position_secs: u64,
    pub preview_position_secs: Option<u64>,
    pub duration_secs: u64,
    pub played_px: u32,
    pub handle_center_x: i32,
    pub loop_enabled: bool,
    pub in_point_marker_x: Option<i32>,
    pub out_point_marker_x: Option<i32>,
}

pub fn activation_hit_test(viewport_width: u32, viewport_height: u32, x: i32, y: i32) -> bool {
    if viewport_width == 0 || viewport_height == 0 {
        return false;
    }

    x >= 0
        && x < viewport_width as i32
        && y >= viewport_height as i32 - TIMELINE_ACTIVATION_ZONE_PX
        && y < viewport_height as i32
}

pub fn scrub_hit_test(viewport_width: u32, viewport_height: u32, x: i32, y: i32) -> bool {
    let layout = layout(viewport_width, viewport_height);
    x >= layout.left && x < layout.right && y >= layout.top && y < layout.bottom
}

pub fn scrub_target_from_cursor(
    viewport_width: u32,
    viewport_height: u32,
    duration: Duration,
    cursor_x: i32,
) -> Duration {
    let layout = layout(viewport_width, viewport_height);
    let track_width = (layout.track_right - layout.track_left).max(1) as f64;
    let clamped_x = cursor_x.clamp(layout.track_left, layout.track_right);
    let ratio = (clamped_x - layout.track_left) as f64 / track_width;
    Duration::from_secs_f64((duration.as_secs_f64() * ratio).clamp(0.0, duration.as_secs_f64()))
}

pub fn build_overlay_model(
    viewport_width: u32,
    viewport_height: u32,
    current_position: Duration,
    preview_position: Option<Duration>,
    duration: Duration,
    loop_enabled: bool,
    in_point: Option<Duration>,
    out_point: Option<Duration>,
) -> Option<TimelineOverlayModel> {
    if viewport_width == 0 || viewport_height == 0 || duration.is_zero() {
        return None;
    }

    let layout = layout(viewport_width, viewport_height);
    let track_width = (layout.track_right - layout.track_left).max(1) as u32;
    let indicator_position = preview_position.unwrap_or(current_position).min(duration);
    let played_ratio = indicator_position.as_secs_f64() / duration.as_secs_f64();
    let played_px = ((track_width as f64) * played_ratio.clamp(0.0, 1.0)).round() as u32;
    let played_px = played_px.min(track_width);

    let point_to_marker_x = |pt: Duration| -> i32 {
        let ratio = (pt.min(duration).as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0);
        layout.track_left + ((track_width as f64 * ratio).round() as i32)
    };

    Some(TimelineOverlayModel {
        viewport_width,
        viewport_height,
        current_position_secs: current_position.as_secs(),
        preview_position_secs: preview_position.map(|position| position.min(duration).as_secs()),
        duration_secs: duration.as_secs(),
        played_px,
        handle_center_x: layout.track_left + played_px as i32,
        loop_enabled,
        in_point_marker_x: in_point.map(point_to_marker_x),
        out_point_marker_x: out_point.map(point_to_marker_x),
    })
}

pub fn layout(viewport_width: u32, viewport_height: u32) -> TimelineLayout {
    let width = viewport_width as i32;
    let height = viewport_height as i32;
    let top = (height - TIMELINE_BOTTOM_MARGIN_PX - TIMELINE_HEIGHT_PX as i32).max(0);
    let bottom = (top + TIMELINE_HEIGHT_PX as i32).min(height);
    let track_left = TRACK_SIDE_PADDING_PX.min(width.saturating_sub(1));
    let track_right = (width - TRACK_SIDE_PADDING_PX).max(track_left + 1);
    let track_top = top + TRACK_TOP_PX;
    let track_bottom = (track_top + TRACK_HEIGHT_PX).min(bottom);

    TimelineLayout {
        left: 0,
        top,
        right: width.max(1),
        bottom,
        track_left,
        track_top,
        track_right,
        track_bottom,
    }
}

pub fn format_timestamp(total_seconds: u64) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 1280;
    const H: u32 = 720;

    fn dur_secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn activation_zone_covers_bottom_strip_only() {
        assert!(activation_hit_test(W, H, 0, H as i32 - 1));
        assert!(activation_hit_test(W, H, W as i32 - 1, H as i32 - TIMELINE_ACTIVATION_ZONE_PX));
        assert!(!activation_hit_test(W, H, 0, H as i32 - TIMELINE_ACTIVATION_ZONE_PX - 1));
        assert!(!activation_hit_test(W, H, -1, H as i32 - 1));
        assert!(!activation_hit_test(W, H, W as i32, H as i32 - 1));
    }

    #[test]
    fn activation_zone_false_for_zero_viewport() {
        assert!(!activation_hit_test(0, H, 0, 0));
        assert!(!activation_hit_test(W, 0, 0, 0));
    }

    #[test]
    fn scrub_hit_test_matches_layout() {
        let layout = layout(W, H);
        assert!(scrub_hit_test(W, H, layout.left, layout.top));
        assert!(scrub_hit_test(W, H, layout.right - 1, layout.bottom - 1));
        assert!(!scrub_hit_test(W, H, layout.right, layout.top));
        assert!(!scrub_hit_test(W, H, layout.left, layout.top - 1));
    }

    #[test]
    fn scrub_target_clamps_within_track() {
        let duration = dur_secs(100.0);
        let layout = layout(W, H);

        let left_of_track = scrub_target_from_cursor(W, H, duration, layout.track_left - 50);
        assert_eq!(left_of_track, Duration::ZERO);

        let right_of_track = scrub_target_from_cursor(W, H, duration, layout.track_right + 50);
        assert_eq!(right_of_track, duration);
    }

    #[test]
    fn scrub_target_midpoint_maps_to_half_duration() {
        let duration = dur_secs(100.0);
        let layout = layout(W, H);
        let mid_x = (layout.track_left + layout.track_right) / 2;
        let target = scrub_target_from_cursor(W, H, duration, mid_x);

        let delta = (target.as_secs_f64() - 50.0).abs();
        assert!(delta < 0.5, "midpoint mapped to {target:?}");
    }

    #[test]
    fn scrub_target_monotonic_in_cursor_x() {
        let duration = dur_secs(120.0);
        let layout = layout(W, H);
        let mut prev = Duration::ZERO;
        for x in (layout.track_left..=layout.track_right).step_by(10) {
            let t = scrub_target_from_cursor(W, H, duration, x);
            assert!(t >= prev, "non-monotonic at x={x}: {t:?} < {prev:?}");
            prev = t;
        }
    }

    #[test]
    fn overlay_model_rejects_invalid_inputs() {
        assert!(build_overlay_model(0, H, Duration::ZERO, None, dur_secs(10.0), false, None, None).is_none());
        assert!(build_overlay_model(W, 0, Duration::ZERO, None, dur_secs(10.0), false, None, None).is_none());
        assert!(build_overlay_model(W, H, Duration::ZERO, None, Duration::ZERO, false, None, None).is_none());
    }

    #[test]
    fn overlay_model_preview_overrides_current_for_handle() {
        let duration = dur_secs(100.0);
        let current = dur_secs(10.0);
        let preview = dur_secs(75.0);
        let with_preview =
            build_overlay_model(W, H, current, Some(preview), duration, false, None, None).unwrap();
        let without_preview =
            build_overlay_model(W, H, current, None, duration, false, None, None).unwrap();

        assert!(with_preview.played_px > without_preview.played_px);
        assert!(with_preview.handle_center_x > without_preview.handle_center_x);
        assert_eq!(with_preview.current_position_secs, 10);
        assert_eq!(with_preview.preview_position_secs, Some(75));
    }

    #[test]
    fn overlay_model_clamps_over_duration() {
        let duration = dur_secs(30.0);
        let model = build_overlay_model(
            W,
            H,
            dur_secs(999.0),
            Some(dur_secs(9999.0)),
            duration,
            true,
            Some(dur_secs(10.0)),
            Some(dur_secs(20.0)),
        )
        .unwrap();

        let layout = layout(W, H);
        let track_width = (layout.track_right - layout.track_left) as u32;
        assert_eq!(model.played_px, track_width);
        assert_eq!(model.handle_center_x, layout.track_right);
        assert_eq!(model.preview_position_secs, Some(30));
    }

    #[test]
    fn overlay_model_markers_positioned_proportionally() {
        let duration = dur_secs(100.0);
        let layout = layout(W, H);
        let track_width = layout.track_right - layout.track_left;
        let model = build_overlay_model(
            W,
            H,
            Duration::ZERO,
            None,
            duration,
            false,
            Some(dur_secs(25.0)),
            Some(dur_secs(75.0)),
        )
        .unwrap();

        let in_x = model.in_point_marker_x.unwrap();
        let out_x = model.out_point_marker_x.unwrap();
        let expected_in = layout.track_left + (track_width as f64 * 0.25).round() as i32;
        let expected_out = layout.track_left + (track_width as f64 * 0.75).round() as i32;
        assert_eq!(in_x, expected_in);
        assert_eq!(out_x, expected_out);
    }

    #[test]
    fn overlay_model_dedup_is_pixel_granular() {
        let duration = dur_secs(100.0);
        let layout = layout(W, H);
        let track_width = (layout.track_right - layout.track_left) as f64;
        let sub_pixel_offset = duration.as_secs_f64() / (track_width * 4.0);
        let a =
            build_overlay_model(W, H, dur_secs(10.0), None, duration, false, None, None).unwrap();
        let b = build_overlay_model(
            W,
            H,
            dur_secs(10.0 + sub_pixel_offset),
            None,
            duration,
            false,
            None,
            None,
        )
        .unwrap();
        assert_eq!(a.played_px, b.played_px);
        assert_eq!(a, b);
    }

    #[test]
    fn format_timestamp_below_hour_is_mm_ss() {
        assert_eq!(format_timestamp(0), "00:00");
        assert_eq!(format_timestamp(59), "00:59");
        assert_eq!(format_timestamp(600), "10:00");
        assert_eq!(format_timestamp(3599), "59:59");
    }

    #[test]
    fn format_timestamp_at_or_above_hour_is_h_mm_ss() {
        assert_eq!(format_timestamp(3600), "1:00:00");
        assert_eq!(format_timestamp(3661), "1:01:01");
        assert_eq!(format_timestamp(36000), "10:00:00");
    }
}
