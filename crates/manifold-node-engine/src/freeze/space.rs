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
//! Space model, mirroring the plan's resolution:
//! - [`ElementSpace::Concrete`] — the plan resolved a fixed `(w, h)`.
//! - [`ElementSpace::Scaled`] — canvas-relative `(num, den)` (a quarter-res
//!   sim chain below a `downsample`).
//! - [`ElementSpace::Canvas`] — canvas-default (the overwhelming majority).
//! - [`ElementSpace::Live`] — sized by a producer whose canvas scale a live
//!   param can change after this plan was built.
//!
//! Two spaces are fusable together only when EQUAL. `Concrete(480, 270)` and
//! `Scaled(1, 4)` may coincide at one canvas size and not another, so they
//! never count as the same space — conservative, fails closed.
//!
//! The plan here is compiled with the def's own params, so it only sees the
//! scale a live producer declares at load. When the live param flips later the
//! executor re-sizes that producer's outputs, and everything that follows
//! them, without fusion running again. [`ElementSpace::Live`] therefore stays
//! symbolic: which producer the grid follows, never a size.

use ahash::AHashMap;
use manifold_core::effect_graph_def::EffectGraphDef;

use crate::exec::execution_plan::{ExecutionStep, ResourceId};
use crate::load::graph_loader::{BoundaryHandling, HandleScope, instantiate_def};
use crate::ports::PortType;
use crate::{exec::execution_plan::compile, graph::Graph, persistence::PrimitiveRegistry};

/// The grid a texture output resolves to in the unfused plan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElementSpace {
    /// Canvas-default — sized by the live canvas at acquire time.
    Canvas,
    /// Canvas-relative fraction `(num, den)`.
    Scaled(u32, u32),
    /// Fixed dims `(w, h)` known at compile time.
    Concrete(u32, u32),
    /// Follows the live canvas scale declared by doc node `node` (see
    /// `EffectNode::output_canvas_scale_is_live`): either an output of that
    /// node, or the output of a node whose every texture input is one. The
    /// real size depends on a param the plan did not see, so it equals only
    /// another `Live` of the same producer, never `Canvas`, `Scaled` or
    /// `Concrete`. All live ports of one node count as one source: they move
    /// together when that node's live param flips.
    Live { node: u32 },
}

/// Element space of every consumed Texture2D output in `def`, keyed by
/// `(doc node id, output port)`. Resolved by building the UNFUSED graph and
/// compiling its plan (both device-free), then reading each resource's
/// `resource_dims` / `resource_canvas_scale`. `None` when the def doesn't
/// build standalone (synthetic test fixtures, malformed defs) — callers fall
/// back to treating everything as [`ElementSpace::Canvas`], which reproduces
/// the pre-tier-6 behaviour.
///
/// Live sizes the plan cannot see: an output its node declares live
/// (`EffectNode::output_canvas_scale_is_live`) is [`ElementSpace::Live`] of
/// that node, and so is an output that follows its texture inputs (no concrete
/// dims, no declared scale or cap of its own) when every wired texture input is
/// `Live` of one node. That is the executor's own propagation: a canvas-default
/// or back-edge input makes the output canvas, and equal input scales give that
/// scale. Anything else, a mix of sources or of live and fixed inputs
/// included, keeps what the plan resolved.
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
        &crate::scene::mesh_change::PreparedMeshRules::default(),
    )
    .ok()?;
    let plan = compile(&graph).ok()?;

    // Invert the doc→runtime map once, then walk the plan's steps: every
    // consumed output appears exactly once as a step output.
    let runtime_to_doc: AHashMap<_, _> = inst.id_map.iter().map(|(d, r)| (*r, *d)).collect();
    let mut spaces: AHashMap<(u32, String), ElementSpace> = AHashMap::default();
    // Space of every texture resource resolved so far. Steps run in
    // topological order, so a step's inputs are here already unless they are
    // state-capture back-edges, which count as not live.
    let mut resource_spaces: AHashMap<ResourceId, ElementSpace> = AHashMap::default();
    for step in plan.steps() {
        let Some(&doc_id) = runtime_to_doc.get(&step.node) else {
            continue;
        };
        let Some(node_inst) = graph.get_node(step.node) else {
            continue;
        };
        let node = &node_inst.node;
        let params = &node_inst.params;
        let inputs_live_source = shared_live_source(node.inputs(), step, &resource_spaces);
        for &(ref port, res) in &step.outputs {
            if !plan.resource_type(res).is_some_and(|t| t.is_texture_2d()) {
                continue;
            }
            let space = if let Some((w, h)) = plan.resource_dims(res) {
                ElementSpace::Concrete(w, h)
            } else if node.output_canvas_scale_is_live(port) {
                ElementSpace::Live { node: doc_id }
            } else if let Some(source) = inputs_live_source
                && node.output_canvas_scale(port, params).is_none()
                && node.output_canvas_max_dim(port, params).is_none()
            {
                ElementSpace::Live { node: source }
            } else {
                match plan.resource_canvas_scale(res) {
                    // n/n of the canvas is the canvas: one name, so a node that
                    // declares full-canvas scale fuses with canvas-default ones.
                    Some((n, d)) if n == d => ElementSpace::Canvas,
                    Some((n, d)) => ElementSpace::Scaled(n, d),
                    None => ElementSpace::Canvas,
                }
            };
            resource_spaces.insert(res, space);
            spaces.insert((doc_id, port.to_string()), space);
        }
    }
    Some(spaces)
}

/// The one node every wired texture input of `step` is `Live` of, or `None`
/// when there is no texture input, one input is not `Live` (canvas-default,
/// scaled, concrete, a back-edge, a producer outside the doc), or two
/// inputs follow different nodes. Texture inputs are counted the way the
/// executor counts them for its own dims propagation.
fn shared_live_source(
    input_ports: &[crate::ports::NodeInput],
    step: &ExecutionStep,
    resource_spaces: &AHashMap<ResourceId, ElementSpace>,
) -> Option<u32> {
    let mut source = None;
    for &(name, res) in &step.inputs {
        let is_texture = input_ports.iter().any(|p| {
            p.name == name
                && matches!(
                    p.ty,
                    PortType::Texture2D | PortType::Texture2DTyped(_) | PortType::Texture3D
                )
        });
        if !is_texture {
            continue;
        }
        let ElementSpace::Live { node } = *resource_spaces.get(&res)? else {
            return None;
        };
        if *source.get_or_insert(node) != node {
            return None;
        }
    }
    source
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
