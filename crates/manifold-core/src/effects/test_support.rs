//! Shared test fixtures + registrations for the effects module tests
//! (used by >=2 module test mods). Extracted from the flat effects.rs
//! test mod (P2-E, E-S6).

use super::*;
use crate::units::Beats;

inventory::submit! {
    crate::effect_registration::EffectMetadata {
        id: PresetTypeId::new("TestTwoParamRoundTrip"),
        display_name: "Test Two Param Round Trip",
        category: "Test",
        available: true,
        osc_prefix: "testTwoParamRoundTrip",
        legacy_discriminant: None,
        params: &[
            crate::generator_registration::ParamSpec::continuous(
                "alpha", "Alpha", 0.0, 1.0, 0.0, "F2", "",
            ),
            crate::generator_registration::ParamSpec::continuous(
                "beta", "Beta", 0.0, 1.0, 0.0, "F2", "",
            ),
        ],
    }
}

// Registered via `inventory::submit!` at module scope (mirrors
// `manifold-playback`'s `modulation::tests` fixture pattern) — the
// registry is normally populated by manifold-renderer's effect
// implementations, which manifold-core's own test binary doesn't link.
inventory::submit! {
    crate::effect_registration::EffectMetadata {
        id: PresetTypeId::new("TestCreateDefaultUntouched"),
        display_name: "Test Create Default Untouched",
        category: "Test",
        available: true,
        osc_prefix: "testCreateDefaultUntouched",
        legacy_discriminant: None,
        params: &[crate::generator_registration::ParamSpec::continuous(
            "amount", "Amount", 0.0, 1.0, 0.42, "F2", "",
        )],
    }
}

// ── User-exposed parameter bindings (Phase 3 step 20) ─────────

pub(super) fn sample_user_binding(id: &str, node: &str, inner: &str) -> UserParamBinding {
    UserParamBinding {
        id: id.to_string(),
        label: inner.to_string(),
        node_id: NodeId::new(node),
        legacy_node_handle: None,
        inner_param: inner.to_string(),
        min: 0.0,
        max: 1.0,
        default_value: 0.25,
        convert: ParamConvert::Float,
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
        value_labels: Vec::new(),
        section: None,
    }
}

/// Build a bundled test [`Param`] (value == base == `value`) with the given
/// id, exposure, and a 0..1 range. Replaces the old positional `ParamSlot`.
pub(super) fn slot(id: &str, value: f32, exposed: bool) -> crate::params::Param {
    let spec = crate::effect_graph_def::ParamSpecDef {
        id: id.to_string(),
        name: String::new(),
        min: 0.0,
        max: 1.0,
        default_value: value,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: Default::default(),
        invert: false,
        is_angle: false,
        is_trigger_gate: false,
        wraps: false,
        section: None,
    };
    let mut p = crate::params::Param::bundled(spec);
    p.value = value;
    p.base = value;
    p.exposed = exposed;
    p
}

/// Build a manifest from positional `(value, exposed)` pairs, assigning
/// synthetic ids `p0`, `p1`, … in card order — the value-only analogue of
/// the old `param_values: vec![ParamSlot::exposed(..)]`.
pub(super) fn manifest(slots: &[(f32, bool)]) -> crate::params::ParamManifest {
    crate::params::ParamManifest::from_params(
        slots
            .iter()
            .enumerate()
            .map(|(i, &(v, e))| slot(&format!("p{i}"), v, e))
            .collect(),
    )
}

// ─── Automation lane curve evaluation ───

pub(super) fn pt(beat: f64, value: f32, shape: SegmentShape) -> AutomationPoint {
    AutomationPoint {
        beat: Beats(beat),
        value,
        shape,
    }
}

