//! No text engine off macOS yet: every string rasterizes to nothing.

use super::{RasterizeOptions, RasterizedText};

#[derive(Default)]
pub struct TextRasterizer;

impl TextRasterizer {
    pub fn new() -> Self {
        Self
    }

    pub fn available_font_families() -> Vec<String> {
        Vec::new()
    }

    pub fn rasterize(
        &mut self,
        _text: &str,
        _font_size: f32,
        _options: &RasterizeOptions,
    ) -> Option<RasterizedText> {
        None
    }

    pub fn prewarm_font(&mut self, _family: &str) {}
}
