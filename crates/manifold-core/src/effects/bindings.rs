//! User-exposed parameter bindings and the card→consumer reshape pipeline
//! (`ParamConvert`, `UserParamBinding`, `binding_id_for_node_param_in`,
//! `apply_card_reshape`/`invert_card_reshape`, `RemovedExposure`). Extracted
//! from effects.rs (P2-E, design D4).

use serde::{Deserialize, Serialize};
use crate::id::NodeId;
use super::{ParamEnvelope, ParameterDriver};

// ─── User-Exposed Parameters ───

/// Conversion shape for a user-exposed parameter — the core-side
/// counterpart to `manifold-renderer`'s `ParamConvert`.
///
/// User bindings always route 1:1 from a host-visible slider straight
/// to a single inner-node param, so the renderer's `EnumRemap` and
/// `Custom` variants are intentionally absent here. Effect authors who
/// want non-trivial conversions on a user-exposable surface should keep
/// the conversion inside the effect itself.
///
/// Wire format: serialized as a tagged enum (`{"type": "Float"}` etc.)
/// so future variants don't break round-trips of existing fixtures.
///
/// Phase 4 of the bindings unification plan merged this with the
/// renderer-side `ParamConvert` (the renamed name retained — the
/// previous `ParamConvert` name implied a user-tier-only enum).
/// Both static spec bindings and per-instance user bindings now share
/// this one type at every layer. The `EnumRemap` and `FloatTransform`
/// variants that used to live on the renderer side are gone — their
/// curation moved into the primitives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum ParamConvert {
    /// Pass-through `f32`.
    #[default]
    Float,
    /// Round to nearest int (slider sees float; node sees rounded value).
    IntRound,
    /// Threshold at 0.5 (slider sees 0..1; node sees 0 or 1).
    BoolThreshold,
    /// Round to nearest enum index.
    EnumRound,
    /// Momentary button. Storage is a monotonic `u32` counter held as
    /// `f32`; the outer-card click handler increments by one per press
    /// (no toggle state). Consuming primitives detect rising edges via
    /// the standard `last_count: Option<u32>` cold-start pattern — same
    /// as `node.trigger_gate`. Behaves like an `IntRound` for modulation
    /// resolution (whole-number domain).
    Trigger,
}

