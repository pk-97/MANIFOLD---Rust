//! Editor palette + catalog default lookups.
//!
//! Two helpers consumed by the editor UI:
//!
//! 1. [`palette_atoms`] — the flat alphabetical list of atoms users
//!    can drop into a graph via the palette. Composites and
//!    monolithic-wrapper primitives (Bloom, Halation, AutoGain, etc.)
//!    are excluded — they're whole effects, not building blocks.
//! 2. [`catalog_graph_def_for`] — the catalog-default
//!    [`EffectGraphDef`] for an [`EffectTypeId`]. Editing commands
//!    need this to lift `EffectInstance.graph` from `None` on first
//!    edit. Only graph-backed effects (Mirror, SoftFocus,
//!    StylizedFeedback, NodeGraphTest) return `Some`.
//!
//! Phase 4 of per-card divergence in `docs/NODE_GRAPH_SYSTEM.md`.

use manifold_core::EffectTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;

use crate::node_graph::boundary_nodes::{FinalOutput, Source};
use crate::node_graph::composites::{
    build_mirror, build_soft_focus, MIRROR_TYPE_ID, SOFT_FOCUS_TYPE_ID,
};
use crate::node_graph::graph::Graph;
use crate::node_graph::persistence::EffectGraphDefExt;
use crate::node_graph::primitives;

/// One entry in the editor's palette: a node type the user can drop
/// into the graph. Owned strings so the list can be cloned across the
/// content/UI boundary without allocations downstream.
#[derive(Debug, Clone)]
pub struct PaletteAtom {
    /// Display label, e.g. "Blur", "Mix", "Feedback".
    pub label: String,
    /// Stable `type_id` the renderer's [`super::PrimitiveRegistry`]
    /// recognizes — passed verbatim to
    /// `AddGraphNodeCommand::new_node_type_id`.
    pub type_id: String,
}

/// The flat alphabetical list of atoms shown in the editor palette.
///
/// Sourced from the V1 primitive catalog (`docs/NODE_CATALOG.md`).
/// Composites and monolithic-wrapper primitives are intentionally
/// excluded — those are whole effects shipped as cards, not authoring
/// building blocks.
pub fn palette_atoms() -> Vec<PaletteAtom> {
    let mut atoms = vec![
        PaletteAtom {
            label: "Blend".to_string(),
            type_id: primitives::BLEND_TYPE_ID.to_string(),
        },
        PaletteAtom {
            label: "Blur".to_string(),
            type_id: primitives::BLUR_TYPE_ID.to_string(),
        },
        PaletteAtom {
            label: "Brightness".to_string(),
            type_id: primitives::BRIGHTNESS_TYPE_ID.to_string(),
        },
        PaletteAtom {
            label: "Channel Mix".to_string(),
            type_id: primitives::CHANNEL_MIX_TYPE_ID.to_string(),
        },
        PaletteAtom {
            label: "Color Ramp".to_string(),
            type_id: primitives::COLOR_RAMP_TYPE_ID.to_string(),
        },
        PaletteAtom {
            label: "Feedback".to_string(),
            type_id: primitives::FEEDBACK_TYPE_ID.to_string(),
        },
        PaletteAtom {
            label: "Gaussian Blur".to_string(),
            type_id: primitives::GAUSSIAN_BLUR_TYPE_ID.to_string(),
        },
        PaletteAtom {
            label: "Mip Chain".to_string(),
            type_id: primitives::MIP_CHAIN_TYPE_ID.to_string(),
        },
        PaletteAtom {
            label: "Mix".to_string(),
            type_id: primitives::MIX_TYPE_ID.to_string(),
        },
        PaletteAtom {
            label: "Sample".to_string(),
            type_id: primitives::SAMPLE_TYPE_ID.to_string(),
        },
        PaletteAtom {
            label: "Threshold".to_string(),
            type_id: primitives::THRESHOLD_TYPE_ID.to_string(),
        },
        PaletteAtom {
            label: "Transform".to_string(),
            type_id: primitives::TRANSFORM_TYPE_ID.to_string(),
        },
        PaletteAtom {
            label: "Wet/Dry".to_string(),
            type_id: primitives::WET_DRY_TYPE_ID.to_string(),
        },
    ];
    atoms.sort_by(|a, b| a.label.cmp(&b.label));
    atoms
}

