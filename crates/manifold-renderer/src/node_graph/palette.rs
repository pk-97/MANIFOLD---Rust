//! Editor palette + catalog default lookups.
//!
//! Two helpers consumed by the editor UI:
//!
//! 1. [`palette_atoms`] — the flat alphabetical list of atoms users
//!    can drop into a graph via the palette. Composites and
//!    monolithic-wrapper primitives (Bloom, Halation, BlobTracking,
//!    etc.) are excluded — they're whole effects, not building blocks.
//! 2. [`catalog_graph_def_for`] — the catalog-default
//!    [`EffectGraphDef`] for an [`PresetTypeId`]. Editing commands
//!    need this to lift `PresetInstance.graph` from `None` on first
//!    edit. Sourced from the bundled-preset registry (§6.6 #26), so
//!    every shipping effect returns `Some` — per-card divergence is
//!    available on every card, not just the original Mirror /
//!    SoftFocus pair.
//!
//! Phase 4 of per-card divergence in `docs/NODE_GRAPH_SYSTEM.md`.

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;

use crate::node_graph::bundled_presets::bundled_preset_def;
use crate::node_graph::persistence::PrimitiveFactory;

/// Picker section. Texture-domain building blocks group under
/// [`PaletteCategory::Atom`]; control-rate / scalar producers group
/// under [`PaletteCategory::Driver`]. The two strata are mentally
/// distinct (compose images vs. shape values), so the editor draws
/// them as separate sections — a flat list buries an LFO between
/// Blur and Brightness.
///
/// Variants are listed in display order; the palette renders one
/// header per non-empty category, ordered by enum declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaletteCategory {
    Atom,
    Driver,
}

impl PaletteCategory {
    /// Header text shown above each section in the picker.
    pub fn label(self) -> &'static str {
        match self {
            Self::Atom => "Atoms",
            Self::Driver => "Drivers",
        }
    }

    /// Section render order. Cheaper to bake into the enum than to
    /// thread a comparator through the UI layer.
    pub const ORDER: &'static [Self] = &[Self::Atom, Self::Driver];
}

/// Static picker metadata a primitive declares at its definition
/// site (via the `primitive!` macro's `picker:` field, or directly on
/// the [`PrimitiveFactory`] inventory entry for hand-written
/// primitives). When `Some`, the primitive appears in the editor
/// palette under [`Self::category`] with [`Self::label`] as the
/// clickable row. When `None`, the primitive is registered (loadable
/// from JSON) but doesn't appear in the picker — used for boundary
/// nodes, aesthetic effects shipped as whole cards, and any primitive
/// authored as an internal building block of a composite.
///
/// [`PrimitiveFactory`]: super::persistence::PrimitiveFactory
#[derive(Debug, Clone, Copy)]
pub struct PickerInfo {
    pub label: &'static str,
    pub category: PaletteCategory,
}

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
    /// Which picker section this entry belongs to.
    pub category: PaletteCategory,
}

/// The flat alphabetical list of atoms shown in the editor palette.
///
/// Sourced from the V1 primitive catalog (`docs/NODE_CATALOG.md`).
/// Composites and monolithic-wrapper primitives are intentionally
/// excluded — those are whole effects shipped as cards, not authoring
/// building blocks.
pub fn palette_atoms() -> Vec<PaletteAtom> {
    // Auto-populated from the `PrimitiveFactory` inventory channel —
    // every shipping primitive that declared `picker: ...` at its
    // definition site lands here automatically. Adding a node never
    // requires touching this function. Aesthetic effects, system
    // boundary nodes, and internal composite building blocks omit
    // the field and so stay hidden.
    let mut atoms: Vec<PaletteAtom> = inventory::iter::<PrimitiveFactory>
        .into_iter()
        .filter_map(|f| {
            let info = f.picker?;
            Some(PaletteAtom {
                label: info.label.to_string(),
                type_id: f.type_id.to_string(),
                category: info.category,
            })
        })
        .collect();
    atoms.sort_by(|a, b| {
        let cat_order = |c: PaletteCategory| {
            PaletteCategory::ORDER
                .iter()
                .position(|&v| v == c)
                .unwrap_or(usize::MAX)
        };
        cat_order(a.category)
            .cmp(&cat_order(b.category))
            .then_with(|| a.label.cmp(&b.label))
    });
    atoms
}

/// Friendly display label for a node `type_id` — the same name the palette
/// shows (e.g. "Scale + Offset (value)" for `node.scale_offset_value`). Sourced
/// from the primitive's `picker` label so canvas node titles match the
/// palette instead of showing the raw prettified type id. `None` for nodes
/// with no picker (boundary nodes, internal building blocks); callers fall
/// back to a prettified type id.
pub fn friendly_label_for(type_id: &str) -> Option<&'static str> {
    inventory::iter::<PrimitiveFactory>
        .into_iter()
        .find(|f| f.type_id == type_id)
        .and_then(|f| f.picker)
        .map(|p| p.label)
}

