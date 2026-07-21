//! Registry-side param descriptor types (`RegistryParamDef`, `RangeContract`,
//! `RangeReason`). Extracted from effects.rs (P2-E, design D4).

use serde::{Deserialize, Serialize};

// ─── Param Definition ───

/// Registry-side param descriptor: the manifest's [`crate::effect_graph_def::ParamSpecDef`]
/// (the ONE slider-surface shape, shared with the graph metadata now that
/// `ParamDef` no longer exists as a separate near-twin) plus the one fact a
/// registry entry genuinely owns that a card manifest must not carry — the
/// range contract (PARAM_RANGE_CONTRACT_DESIGN.md D3/D4: the card manifest
/// must stay unable to carry a contract). Not serialized: `PresetDef` and its
/// `param_defs` are built in-memory at registry-construction time from
/// `inventory::submit!` sources or JSON-loaded `ParamSpecDef`s, never
/// deserialized as this shape.
///
/// Collapses the former `effects::ParamDef` / `effect_graph_def::ParamSpecDef`
/// twin (and the three hand-written converters between them) into one
/// descriptor that exists once — see `handoff_param_descriptor_unification_brief`.
#[derive(Debug, Clone, Default)]
pub struct RegistryParamDef {
    pub spec: crate::effect_graph_def::ParamSpecDef,
    /// A real physical/mathematical boundary this param's inner value must
    /// not cross — as opposed to `spec.min`/`spec.max`, which are display
    /// hints (default slider travel) a card, text entry, or modulation is
    /// free to exceed. `None` for the overwhelming majority of params
    /// (PARAM_RANGE_CONTRACT_DESIGN.md D6: remove-by-default — no
    /// kernel/shader proof, no contract). See [`RangeContract`].
    pub contract: Option<RangeContract>,
}

/// A named, real boundary on a param's inner value — the ONLY thing card
/// range validation (`node_graph::validate` lint (h)) enforces as an error.
/// Everything else (`RegistryParamDef::spec.min`/`max`) is a display hint that never
/// restricts (Peter, `docs/PARAM_RANGE_CONTRACT_DESIGN.md`: *"Inner nodes
/// that don't have a real physical range or boundary shouldn't have a
/// boundary — that's what the card mappings and ranges are for."*).
///
/// One-sided bounds are first-class (`min`/`max` are independently
/// optional) — a contract may only forbid going too low, or too high, or
/// both. `reason` is mandatory: there is no contract without a named
/// excuse, mirroring the `BoundaryReason` declared-excuse pattern
/// (`node_graph::freeze::classify::BoundaryReason`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RangeContract {
    pub min: Option<f32>,
    pub max: Option<f32>,
    pub reason: RangeReason,
}

/// Why a `RangeContract` exists (design doc D2). A closed enum: every
/// contract names exactly one of these — the meta-test
/// `every_range_contract_names_a_real_boundary` (manifold-renderer,
/// `node_graph::freeze::classify`) pins each contracted param to its
/// reason in a curated table, so a contract can't creep back onto a
/// param whose range is merely a creative-amount hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RangeReason {
    /// Addresses a discrete resource (mux select, array slot).
    Index,
    /// Sizes an allocation (num_inputs, particle caps).
    Count,
    /// The kernel divides/degenerates at or below the bound.
    DegenerateFloor,
    /// Geometry collapses outside the bound.
    DegenerateGeometry,
    /// The shader physically clamps; beyond the bound is a dead input.
    ShaderClamp,
    /// The math is ONLY defined on the interval — a true domain, not a
    /// lerp/blend factor (those extrapolate legitimately; see the Bloom
    /// ruling in the design doc's intro).
    NormalizedDomain,
}
