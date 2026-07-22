//! glTF import ASSEMBLER (P1c stage 2) — a pure function that turns a
//! parsed `.glb`'s [`gltf_load::GltfImportSummary`] (stage 1: the CPU-only
//! parse) into a generator [`EffectGraphDef`] that renders the model
//! faithfully: one `node.render_scene` object PER DISTINCT MATERIAL, each
//! fed its material-filtered geometry (`node.gltf_mesh_source`), that
//! material's base-color texture (`node.gltf_texture_source`, when present),
//! a `node.pbr_material` atom carrying the glTF's PBR factors, and a
//! `node.transform_3d` atom (seeded to recenter the object at the origin) —
//! plus a shared synthesized framing camera (`node.orbit_camera`), a sun
//! light (`node.light`), and an IBL envmap (`node.bake_environment`).
//!
//! Each object's producers are wrapped in one named, tinted node **group**
//! so a multi-mesh import reads as a few labelled boxes in the graph editor
//! rather than a wall of loose nodes; the group flattens away at load to the
//! exact same flat graph, so nothing the runtime sees changes (see
//! [`build_import_graph`] and `docs/GROUPING_GRAPHS.md`).
//!
//! No GPU, no file I/O beyond the one [`gltf_load::gltf_import_summary`]
//! parse this module drives — everything here is graph-shape assembly.
//! The glb path itself never becomes a node `param` (there is no `String`
//! variant in [`SerializedParamValue`] — see its doc comment); it flows
//! through `presetMetadata.stringParams` + `stringBindings`, the same
//! outer-card text-config convention `node.image_folder`-based presets use
//! (see `assets/generator-presets/MriVolume.json`'s `axial_folder`).
//!
//! Production caller: `manifold-app`'s `.glb`/`.gltf` file-drop handler
//! (`Application::import_model_file`) calls [`assemble_import_graph`], then
//! installs the result on a new generator layer via
//! `manifold_editing::commands::layer::ImportModelLayerCommand`.

use std::path::Path;

use manifold_core::effect_graph_def::EffectGraphDef;

use super::gltf_load;

mod animation;
mod assembly;
mod cards;
mod materials;
mod merge;
mod object_group;
mod report;
mod scene;
#[cfg(test)]
mod tests;

pub use merge::{MergePlan, assemble_merge_plan};
pub use report::ImportReport;

use scene::build_import_graph;

// Re-imported so `merge`'s `super::scene_vm::RENDER_SCENE_TYPE_ID` reference
// resolves from the gltf_import module after the split (pure-move wiring).
pub(super) use crate::node_graph::scene_vm;

/// Stable identity for the one outer-card text config every imported
/// preset carries: the source `.glb`/`.gltf` path.
pub(super) const MODEL_FILE_PARAM_ID: &str = "model_file";
/// GLB_CONFORMANCE_DESIGN.md D6 — the HDRI environment's own Browse field,
/// a distinct string param from [`MODEL_FILE_PARAM_ID`] (the imported
/// .glb's path). Empty by default; `node.hdri_source` reads an empty path
/// as "nothing decoded" and clears its output to black.
pub(super) const HDRI_FILE_PARAM_ID: &str = "hdri_file";

/// Parse `path` and assemble a generator [`EffectGraphDef`] that renders it
/// faithfully: one `node.render_scene` object per distinct material — 1:1,
/// no truncation (GLB_CONFORMANCE_DESIGN.md D4) — each fed its
/// material-filtered geometry + base-color texture (if any) + a PBR
/// material, framed by a synthesized orbit camera sized to the glb's
/// bounding box, lit by one sun light, under a baked IBL envmap (required —
/// `node.pbr_material` is degenerate without one). Pure function: one CPU
/// parse via [`gltf_load::gltf_import_summary`], no GPU, no other I/O.
///
/// Errors when the glb has no materials with geometry (nothing to import),
/// or when it has more than [`OBJECT_SAFETY_MAX`] materials with geometry
/// (a real GPU/port-list safety bound, D4 — never silently truncated) —
/// propagated from [`gltf_load::gltf_import_summary`] or raised here.
pub fn assemble_import_graph(path: &Path) -> Result<(EffectGraphDef, ImportReport), String> {
    let summary = gltf_load::gltf_import_summary(path)?;
    build_import_graph(&summary, path)
}
