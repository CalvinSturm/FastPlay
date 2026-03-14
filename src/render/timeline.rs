use std::time::Duration;

pub const TIMELINE_ACTIVATION_ZONE_PX: i32 = 56;
pub const TIMELINE_HEIGHT_PX: u32 = 52;

const TIMELINE_BOTTOM_MARGIN_PX: i32 = 0;
const TRACK_SIDE_PADDING_PX: i32 = 16;
const TRACK_TOP_PX: i32 = 32;
const TRACK_HEIGHT_PX: i32 = 8;

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

    Some(TimelineOverlayModel {
        viewport_width,
        viewport_height,
        current_position_secs: current_position.as_secs(),
        preview_position_secs: preview_position.map(|position| position.min(duration).as_secs()),
        duration_secs: duration.as_secs(),
        played_px,
        handle_center_x: layout.track_left + played_px as i32,
        loop_enabled,
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