// `resolve_param_in` / `ResolvedParam` / the `override_range` closure are
// DELETED. Modulation no longer resolves an
// id to a positional slot: the driver / envelope / audio-mod evaluators read
// `fx.params.get_mut(id)` and take range + whole-number data straight off the
// entry — `p.spec.min` / `p.spec.max` / `p.whole_numbers()`. Calibration edits
// `Param.spec.min`/`max` in place, so a recalibrated slider's range IS the
// entry's range; the old "read the graph override, else the catalog" split
// (the driver-overshoot bug's home) is gone with the split.
/// A user-exposed parameter on an [`PresetInstance`].
///
/// V2 user-exposed-params surface (see `docs/EFFECT_RUNTIME_UNIFICATION.md`
/// §7.6). Each binding is per-instance: ticking "expose UVTransform.translate"
/// on Mirror#0 doesn't affect Mirror#1.
///
/// Stable addressing comes from [`NodeId`] — the inner node's identity,
/// minted once at node creation and invariant under group / ungroup /
/// move / flatten. (The node's handle is a display name that flatten
/// prefixes when the node is grouped, which is exactly why addressing
/// can't key off it.) The renderer resolves this id to a runtime node
/// via `Graph::instance_by_node_id` at chain-build time.
///
/// Storage: each `UserParamBinding` reserves one slot at the tail of
/// the parent `PresetInstance.param_values` (positions
/// `def.param_count..def.param_count + user_param_bindings.len()`).
/// The host writes through that slot; the renderer reads it once per
/// frame via the binding's resolved inner-node target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserParamBinding {
    /// Stable user-generated `ParamId`. Convention:
    /// `"user.<short_node_handle>.<inner_param>.<n>"` where `<n>`
    /// disambiguates collisions (linear probe from 1).
    ///
    /// Once shipped in a saved project, this id is forever — driver,
    /// envelope, Ableton, OSC mappings reference it.
    pub id: String,
    /// Display label shown on the effect-card slider. Mutable; doesn't
    /// affect addressing.
    pub label: String,
    /// Stable [`NodeId`] of the inner node this binding drives. Minted
    /// at node creation, invariant under group / ungroup / move /
    /// flatten. `#[serde(default)]` so pre-node-id projects load with an
    /// empty id; the load migration backfills it from the resolved
    /// graph (`migrate_user_param_bindings_to_node_id`).
    #[serde(default)]
    pub node_id: NodeId,
    /// Load-migration shim. Pre-node-id projects addressed the target by
    /// handle under the JSON key `nodeHandle`; that value is captured
    /// here on load so the renderer-layer migration can resolve it to a
    /// [`NodeId`] (against the instance's graph or its canonical preset),
    /// then it's cleared. `skip_serializing_if` empty so it never
    /// appears in freshly-saved files — the runtime resolver only ever
    /// reads `node_id`, never this. Not a fallback; a one-shot upgrade.
    #[serde(
        default,
        rename = "nodeHandle",
        skip_serializing_if = "Option::is_none"
    )]
    pub legacy_node_handle: Option<String>,
    /// Inner-node parameter name (matches `ParamDef::id` on the addressed node).
    pub inner_param: String,
    pub min: f32,
    pub max: f32,
    pub default_value: f32,
    /// Conversion shape applied at the renderer boundary. Defaults to
    /// `Float` (1:1 pass-through), the only convert needed for the
    /// initial three graph-backed effects (Mirror, SoftFocus,
    /// StylizedFeedback).
    #[serde(default)]
    pub convert: ParamConvert,
    /// Angle presentation hint, captured from the inner param's
    /// `ParamType::Angle` at expose time. Display-only: the stored value
    /// stays RADIANS (drivers / Ableton / envelopes write radians every
    /// frame, unchanged), and the card slider converts to DEGREES only at
    /// the text boundary. `ParamConvert::Float` cannot carry this because
    /// angle and plain-float share it, so it rides its own flag.
    /// `serde(default)` keeps pre-existing projects loading; the post-load
    /// alignment pass backfills it from the registry.
    #[serde(default)]
    pub is_angle: bool,
    /// Card-slider invert. When true the normalized slider position is
    /// flipped (card-left drives inner-max) at the renderer write boundary.
    /// Mapping-only: the stored slot stays physical-valued, so drivers /
    /// Ableton / envelopes writing the same slot are unaffected.
    /// `serde(default)` (false) keeps every saved show 1:1.
    #[serde(default)]
    pub invert: bool,
    /// Card-slider response curve, applied to the normalized position at the
    /// renderer write boundary (after invert, before scaling to [min, max]).
    /// Reuses the macro-bank curve so the whole app shares one curve type.
    /// `serde(default)` (Linear) keeps every saved show 1:1.
    #[serde(default)]
    pub curve: crate::macro_bank::MacroCurve,
    /// Card→consumer linear remap applied at the renderer write boundary AFTER
    /// the slider reshape and BEFORE wrap/convert: `out = value * scale + offset`.
    /// This is where an in-graph `affine_scalar` that only rescaled a card value
    /// toward its inner consumer folds in — the card keeps storing the friendly
    /// value (Curl 85°, Particle Count 2.0), drivers/Ableton/envelopes write the
    /// same slot unchanged, and the binding does the scale the node used to do.
    /// `serde(default = "one")` keeps `scale = 1.0`; with `offset = 0.0` that is
    /// identity, so every saved show stays byte-identical until a fold sets them.
    #[serde(default = "one")]
    pub scale: f32,
    #[serde(default)]
    pub offset: f32,
    /// Enum option labels captured from the inner param's `ParamDef` at
    /// expose time. Drives the card slider's stepped/labelled rendering so an
    /// exposed enum (Fold Mode, Blend Mode, …) shows its option names instead
    /// of a bare 0..N numeric slider. Empty for non-enum params; carried onto
    /// the appended `ParamSpecDef` so the card reads it through the normal
    /// reshape overlay. `serde(default)` keeps pre-existing projects loading.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value_labels: Vec<String>,
    /// Card-bundling section name (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2
    /// D5), captured from the innermost enclosing group's display name at
    /// expose time and carried onto the appended [`crate::effect_graph_def
    /// ::ParamSpecDef`] so the card reads it through the normal manifest
    /// path. `None` for a top-level (unscoped) expose. `serde(default)`
    /// keeps pre-existing projects loading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

