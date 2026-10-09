//! Image and control nodes for the graph engine.
//! Owns image effects, generators, text rasterization and image composite builders.
//! Never depends on other node families, the catalog, compositor, UI paint,
//! UI, editing, IO or the app.

pub mod node_graph;
#[cfg(target_os = "macos")]
pub mod text_rasterizer;
