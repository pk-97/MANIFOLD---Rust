//! Element-space resolution for the fusion finder / installer (tier 6).
//!
//! A fused kernel iterates ONE grid and reads its coincident externals via
//! `textureLoad` at its own coordinate, so every member must have run at that
//! same grid when unfused — and the fused node's output must land at that
//! grid too, or downstream sampling shifts at texture edges (the ParticleText
//! fp32 divergence class). The executor's compile-time dims propagation
//! ([`compile`]'s `resource_dims` / `resource_canvas_scales` pass) is the
//! authority on what grid each unfused output actually used, and it's
//! device-free — so rather than re-deriving its rules (and drifting), we
//! build the unfused graph + plan and read the answer.
//!
//! Three-way space model, mirroring the plan's resolution:
//! - [`ElementSpace::Concrete`] — the plan resolved a fixed `(w, h)`.
//! - [`ElementSpace::Scaled`] — canvas-relative `(num, den)` (a quarter-res
//!   sim chain below a `downsample`).
//! - [`ElementSpace::Canvas`] — canvas-default (the overwhelming majority).
//!
//! Two spaces are fusable together only when EQUAL. `Concrete(480, 270)` and
//! `Scaled(1, 4)` may coincide at one canvas size and not another, so they
//! never count as the same space — conservative, fails closed.

use ahash::AHashMap;
use manifold_core::effect_graph_def::EffectGraphDef;

use crate::node_graph::graph_loader::{BoundaryHandling, HandleScope, instantiate_def};
use crate::node_graph::{Graph, PrimitiveRegistry, compile};

/// The grid a texture output resolves to in the unfused plan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElementSpace {
    /// Canvas-default — sized by the live canvas at acquire time.
    Canvas,
    /// Canvas-relative fraction `(num, den)`.
    Scaled(u32, u32),
    /// Fixed dims `(w, h)` known at compile time.
    Concrete(u32, u32),
}

/// Element space of every consumed Texture2D output in `def`, keyed by
/// `(doc node id, output port)`. Resolved by building the UNFUSED graph and
/// compiling its plan (both device-free), then reading each resource's
/// `resource_dims` / `resource_canvas_scale`. `None` when the def doesn't
/// build standalone (synthetic test fixtures, malformed defs) — callers fall
/// back to treating everything as [`ElementSpace::Canvas`], which reproduces
/// the pre-tier-6 behaviour.
pub fn resolve_output_spaces(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Option<AHashMap<(u32, String), ElementSpace>> {
    let mut graph = Graph::new();
    let inst = instantiate_def(
        &mut graph,
        def,
        registry,
        HandleScope::Global,
        BoundaryHandling::Standalone,
    &crate::node_graph::mesh_change::PreparedMeshRules::default())
    .ok()?;
    let plan = compile(&graph).ok()?;

    // Invert the doc→runtime map once, then walk the plan's steps: every
    // consumed output appears exactly once as a step output.
    let runtime_to_doc: AHashMap<_, _> = inst.id_map.iter().map(|(d, r)| (*r, *d)).collect();
    let mut spaces: AHashMap<(u32, String), ElementSpace> = AHashMap::default();
    for step in plan.steps() {
        let Some(&doc_id) = runtime_to_doc.get(&step.node) else {
            continue;
        };
        for &(ref port, res) in &step.outputs {
            if !plan.resource_type(res).is_some_and(|t| t.is_texture_2d()) {
                continue;
            }
            let space = match (plan.resource_dims(res), plan.resource_canvas_scale(res)) {
                (Some((w, h)), _) => ElementSpace::Concrete(w, h),
                // n/n of the canvas is the canvas: one name, so a node that
                // declares full-canvas scale fuses with canvas-default ones.
                (None, Some((n, d))) if n == d => ElementSpace::Canvas,
                (None, Some((n, d))) => ElementSpace::Scaled(n, d),
                (None, None) => ElementSpace::Canvas,
            };
            spaces.insert((doc_id, port.to_string()), space);
        }
    }
    Some(spaces)
}

/// Convenience over a resolved map: the space of `(node, port)`, defaulting
/// to [`ElementSpace::Canvas`] for anything unresolved (unconsumed outputs,
/// or a def that didn't build).
pub fn space_of(
    spaces: Option<&AHashMap<(u32, String), ElementSpace>>,
    node: u32,
    port: &str,
) -> ElementSpace {
    spaces
        .and_then(|m| m.get(&(node, port.to_string())).copied())
        .unwrap_or(ElementSpace::Canvas)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::freeze::install::fuse_canonical_def;

    /// Camera Sky declares full-canvas scale and Over defaults to the canvas:
    /// both resolve to one space, so the pair still fuses into one kernel.
    #[test]
    fn full_canvas_scale_is_the_canvas_space() {
        let json = r#"{
            "version": 1, "name": "CameraSkyOverSpace", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.free_camera", "nodeId": "cam" },
                { "id": 2, "typeId": "node.camera_sky", "nodeId": "sky" },
                { "id": 3, "typeId": "node.over", "nodeId": "over" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "sky" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "camera" },
                { "fromNode": 0, "fromPort": "out", "toNode": 3, "toPort": "top" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "bottom" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse fixture graph");
        let registry = PrimitiveRegistry::with_builtin();
        let spaces = resolve_output_spaces(&def, &registry).expect("fixture builds");
        assert_eq!(space_of(Some(&spaces), 2, "out"), ElementSpace::Canvas);
        assert_eq!(space_of(Some(&spaces), 3, "out"), ElementSpace::Canvas);
        let fused = fuse_canonical_def(&def, &registry).expect("camera_sky + over is one fusable region");
        assert_eq!(fused.def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").count(), 1);
    }
}
