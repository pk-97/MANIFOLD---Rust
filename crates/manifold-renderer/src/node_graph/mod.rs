//! Effect & generator graph system.
//!
//! See `docs/NODE_GRAPH_SYSTEM.md` for the full architecture overview.
//!
//! This module currently defines only the core abstractions — the [`EffectNode`]
//! trait, port and parameter types, and graph-level identifiers. The graph
//! runtime (topological sort, execution plan, lifetime planner, resource
//! bindings) lands in subsequent steps.

pub(crate) mod bundled_presets;
pub mod catalog_gen;
pub mod primitives;


pub use viewport_overlay::{
    ScreenLine, ViewportOverlayConfig, WorldLine, build_overlay_lines, camera_frustum_lines,
    composite_overlay_lines_rgba8, grid_lines, light_billboard_lines, project_lines,
};
pub use viewport_gizmo::{
    GizmoAxis, GizmoMode, GizmoTarget, GizmoTargetKind, drag_write, gizmo_lines, gizmo_target_for, move_drag_delta,
    pick_axis, pick_object, rotate_drag_delta, scale_drag_delta,
};
pub use viewport_render::{ViewportRenderError, override_camera_def, render_viewport_frame};
pub use viewport_session::ViewportSession;
pub use bundled_presets::{
    bundled_preset_def, bundled_preset_json, bundled_preset_type_ids, loaded_presets_from_bundled,
    loaded_scene_modifier_presets_from_bundled,
};
#[cfg(test)]
mod catalog_tests;
#[cfg(test)]
mod scene_tests;
#[cfg(test)]
mod image_tests;

#[cfg(any(test, feature = "gpu-proofs"))]
pub mod liquid_conformance_fixtures;
