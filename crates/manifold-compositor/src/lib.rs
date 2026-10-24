//! Layer composition and presentation for the instrument.
//! Owns generator rendering, layer blending, preset thumbnails and output processing.
//! Never depends on node families, the catalog, UI, editing, IO or the app
//! in production; catalog dependencies are allowed only in tests.

pub mod compositor;
pub mod display_capture;
pub mod fsr1;
pub mod generator_renderer;
pub mod layer_compositor;
pub mod metalfx_upscaler;
pub mod pq_encoder;
pub mod presentation;
pub mod preset_thumbnail;
pub mod tonemap;