/// serde default for [`UserParamBinding::scale`] — identity is `1.0`, not the
/// `f32::default()` of `0.0` (which would zero every un-migrated binding).
fn one() -> f32 {
    1.0
}

// ─── Card → consumer reshape pipeline ───

/// The card→consumer reshape pipeline — the **single definition** shared by
/// the renderer's runtime write boundary (`ResolvedBinding`'s `Reshape::apply`)
/// and the mapping-popover's live preview, so the two can never drift. A
/// preview computed by different math than the engine is a lie the moment one
/// side changes; routing both through this function makes that unrepresentable.
///
/// The reshape (display label, slider range, invert, response curve, and the
/// card→consumer affine `scale`/`offset`) lives in the preset's authoring
/// surface — each param's `ParamSpecDef` plus its `BindingDef` — the single
/// source after the per-instance `ParamMapping` note was deleted. A
/// recalibration edits that spec on the instance's per-instance graph override
/// ([`UserParamBinding`] does the same inline for user-exposed params); the
/// value slot the modulation surface writes is never touched, so the reshape
/// applies DOWNSTREAM at the renderer boundary and the live rig is unaffected.
///
/// Two stages, matching the [`UserParamBinding`] reshape semantics:
/// 1. **Slider response** — only when `invert` or a non-Linear `curve` is set:
///    normalize the value within `[min, max]`, invert, apply the curve, scale
///    back. This stage clamps to `[0, 1]` so the response is well defined
///    across the slider. A pure scale/offset fold skips this entirely, so it
///    reproduces the `affine_scalar` it replaced exactly.
/// 2. **Card→consumer affine** — `out = v * scale + offset`, UNCLAMPED, where a
///    folded `affine_scalar` lands (e.g. a deg→rad scale a driver may push past
///    the slider max, which the angle wrap then tames downstream).
///
/// Identity inputs (`invert = false`, `curve = Linear`, `scale = 1`,
/// `offset = 0`) return `value` unchanged.
/// Def-level half of [`PresetInstance::binding_id_for_node_param`]: the
/// REAL binding id (`inst.params` key) for the inner-graph param
/// `(node_doc_id, param_key)` in `def`, if any binding — bundled or
/// user-added — targets it. Free function so callers holding the EFFECTIVE
/// def (the catalog default an instance TRACKS when its own `graph` is
/// `None`, e.g. every freshly imported model layer) can resolve bound rows
/// too — see the method's doc for the tracking-instance trap. Node
/// identity follows the expose command's own convention: the node's stable
/// id, falling back to the handle-minted id when the stable id is empty.
pub fn binding_id_for_node_param_in(
    def: &crate::effect_graph_def::EffectGraphDef,
    node_doc_id: u32,
    param_key: &str,
) -> Option<String> {
    use crate::effect_graph_def::{BindingTarget, EffectGraphNode};
    fn find_node(nodes: &[EffectGraphNode], doc_id: u32) -> Option<&EffectGraphNode> {
        for n in nodes {
            if n.id == doc_id {
                return Some(n);
            }
            if let Some(group) = &n.group
                && let Some(found) = find_node(&group.nodes, doc_id)
            {
                return Some(found);
            }
        }
        None
    }
    let node = find_node(&def.nodes, node_doc_id)?;
    let identity = if node.node_id.is_empty() {
        let handle = node.handle.clone().unwrap_or_else(|| format!("node{node_doc_id}"));
        crate::NodeId::new(handle.as_str())
    } else {
        node.node_id.clone()
    };
    let meta = def.preset_metadata.as_ref()?;
    meta.bindings
        .iter()
        .find(|b| match &b.target {
            BindingTarget::Node { node_id, param } => *node_id == identity && param == param_key,
            BindingTarget::Composite { .. } => false,
        })
        .map(|b| b.id.clone())
}

