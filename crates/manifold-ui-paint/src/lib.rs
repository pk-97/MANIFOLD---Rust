//! GPU painting for the bitmap UI.
//! Owns text, clip and automation drawing, bitmap caches and the UI renderer.
//! Never depends on the node engine, node families, catalog, compositor,
//! editing, IO or the app.

pub mod automation_lane_draw;
pub mod clip_content_gpu;
pub mod clip_draw;
pub mod clip_thumb_gpu;
pub mod layer_bitmap_gpu;
#[cfg(target_os = "macos")]
pub mod native_text;
pub mod ui_cache_manager;
pub mod ui_renderer;