/// Build the catalog-default [`EffectGraphDef`] for `effect_type`.
/// Editing commands clone this into `PresetInstance.graph` on first
/// edit so subsequent mutations have a topology to manipulate.
///
/// Backed by the bundled-preset registry (`bundled_presets.rs`), so
/// every shipping effect returns `Some`. Returns `None` only for
/// effect types that aren't registered at all (unknown ids from
/// future-version save files).
pub fn catalog_graph_def_for(effect_type: &PresetTypeId) -> Option<EffectGraphDef> {
    bundled_preset_def(effect_type).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_atoms_are_unique_and_grouped() {
        let atoms = palette_atoms();
        assert!(!atoms.is_empty());

        // Category groups appear in `PaletteCategory::ORDER`; entries
        // within each group are alphabetical by label.
        let mut last_cat_idx: Option<usize> = None;
        let mut last_label_in_cat: Option<&str> = None;
        for atom in &atoms {
            let cat_idx = PaletteCategory::ORDER
                .iter()
                .position(|&c| c == atom.category)
                .expect("category in ORDER");
            match last_cat_idx {
                None => {}
                Some(prev_idx) if prev_idx == cat_idx => {
                    let prev_label = last_label_in_cat.expect("had prior label in same cat");
                    assert!(
                        prev_label <= atom.label.as_str(),
                        "{:?} > {:?} within {:?}",
                        prev_label,
                        atom.label,
                        atom.category,
                    );
                }
                Some(prev_idx) => {
                    assert!(
                        prev_idx < cat_idx,
                        "categories must appear in ORDER, got {:?} after {:?}",
                        atom.category,
                        atoms[0].category,
                    );
                }
            }
            last_cat_idx = Some(cat_idx);
            last_label_in_cat = Some(atom.label.as_str());
        }

        let ids: std::collections::HashSet<_> = atoms.iter().map(|a| &a.type_id).collect();
        assert_eq!(ids.len(), atoms.len(), "duplicate type ids in palette");

        // The first driver slice ships Value + LFO + Math.
        let drivers: Vec<_> = atoms
            .iter()
            .filter(|a| a.category == PaletteCategory::Driver)
            .map(|a| a.label.as_str())
            .collect();
        // Sanity-check Driver section keeps growing as the catalog
        // gains scalar sources/operators. New entries should land in
        // alphabetical order; this assertion enumerates what's shipped
        // today so unintended drops show up.
        assert_eq!(
            drivers,
            &[
                "Atmosphere",
                "Beat Gate",
                "Beat Ramp",
                "Canvas Area Scale",
                "Clip Trigger Cycle",
                "Color Sample",
                "Compressor Envelope",
                "Connect Nearest",
                "Cycle Table Row",
                "Envelope Decay",
                "Envelope Follower (A/R)",
                "Filter Detections",
                "Free Camera",
                "Frequency Ratio",
                "Inject Burst",
                "LFO",
                "Light",
                "Look-At Camera",
                "Luminance",
                "Math",
                "One Euro Filter",
                "Orbit Camera",
                "Peak",
                "Sample & Hold",
                "Scale + Offset (value)",
                "Scene Object",
                "Smoothing",
                "Sum Into Bins",
                "Texture Size",
                "Track Persist",
                // `node.transform_3d` (P1, SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md):
                // this literal enumeration wasn't updated when the atom
                // landed — a pre-existing gap from that phase, not this one.
                "Transform 3D",
                "Trigger Ease To",
                "Trigger Gate",
                "Value",
                // Sorts last: ASCII byte-order puts a lowercase leading
                // letter after every uppercase-leading label above.
                "glTF Animation Source",
                "glTF Morph Weights",
                "glTF Skeleton Pose",
            ],
        );
    }

    #[test]
    fn catalog_default_for_mirror_has_required_handles() {
        let def = catalog_graph_def_for(&PresetTypeId::MIRROR).expect("Mirror has catalog default");
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
        // Previously only Mirror + SoftFocus had catalog graphs;
        // the bundled-preset registry now covers every ChainSpec, so
        // per-card divergence works on every effect.
        for type_id in
            crate::node_graph::bundled_preset_type_ids(manifold_core::preset_def::PresetKind::Effect)
        {
            assert!(
                catalog_graph_def_for(&type_id).is_some(),
                "missing catalog default for shipping effect {}",
                type_id.as_str(),
            );
        }
    }

    #[test]
    fn catalog_default_returns_none_for_unregistered_effects() {
        // PresetTypeId::UNKNOWN is the placeholder for forward-version
        // ids that didn't exist when this binary was built.
        assert!(catalog_graph_def_for(&PresetTypeId::UNKNOWN).is_none());
    }
}
