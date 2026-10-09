//! Scene nodes for the graph engine.
//! Owns scene rendering, materials, meshes, glTF import and scene authoring helpers.
//! Never depends on other node families, the catalog, compositor, UI paint,
//! UI, editing, IO or the app.

pub mod generators;
pub mod denoiser;
pub mod metalfx_temporal_upscaler;
pub mod node_graph;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
