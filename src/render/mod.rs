pub mod presenter;
pub mod surface_registry;
pub mod swapchain;

/// View transform applied during presentation (zoom + pan).
#[derive(Clone, Copy, Debug)]
pub struct ViewTransform {
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl Default for ViewTransform {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }
}

impl ViewTransform {
    pub fn is_identity(&self) -> bool {
        self.zoom == 1.0 && self.pan_x == 0.0 && self.pan_y == 0.0
    }
}