pub fn apply_card_reshape(
    value: f32,
    min: f32,
    max: f32,
    invert: bool,
    curve: crate::macro_bank::MacroCurve,
    scale: f32,
    offset: f32,
) -> f32 {
    let mut v = value;
    if invert || curve != crate::macro_bank::MacroCurve::Linear {
        let range = max - min;
        if range.abs() >= f32::EPSILON {
            let mut n = ((v - min) / range).clamp(0.0, 1.0);
            if invert {
                n = 1.0 - n;
            }
            n = curve.apply(n);
            v = min + range * n;
        }
    }
    v * scale + offset
}

/// Exact inverse of [`apply_card_reshape`] where one exists
/// (`PARAM_TWO_WAY_BINDING_DESIGN.md` D2) — the write-back direction for a
/// node-face edit on a card-bound param: given the target's new value, solve
/// for the card value that would forward-reshape to it. Returns `None` only
/// for a degenerate affine (`scale ≈ 0`, unrepresentable). Out-of-range
/// targets clamp to the slider ends, matching the forward stage-1 clamp —
/// the inverse of a clamped map is defined on the range.
///
/// Body order is the forward run reversed: affine first (undo
/// `v*scale+offset`), then — only when `invert || curve != Linear` —
/// normalize, [`crate::macro_bank::MacroCurve::inverse`], un-invert,
/// denormalize.
pub fn invert_card_reshape(
    target: f32,
    min: f32,
    max: f32,
    invert: bool,
    curve: crate::macro_bank::MacroCurve,
    scale: f32,
    offset: f32,
) -> Option<f32> {
    if scale.abs() < f32::EPSILON {
        return None;
    }
    let mut v = (target - offset) / scale;
    if invert || curve != crate::macro_bank::MacroCurve::Linear {
        let range = max - min;
        if range.abs() >= f32::EPSILON {
            let mut n = ((v - min) / range).clamp(0.0, 1.0);
            n = curve.inverse(n);
            if invert {
                n = 1.0 - n;
            }
            v = min + range * n;
        }
    }
    Some(v)
}

// ─── Param Value (per-slot state) ───

// A single parameter slot's runtime state used to live here.
// `ParamSlot` is DELETED (PARAM_STORAGE_DESIGN.md D1/D3). Its four fields
// (`value`, `base`, `exposed`, `touched`) now live on `crate::params::Param`
// alongside the descriptor (`spec`), `origin`, and `calibrated` — one struct,
// one list, id as identity. Construct a param with `Param::bundled(spec)` /
// `Param::user_added(spec)`; read/write value + base + exposed + touched
// through the manifest (`params.get(id)` / `params.get_mut(id)`).

/// Everything removed when an exposed card param is pruned from an instance:
/// its manifest [`crate::params::Param`] entry (descriptor + state), its
/// `BindingDef`, and any drivers / Ableton mappings / envelopes that
/// referenced its id — plus the display position + binding position each
/// occupied. Returned by [`PresetInstance::remove_exposures_for_node`] and
/// handed back to [`PresetInstance::restore_exposures`] so an undo restores the
/// pre-delete state byte-for-byte. Opaque to callers (the command stack just
/// carries it).
#[derive(Debug, Clone)]
pub struct RemovedExposure {
    /// Display position in the [`crate::params::ParamManifest`] this entry
    /// occupied — captured purely to restore card order via `insert_at`
    /// (PARAM_STORAGE_DESIGN.md D10: a display-order snapshot, never an
    /// identity). `None` when the pruned binding had no param entry
    /// (composite / fan-out binding with no outer slider).
    pub(super) param_position: Option<usize>,
    /// Index in `preset_metadata.bindings` the `BindingDef` occupied.
    pub(super) binding_index: usize,
    /// The removed manifest entry (descriptor + live state), or `None` for a
    /// binding that had no matching param.
    pub(super) param: Option<crate::params::Param>,
    pub(super) binding: crate::effect_graph_def::BindingDef,
    pub(super) drivers: Vec<ParameterDriver>,
    pub(super) ableton_mappings: Vec<crate::ableton_mapping::AbletonParamMapping>,
    pub(super) envelopes: Vec<ParamEnvelope>,
    pub(super) audio_mods: Vec<crate::audio_mod::ParameterAudioMod>,
}