/// Build the catalog-default [`EffectGraphDef`] for a graph-backed
/// effect type. Editing commands clone this into
/// `EffectInstance.graph` on first edit so subsequent mutations have
/// a topology to manipulate.
///
/// Returns `None` for effect types that aren't graph-backed (Bloom,
/// AutoGain, etc.) — those run a single monolithic primitive without
/// an editable sub-graph.
pub fn catalog_graph_def_for(effect_type: &EffectTypeId) -> Option<EffectGraphDef> {
    match effect_type {
        t if t == &EffectTypeId::MIRROR => Some(build_mirror_default()),
        t if t == &EffectTypeId::SOFT_FOCUS_GRAPH => Some(build_soft_focus_default()),
        // StylizedFeedback and NodeGraphTest are graph-backed too, but
        // their catalog graphs aren't promoted to named handles yet —
        // they'd need the same "source"/"final_output" handle pass that
        // Mirror went through in Phase 1. Deferred until those FXs
        // implement `apply_graph_def` end-to-end.
        _ => None,
    }
}

fn build_mirror_default() -> EffectGraphDef {
    let mut graph = Graph::new();
    let src = graph.add_node_named("source", Box::new(Source::new()));
    let handle = build_mirror(&mut graph, (src, "out"))
        .expect("build_mirror should never fail with a valid source");
    let final_out = graph.add_node_named("final_output", Box::new(FinalOutput::new()));
    graph
        .connect(handle.output(), (final_out, "in"))
        .expect("wire Mix.out → FinalOutput.in");
    let mut def = EffectGraphDef::from_graph(&graph);
    def.name = Some(MIRROR_TYPE_ID.to_string());
    def
}

fn build_soft_focus_default() -> EffectGraphDef {
    let mut graph = Graph::new();
    let src = graph.add_node_named("source", Box::new(Source::new()));
    let handle = build_soft_focus(&mut graph, (src, "out"))
        .expect("build_soft_focus should never fail with a valid source");
    let final_out = graph.add_node_named("final_output", Box::new(FinalOutput::new()));
    graph
        .connect(handle.output(), (final_out, "in"))
        .expect("wire Mix.out → FinalOutput.in");
    let mut def = EffectGraphDef::from_graph(&graph);
    def.name = Some(SOFT_FOCUS_TYPE_ID.to_string());
    def
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_atoms_are_unique_and_alphabetical() {
        let atoms = palette_atoms();
        assert!(!atoms.is_empty());
        for w in atoms.windows(2) {
            assert!(
                w[0].label <= w[1].label,
                "atoms must be alphabetically sorted: {:?} > {:?}",
                w[0].label,
                w[1].label,
            );
        }
        let ids: std::collections::HashSet<_> = atoms.iter().map(|a| &a.type_id).collect();
        assert_eq!(ids.len(), atoms.len(), "duplicate type ids in palette");
    }

    #[test]
    fn catalog_default_for_mirror_has_required_handles() {
        let def = catalog_graph_def_for(&EffectTypeId::MIRROR).expect("Mirror has catalog default");
        let handles: std::collections::HashSet<_> = def
            .nodes
            .iter()
            .filter_map(|n| n.handle.as_deref())
            .collect();
        assert!(handles.contains("source"));
        assert!(handles.contains("uv_transform"));
        assert!(handles.contains("mix"));
        assert!(handles.contains("final_output"));
    }

    #[test]
    fn catalog_default_returns_none_for_non_graph_effects() {
        assert!(catalog_graph_def_for(&EffectTypeId::BLOOM).is_none());
        assert!(catalog_graph_def_for(&EffectTypeId::UNKNOWN).is_none());
    }
}
