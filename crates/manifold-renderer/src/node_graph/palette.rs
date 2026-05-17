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
//!    edit. Sourced from the bundled-preset registry (§6.6 #26), so
//!    every shipping effect returns `Some` — per-card divergence is
//!    available on every card, not just the original Mirror /
//!    SoftFocus pair.
//!
//! Phase 4 of per-card divergence in `docs/NODE_GRAPH_SYSTEM.md`.

use manifold_core::EffectTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;

use crate::node_graph::bundled_presets::bundled_preset_def;
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

/// Build the catalog-default [`EffectGraphDef`] for `effect_type`.
/// Editing commands clone this into `EffectInstance.graph` on first
/// edit so subsequent mutations have a topology to manipulate.
///
/// Backed by the bundled-preset registry (`bundled_presets.rs`), so
/// every shipping effect returns `Some`. Returns `None` only for
/// effect types that aren't registered at all (unknown ids from
/// future-version save files).
pub fn catalog_graph_def_for(effect_type: &EffectTypeId) -> Option<EffectGraphDef> {
    bundled_preset_def(effect_type).cloned()
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
    fn catalog_default_is_available_for_every_shipping_effect() {
        // Previously only Mirror + SoftFocusGraph had catalog graphs;
        // the bundled-preset registry now covers every ChainSpec, so
        // per-card divergence works on every effect.
        for type_id in crate::node_graph::bundled_preset_type_ids() {
            assert!(
                catalog_graph_def_for(&type_id).is_some(),
                "missing catalog default for shipping effect {}",
                type_id.as_str(),
            );
        }
    }

    #[test]
    fn catalog_default_returns_none_for_unregistered_effects() {
        // EffectTypeId::UNKNOWN is the placeholder for forward-version
        // ids that didn't exist when this binary was built.
        assert!(catalog_graph_def_for(&EffectTypeId::UNKNOWN).is_none());
    }
}
