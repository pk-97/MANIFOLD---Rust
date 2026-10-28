//! Presentation to a window (D15, P3). Not built yet.

// Stub bodies name the phase that builds them (design section 9); their
// parameters are unused until then.
#![allow(unused_variables)]

use super::*;
use crate::*;

pub struct GpuSurface {
    pub width: u32,
    pub height: u32,
    pub format: GpuTextureFormat,
}

pub struct GpuDrawable {
    _reserved: (),
}

/// Install the one presented-time observer. Returns false if one is
/// already installed.
pub fn set_presented_observer(observer: fn(u64, f64)) -> bool {
    todo!("vulkan P3")
}

impl GpuDevice {
    /// Create a presentable surface backed by a CAMetalLayer.
    pub fn create_surface(
        &self,
        window: &impl raw_window_handle::HasWindowHandle,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        vsync: bool,
    ) -> GpuSurface {
        todo!("vulkan P3")
    }
}

impl GpuSurface {
    /// Resize the drawable surface.
    pub fn resize(&mut self, width: u32, height: u32) {
        todo!("vulkan P3")
    }

    /// Set the maximum number of drawables in the pool (2 or 3).
    pub fn set_maximum_drawable_count(&self, count: u32) {
        todo!("vulkan P3")
    }

    /// Acquire the next drawable from the surface.
    pub fn next_drawable(&self) -> Option<GpuDrawable> {
        todo!("vulkan P3")
    }

    /// Configure the surface for Extended Dynamic Range (EDR) output.
    pub fn configure_edr(&self) {
        todo!("vulkan P3")
    }

    /// Set the layer's contents gravity to resize-aspect (letterbox).
    pub fn set_contents_gravity_resize_aspect(&self) {
        todo!("vulkan P3")
    }

    /// Control whether presents are synchronized with Core Animation transactions.
    pub fn set_presents_with_transaction(&self, enabled: bool) {
        todo!("vulkan P3")
    }

    /// Set the layer's background color.
    pub fn set_background_color(&self, r: f64, g: f64, b: f64, a: f64) {
        todo!("vulkan P3")
    }
}

impl GpuDrawable {
    /// Create a GpuTexture referencing this drawable's backing texture.
    pub fn gpu_texture(&self, format: GpuTextureFormat) -> GpuTexture {
        todo!("vulkan P3")
    }

    /// Present the drawable immediately.
    pub fn present(self) {
        todo!("vulkan P3")
    }

    /// Report when this drawable reaches the screen to the observer installed
    /// by [`set_presented_observer`], and return its drawable ID; `None` (and
    /// no handler) when no observer is installed.
    pub fn observe_presented(&self) -> Option<u64> {
        todo!("vulkan P3")
    }
}

impl super::encoder::GpuEncoder {
    /// Schedule a drawable for presentation when the command buffer completes.
    pub fn present_drawable(&mut self, drawable: &GpuDrawable) {
        todo!("vulkan P2")
    }
}
