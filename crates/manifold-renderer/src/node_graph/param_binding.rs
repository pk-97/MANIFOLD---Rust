//! Host-visible parameter bindings — the surface between an effect's
//! UI sliders / OSC paths / Ableton macros and the inner graph nodes
//! that actually consume the values.
//!
//! See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 and
//! `docs/archive/BINDINGS_UNIFICATION_PLAN.md` for the full design.
//!
//! ## Two layers, one runtime
//!
//! Source declarations are still tiered — that's a load-bearing
//! property of the editor (registry-exposed vs user-exposed sliders),
//! the file format (`ChainSpec.bindings` vs
//! `PresetInstance.user_param_bindings`), and external addressing
//! semantics (`ParamId` namespace shared across both). What used to be
//! tiered at *runtime* is now collapsed:
//!
//! - **Source side.** [`ParamBinding`] declared in `inventory::submit!`
//!   blocks for compile-time spec bindings;
//!   `manifold_core::effects::UserParamBinding` lives on
//!   `PresetInstance` for per-instance user bindings.
//! - **Runtime side.** One [`ResolvedBinding`] type — both flavours
//!   flow through [`ResolvedBinding::from_static`] /
//!   [`ResolvedBinding::from_user`] into the same resolved shape and
//!   the same [`apply_bindings`] loop. Effect slots store a single
//!   `Vec<ResolvedBinding>` of length `n_static + n_user`. Cache
//!   entries, default-seed walks, and audit walks all see one list.
//!
//! The `[]` second-slice bug class — passing an empty slice for a tier
//! that has live data — is unrepresentable after this collapse because
//! there is no second slice.
//!
//! ## Three layers of identity
//!
//! 1. [`ParamId`] — stable string forever once shipped. External
//!    mappings (OSC, Ableton, MIDI, modulation drivers) key on this.
//!    Renaming the label or reorganising the underlying graph never
//!    invalidates a `ParamId`. The id namespace is shared between
//!    static and user bindings; lookup helpers walk both.
//! 2. [`ResolvedBinding::label`] — display string on the slider. Free
//!    to edit. Range, type flags, enum labels, and OSC suffix live on
//!    the effect's `EffectMetadata.params` entry of the same id — the
//!    binding deliberately doesn't duplicate them.
//! 3. [`ResolvedBinding::target`] — runtime routing to a graph node
//!    parameter. May change as the effect's internals are decomposed
//!    or refactored. `HandleNode` is resolved away at chain-build
//!    time; what's left is `Node` / `Composite` / `Custom`.
//!
//! ## Why `Cow<'static, str>`
//!
//! Static strings (V1: developer-defined effects compiled in) and
//! owned strings (V2: user-exposed parameters generated at runtime)
//! flow through the same code paths. `Cow::Borrowed` for compile-time
//! IDs, `Cow::Owned` for user-generated. Same trick `PresetTypeId`
//! uses.

use std::borrow::Cow;

use manifold_core::NodeId;
use manifold_core::params::ParamManifest;

use crate::node_graph::composites::CompositeHandle;
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::{ParamType, ParamValue};
use crate::node_graph::validation::GraphError;

// ParamId now lives in `manifold_core::effects` so the data-model layer
// (ParameterDriver, ParamEnvelope, AbletonParamMapping) can use the
// same type. Re-exported here for renderer-internal call sites.
pub use manifold_core::effects::ParamId;

// ─── Source declaration (compile-time spec bindings) ─────────────────

/// Compile-time spec binding declared in an effect's `inventory::submit!`
/// `ChainSpec.bindings` array.
///
/// Source-side type only — the runtime never iterates `ParamBinding`
/// directly. At chain-build time each entry flows through
/// [`ResolvedBinding::from_static`] into a [`ResolvedBinding`] in
/// `EffectSlot.bindings[0..n_static]`.
#[derive(Debug, Clone)]
pub struct ParamBinding {
    /// Stable identity. Forever rule: never rename, never reuse.
    pub id: ParamId,
    /// Display label for the outer effect-card slider and for the
    /// editor's "Effect Parameters" read-only list. Free to edit.
    pub label: &'static str,
    /// Initial value the host slot lands on when the effect is
    /// instantiated. Planted onto the inner-node target at chain build
    /// time via [`apply_binding_defaults`] so the per-frame
    /// skip-on-unchanged check in [`LastAppliedCache`] holds against
    /// an inner that already matches.
    pub default_value: f32,
    /// Where this parameter's value flows in the graph.
    pub target: ParamTarget,
    /// Conversion from f32 (UI/storage form) to the typed
    /// [`ParamValue`] the graph node expects.
    pub convert: ParamConvert,
    /// Card→consumer linear remap: `out = value * scale + offset`, applied
    /// at the write boundary via [`ResolvedBinding::from_static`]'s reshape.
    /// `1.0` / `0.0` is identity (no reshape, byte-identical). This is where
    /// a folded preset `affine_scalar` lives so the node can be deleted.
    pub scale: f32,
    pub offset: f32,
    /// The card param's value range — the normalize span for the slider
    /// response (curve/invert). Sourced from the owning `ParamSpecDef` when the
    /// binding is built from a preset; `0.0`/`1.0` for hand-built bindings.
    pub min: f32,
    pub max: f32,
    /// The card param's slider response, sourced from the preset
    /// (`ParamSpecDef.curve`/`.invert`) — the single reshape source.
    /// Applied in [`ResolvedBinding::from_static`].
    /// `Linear`/`false` is identity, so existing presets stay byte-identical.
    pub curve: manifold_core::macro_bank::MacroCurve,
    pub invert: bool,
}

/// Routing destination declared on a spec [`ParamBinding`].
///
/// Source-side only. The runtime variant is [`ResolvedTarget`], which
/// strips `HandleNode` (always resolved at build time) and keeps
/// `Node | Composite | Custom`.
#[derive(Debug, Clone)]
pub enum ParamTarget {
    /// Routed through a [`CompositeHandle`]'s exposed-param map.
    /// Used by composite-shaped effects (Mirror, SoftFocus, Bloom)
    /// where one outer name resolves to one or more inner-node
    /// parameters via the handle.
    Composite { outer_name: Cow<'static, str> },
    /// Spec-time inner-node reference by stable [`NodeId`]. Lives in the
    /// `&'static [ParamBinding]` arrays carried by a
    /// [`LoadedPresetView`] before any graph exists. Resolved into
    /// [`ResolvedTarget::Node`] at chain build time once the splice has
    /// produced its `(NodeId, NodeInstanceId)` map. The id is invariant
    /// under group / ungroup / move / flatten — unlike the node's
    /// handle, which flatten prefixes. See [`ResolvedBinding::from_static`].
    Node {
        node_id: NodeId,
        /// `Borrowed` for a compile-time/canonical param name; `Owned` for a
        /// fuse-computed field name (`n{idx}_<param>`) — the FUSION_SOTA_DESIGN
        /// D5 fix: this used to force a `Box::leak` per fuse-build (unbounded
        /// past `FUSED_CACHE_CAP`); `Cow` lets a retargeted binding own its
        /// field name instead, same trick already used by
        /// [`ParamTarget::Composite::outer_name`].
        param: Cow<'static, str>,
    },
    /// Escape hatch for routing that's neither composite nor a single
    /// node. Function pointer (no captures); for closures, build a
    /// tiny helper struct that implements [`PartialEq`] and route via
    /// `Composite`. Rare in practice.
    Custom(fn(&mut Graph, f32)),
}

/// Conversion from f32 (UI / storage form) to typed [`ParamValue`].
///
/// One enum, shared across static spec bindings and per-instance user
/// bindings — Phase 4 of the bindings unification plan merged the
/// renderer-side variant set with the core-side `ParamConvert`.
/// Re-exported from `manifold_core::effects` so the data-model layer
/// and the renderer agree on the wire/serde form. The four variants
/// here are the complete authoring surface; unit conversions and enum
/// remaps that used to live as renderer-side `EnumRemap` /
/// `FloatTransform` are now baked into the primitives themselves
/// (Transform.rot's degrees→radians, Strobe.rate's note-table
/// translation, Mirror.mode's id surface).
pub use manifold_core::effects::ParamConvert;

/// Convert one host-side f32 to the typed [`ParamValue`] the inner
/// graph node expects. Lives here rather than on the enum because
/// `ParamValue` is a renderer-side type and `ParamConvert` lives in
/// `manifold-core` (the data-model layer can't depend on the
/// renderer).
pub fn convert_param_value(convert: ParamConvert, value: f32) -> ParamValue {
    // `IntRound` rounds to a whole number but stores as `Float`. There used
    // to be a distinct `ParamValue::Int` variant; collapsing it eliminated
    // a class of silent fall-through bugs where readers only matched on
    // `Float` and silently defaulted any `Int`-typed param.
    match convert {
        ParamConvert::Float => ParamValue::Float(value),
        ParamConvert::IntRound => ParamValue::Float(value.round()),
        ParamConvert::BoolThreshold => ParamValue::Bool(value > 0.5),
        ParamConvert::EnumRound => {
            let v = value.round().max(0.0) as u32;
            ParamValue::Enum(v)
        }
        // Trigger storage is a monotonic counter held as Float —
        // pass through and let the consuming primitive detect rising
        // edges via the standard `last_count: Option<u32>` cold-start
        // pattern. Same wire shape as `system.generator_input.trigger_count`.
        ParamConvert::Trigger => ParamValue::Float(value),
    }
}

// ─── Resolved runtime binding ────────────────────────────────────────

/// Where a [`ResolvedBinding`] came from, for editor surfacing and
/// audit reports. The apply path does NOT branch on this — both
/// sources walk the same loop, hit the same cache, share the same
/// id namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingSource {
    /// Compile-time spec binding from `ChainSpec.bindings`.
    Static,
    /// Per-instance user binding from `PresetInstance.user_param_bindings`.
    User,
}

/// Runtime routing destination. The build-time-only `HandleNode`
/// variant is resolved away into `Node` before this enum is
/// constructed.
#[derive(Debug, Clone)]
pub enum ResolvedTarget {
    Composite { outer_name: Cow<'static, str> },
    Node {
        node: NodeInstanceId,
        /// Same `Cow` rationale as [`ParamTarget::Node::param`] — a resolved
        /// binding lives inside `EffectSlot` for the chain's lifetime, so an
        /// owned `Cow::Owned` field name is exactly as durable as the old
        /// leaked `&'static str`, without the leak.
        param: Cow<'static, str>,
    },
    Custom(fn(&mut Graph, f32)),
}

/// Fully-resolved per-effect binding. One per outer→inner *routing*
/// (static or user-exposed) on the slot's effect.
///
/// Stored in `EffectSlot.bindings` as a flat vector. Each entry reads its
/// value from `PresetInstance.params` (the id-keyed manifest) by
/// [`Self::source_id`] — static and user bindings alike, no positional
/// prefix/tail split.
///
/// **`source_id` is the load-bearing invariant.** It records which manifest
/// param this binding reads from. The per-frame [`apply_bindings`] loop does
/// `values.get(&binding.source_id)`, NOT `values[i]` — the id is the identity,
/// so a reorder or a neighbour delete can't misroute it (the exact bug class
/// the manifest redesign kills). Fan-out (one outer param driving multiple
/// inner targets, the way Lissajous's `clip_trigger` drives two `mux.selector`
/// targets on the generator side) is expressible by pushing two bindings with
/// the same `source_id`, and the apply loop handles it without further
/// plumbing.
#[derive(Debug, Clone)]
pub struct ResolvedBinding {
    pub id: ParamId,
    pub label: Cow<'static, str>,
    pub default_value: f32,
    pub target: ResolvedTarget,
    pub convert: ParamConvert,
    pub source: BindingSource,
    /// Manifest id this binding reads its value from. Usually equal to
    /// [`Self::id`] (1:1); a fan-out pushes two bindings with the same
    /// `source_id`. The value is `values.get(&source_id).value`, falling back
    /// to `default_value` when the id isn't in the manifest.
    pub source_id: ParamId,
    /// `Some` only for User bindings with a non-identity card mapping
    /// (invert or a non-Linear curve); `None` for static bindings and
    /// identity User bindings, which then pay nothing and stay 1:1.
    pub(crate) reshape: Option<Reshape>,
    /// `true` when the target param is a [`ParamType::Angle`] knob, so the
    /// applied value loops onto `[0, TAU)` via `rem_euclid` at the write
    /// boundary (Peter's "angles loop 0..360"). Derived from the param type
    /// at resolve time — no per-param tagging — and cached here to keep the
    /// per-frame apply path off a parameter lookup. Only ever `true` for User
    /// bindings; static / generator bindings stay `false` (their angle values
    /// are author-set and already in range). Safe because every angle consumer
    /// feeds cos/sin (2π-periodic), so the wrap is a no-op on the rendered
    /// result for in-range values and only tames a driver that climbs past 2π.
    pub(crate) wraps_angle: bool,
}

/// Non-identity card mapping applied to a User binding's value at the write
/// boundary in two stages. First the slider response: when `invert` or a
/// non-Linear `curve` is set, normalize within `[min, max]`, invert, apply the
/// curve, and scale back — this stage clamps to `[0, 1]` so the response is well
/// defined across the slider. Then the card→consumer remap `out = v*scale+offset`,
/// UNCLAMPED — this is where a folded `affine_scalar` lands, and it must not clamp
/// to stay byte-identical with the node it replaces (e.g. a deg→rad scale on a
/// value a driver may push past the slider max, which the angle wrap then tames).
/// Built (`Some`) only when at least one stage is non-identity (invert,
/// curve != Linear, scale != 1, or offset != 0), so every existing show carries
/// `None` and stays byte-identical with zero per-frame cost.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Reshape {
    min: f32,
    max: f32,
    invert: bool,
    curve: manifold_core::macro_bank::MacroCurve,
    scale: f32,
    offset: f32,
}

impl Reshape {
    /// Build a reshape from a preset param's slider response (range + curve +
    /// invert) plus the binding's card→consumer affine (scale/offset). Returns
    /// `None` when every stage is identity (Linear, no invert, scale 1, offset
    /// 0) so the binding pays nothing and stays byte-identical. The single
    /// builder shared by the effect path ([`ResolvedBinding::from_static`]) and
    /// the generator path, so both honor a preset-authored curve identically.
    pub(crate) fn from_preset_response(
        min: f32,
        max: f32,
        curve: manifold_core::macro_bank::MacroCurve,
        invert: bool,
        scale: f32,
        offset: f32,
    ) -> Option<Self> {
        (invert
            || curve != manifold_core::macro_bank::MacroCurve::Linear
            || scale != 1.0
            || offset != 0.0)
        .then_some(Self {
            min,
            max,
            invert,
            curve,
            scale,
            offset,
        })
    }

    /// Apply the slider response (clamped, only when invert/curve is set) then
    /// the card→consumer affine remap (unclamped). Delegates to
    /// [`manifold_core::effects::apply_card_reshape`] — the single definition of
    /// this pipeline, shared with the mapping-popover preview so the two never
    /// drift. A pure scale/offset fold with no invert/curve skips the
    /// normalize+clamp entirely, so it reproduces the replaced `affine_scalar`
    /// exactly.
    fn apply(&self, value: f32) -> f32 {
        manifold_core::effects::apply_card_reshape(
            value, self.min, self.max, self.invert, self.curve, self.scale, self.offset,
        )
    }
}

/// Resolve a stable [`NodeId`] to the runtime [`NodeInstanceId`] it
/// produced at splice time, via the effect's `(NodeId, NodeInstanceId)`
/// map. The map is built once per chain build from the spliced nodes —
/// node ids are unique, so the first (only) match wins. `None` when the
/// id isn't in the map (the targeted node was deleted or refactored
/// out).
fn resolve_node_id(
    node_map: &[(NodeId, NodeInstanceId)],
    node_id: &NodeId,
) -> Option<NodeInstanceId> {
    node_map
        .iter()
        .find(|(nid, _)| nid == node_id)
        .map(|(_, id)| *id)
}

impl ResolvedBinding {
    /// Resolve a spec [`ParamBinding`] against the splice's node-id
    /// map. `Node` targets resolve their [`NodeId`] to the runtime node;
    /// other variants pass through. Returns `None` when the binding's
    /// id isn't in `node_map` — caller logs and drops the orphan binding.
    ///
    /// `source_id` is the manifest id this binding reads from — the binding's
    /// own id for the 1:1 case (a fan-out caller would pass the shared driver
    /// id instead).
    ///
    /// The reshape is built from the PRESET's own slider response
    /// (`ParamSpecDef.curve`/`.invert` + range, carried on `ParamBinding`)
    /// plus the recipe's scale/offset — the single source after the
    /// per-instance `ParamMapping` note was deleted. Identity inputs
    /// (Linear, no invert, scale 1, offset 0) yield `None` and pay nothing;
    /// a preset-authored curve / a widened range (via a per-instance graph
    /// override or a fork) takes effect through this same path.
    pub fn from_static(
        b: &ParamBinding,
        node_map: &[(NodeId, NodeInstanceId)],
    ) -> Option<Self> {
        let target = match &b.target {
            ParamTarget::Composite { outer_name } => ResolvedTarget::Composite {
                outer_name: outer_name.clone(),
            },
            ParamTarget::Node { node_id, param } => {
                let node = resolve_node_id(node_map, node_id)?;
                ResolvedTarget::Node { node, param: param.clone() }
            }
            ParamTarget::Custom(f) => ResolvedTarget::Custom(*f),
        };
        let reshape =
            Reshape::from_preset_response(b.min, b.max, b.curve, b.invert, b.scale, b.offset);
        Some(Self::assemble(
            b.id.clone(),
            Cow::Borrowed(b.label),
            b.default_value,
            target,
            b.convert,
            BindingSource::Static,
            b.id.clone(),
            reshape,
            false,
        ))
    }

    /// Resolve a per-instance user binding against the splice's node-id
    /// map. Returns `None` if the binding's `node_id` isn't in
    /// `node_map` (the targeted node was deleted or refactored out) or
    /// the binding's `inner_param` doesn't match any
    /// [`crate::node_graph::ParamDef`] on the resolved node. Caller logs
    /// and skips — orphan bindings remain in the project file but render
    /// inert until they re-bind.
    ///
    /// `source_id` is the manifest id this binding reads from — the user
    /// binding's own id (`core.id`).
    pub fn from_user(
        core: &manifold_core::effects::UserParamBinding,
        graph: &Graph,
        node_map: &[(NodeId, NodeInstanceId)],
    ) -> Option<Self> {
        let target_node = resolve_node_id(node_map, &core.node_id)?;
        let inst = graph.get_node(target_node)?;
        // Pull the `&'static str` off the inner node's `ParamDef`
        // list so the resolved target carries a stable, allocation-free
        // param name (instead of leaking the user binding's owned
        // string). Capture the param type in the same pass so the write
        // boundary knows whether to loop the value (Angle knobs only).
        let target_def = inst
            .node
            .parameters()
            .iter()
            .find(|p| p.name == core.inner_param.as_str())?;
        let target_param = crate::node_graph::effect_node::intern_name(&target_def.name);
        let wraps_angle = matches!(target_def.ty, ParamType::Angle);
        let convert = match core.convert {
            manifold_core::effects::ParamConvert::Float => ParamConvert::Float,
            manifold_core::effects::ParamConvert::IntRound => ParamConvert::IntRound,
            manifold_core::effects::ParamConvert::BoolThreshold => ParamConvert::BoolThreshold,
            manifold_core::effects::ParamConvert::EnumRound => ParamConvert::EnumRound,
            manifold_core::effects::ParamConvert::Trigger => ParamConvert::Trigger,
        };
        // Only carry a reshape when the card mapping is non-identity, so
        // every existing binding (invert=false, curve=Linear, scale=1,
        // offset=0) stays 1:1. The user binding carries its own slider
        // response (min/max/invert/curve) plus a folded affine (scale/offset),
        // so this reshape is assembled inline rather than through the static
        // path's `Reshape::from_preset_response`.
        let reshape = (core.invert
            || core.curve != manifold_core::macro_bank::MacroCurve::Linear
            || core.scale != 1.0
            || core.offset != 0.0)
            .then_some(Reshape {
                min: core.min,
                max: core.max,
                invert: core.invert,
                curve: core.curve,
                scale: core.scale,
                offset: core.offset,
            });
        Some(Self::assemble(
            Cow::Owned(core.id.clone()),
            Cow::Owned(core.label.clone()),
            core.default_value,
            ResolvedTarget::Node {
                node: target_node,
                param: Cow::Borrowed(target_param),
            },
            convert,
            BindingSource::User,
            Cow::Owned(core.id.clone()),
            reshape,
            wraps_angle,
        ))
    }

    /// The single field-assembly point for a resolved binding. Every
    /// constructor — `from_static`, `from_user`, and the generator path in
    /// [`crate::generators::json_graph_generator`] — funnels through here, so
    /// a newly-added field can't be silently dropped by a second hand-rolled
    /// literal. (The generator copy once hard-coded `reshape: None` and
    /// over-drove a folded deg→rad affine 57×; this constructor is why that
    /// can't recur — there is no second literal to forget.)
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn assemble(
        id: ParamId,
        label: Cow<'static, str>,
        default_value: f32,
        target: ResolvedTarget,
        convert: ParamConvert,
        source: BindingSource,
        source_id: ParamId,
        reshape: Option<Reshape>,
        wraps_angle: bool,
    ) -> Self {
        Self {
            id,
            label,
            default_value,
            target,
            convert,
            source,
            source_id,
            reshape,
            wraps_angle,
        }
    }

    /// Apply this binding's value to the graph.
    ///
    /// `handle` is required iff `target` is [`ResolvedTarget::Composite`].
    /// Passing `None` for a `Composite` target panics — the caller is
    /// expected to know whether their effect uses composite routing.
    pub fn apply(
        &self,
        graph: &mut Graph,
        handle: Option<&CompositeHandle>,
        value: f32,
    ) -> Result<(), GraphError> {
        // Card-slider reshape (invert + response curve) for User bindings
        // that opted in; identity / static bindings skip it entirely.
        let value = match &self.reshape {
            Some(r) => r.apply(value),
            None => value,
        };
        // Loop angle knobs onto [0, TAU). No-op for in-range values (so it
        // stays byte-identical for normal slider use); only bites when a
        // driver sweeps a rotation past a full turn. rem_euclid (not %) so a
        // negative input maps to the geometrically-correct positive angle.
        let value = if self.wraps_angle {
            value.rem_euclid(std::f32::consts::TAU)
        } else {
            value
        };
        let pv = convert_param_value(self.convert, value);
        match &self.target {
            ResolvedTarget::Composite { outer_name } => handle
                .expect("ResolvedTarget::Composite requires a CompositeHandle")
                .set_param(graph, outer_name, pv),
            ResolvedTarget::Node { node, param } => graph.set_param(*node, param.as_ref(), pv),
            ResolvedTarget::Custom(f) => {
                f(graph, value);
                Ok(())
            }
        }
    }
}

// ─── Apply loop + cache ──────────────────────────────────────────────

/// Apply every binding in the unified slice against its manifest param.
/// **One walk, one manifest, one cache.** Static and user bindings share
/// the list; the apply loop doesn't care which is which — it pulls each
/// binding's value via `values.get(&binding.source_id)`.
///
/// **Why `source_id` rather than positional.** A position-keyed walk only
/// works under the 1:1 invariant (one binding per outer slot). The mirror
/// code path on the generator side (`JsonGraphGenerator`) hit a real bug
/// there — Lissajous's single `clip_trigger` toggle fan-outs to two inner
/// targets, the second was indexed past the end of the value array and
/// stayed pinned at its default forever. Routing through `source_id` (the
/// manifest is id-keyed) closes the bug class architecturally on both sides,
/// and a neighbour delete or reorder can no longer misroute a binding.
///
/// If a binding's `source_id` isn't in the manifest (the param was removed,
/// or a project file pre-dates an outer param addition), the binding falls
/// back to its own `default_value`.
///
/// **Per-binding failures are logged, not fatal.** A routing error
/// means the graph has been mutated out from under the binding (target
/// node deleted, param renamed, etc.). That's a developer bug, but it
/// MUST NOT panic the content thread mid-frame: the host runs at
/// production FPS for live performance, and a panic = channel
/// disconnect = entire pipeline stops. Log loudly, skip the broken
/// binding, keep going.
pub fn apply_bindings(
    bindings: &[ResolvedBinding],
    graph: &mut Graph,
    handle: Option<&CompositeHandle>,
    values: &ParamManifest,
    last_applied: &mut LastAppliedCache,
) {
    last_applied
        .entries
        .resize(bindings.len(), BindingCacheEntry::Unset);
    for (i, binding) in bindings.iter().enumerate() {
        let value = values
            .get(binding.source_id.as_ref())
            .map(|p| p.value)
            .unwrap_or(binding.default_value);
        match last_applied.entries[i] {
            BindingCacheEntry::Applied(prev) if prev == value => {
                continue;
            }
            BindingCacheEntry::Unset | BindingCacheEntry::Applied(_) => {}
        }
        if let Err(err) = binding.apply(graph, handle, value) {
            let tag = match binding.source {
                BindingSource::Static => "ParamBinding",
                BindingSource::User => "UserParamBinding",
            };
            eprintln!(
                "[manifold-renderer] {tag} apply failed: id={} value={} err={:?} — \
                 skipping this binding for the current frame. The graph topology likely \
                 changed without rebuilding the bindings list.",
                binding.id, value, err,
            );
            continue;
        }
        last_applied.entries[i] = BindingCacheEntry::Applied(value);
    }
}

/// Plant each binding's declared `default_value` into its inner-node
/// target at chain build time (or after a user-binding rehydrate), so
/// a freshly-built effect actually starts at the values its bindings
/// claim.
///
/// Pairs with [`LastAppliedCache::seed_from_bindings`], which pre-fills
/// the per-frame skip cache with `Applied(default_value)`. Without
/// this seed pass the cache's claim is a lie — the splice plants each
/// inner-node primitive at the primitive's own `ParamDef::default`,
/// which is often a different number than the binding's
/// `default_value` (e.g. `Blur.radius = 4.0` vs SoftFocus's outer
/// `radius.default_value = 6.0`). On the first frame [`apply_bindings`]
/// would see `Applied(default)` in the cache, find the outer slot
/// equal to the binding default, and skip the write — leaving the
/// inner stuck at the primitive's default until the user touches the
/// slider and moves the outer off the default.
///
/// Walks the unified `&[ResolvedBinding]` slice so static AND user
/// bindings both get their declared defaults planted. The latent
/// symmetric bug for user bindings (a freshly-exposed param whose
/// binding default differs from the live inner state) is closed by
/// the same call.
///
/// Per-binding failures log loudly but never panic — same contract as
/// the per-frame apply path.
pub fn apply_binding_defaults(
    bindings: &[ResolvedBinding],
    graph: &mut Graph,
    handle: Option<&CompositeHandle>,
) {
    for binding in bindings {
        if let Err(err) = binding.apply(graph, handle, binding.default_value) {
            let tag = match binding.source {
                BindingSource::Static => "ParamBinding",
                BindingSource::User => "UserParamBinding",
            };
            eprintln!(
                "[manifold-renderer] {tag} default-seed failed: id={} default={} \
                 err={:?} — inner node will run at its primitive default until the outer \
                 slot is moved off `default_value`.",
                binding.id, binding.default_value, err,
            );
        }
    }
}

/// Per-effect cache of "last outer value applied" parallel to the
/// unified bindings list. Lives on the effect instance (not on the
/// bindings) because [`ResolvedBinding`] is `Clone`able / sharable
/// between catalog constructors and tests — adding mutable cache
/// state would force every caller to wrap it in an interior-mutability
/// cell.
///
/// **Why this exists.** The binding apply path runs every frame to
/// power drivers / envelopes / Ableton mappings. Without skip-on-
/// unchanged, that turns inner-node param edits into a 60Hz tug-of-
/// war the outer always wins. With this cache, the binding writes
/// only when the outer slot's value actually changes — inner edits
/// survive when the outer is at rest, and the outer reclaims control
/// the moment it moves.
///
/// `entries` auto-grows on first [`apply_bindings`]; constructors can
/// leave it empty.
#[derive(Debug, Clone, Default)]
pub struct LastAppliedCache {
    pub entries: Vec<BindingCacheEntry>,
}

/// One entry in [`LastAppliedCache`].
///
/// - **`Unset`** — never applied. Next apply unconditionally writes
///   outer → inner, then transitions to `Applied(value)`.
/// - **`Applied(v)`** — last value the binding propagated (or, for
///   pre-seeded entries, the value the writer should pretend it
///   already wrote). Subsequent applies skip when this frame's
///   outer equals `v` and write otherwise.
///
/// Effect constructors pre-seed every entry via
/// [`LastAppliedCache::seed_from_bindings`], so a freshly-constructed
/// FX starts with `Applied(binding.default_value)` for each entry.
/// That seeding is what lets per-card edits to outer-routed inner
/// params survive a chain rebuild: as long as the outer slot stays
/// at its declared default no write fires, and the hydrated value
/// persists. The outer reclaims control the moment it diverges from
/// `binding.default_value`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BindingCacheEntry {
    #[default]
    Unset,
    Applied(f32),
}

impl LastAppliedCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset every tracked outer value so the next apply behaves as
    /// if no value has ever been written.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Truncate the cache to its static prefix so the user-tail is
    /// re-evaluated from scratch on the next apply. Called on
    /// `PresetInstance.user_param_bindings_version` bumps — exposing
    /// or unexposing an inner-graph param rebuilds the user-tail of
    /// `slot.bindings`, and the old cache entries refer to a
    /// different binding list and would skip-write on stale-prev
    /// compare otherwise.
    pub fn clear_tail(&mut self, n_static: usize) {
        if n_static < self.entries.len() {
            self.entries.truncate(n_static);
        }
    }

    /// Pre-seed each entry from the bindings' declared default
    /// values. Effect constructors call this once at build time so
    /// the cache enters the runtime with `Applied(binding.default_value)`
    /// per entry — a freshly-built effect's first [`apply_bindings`]
    /// then writes ONLY for outer slots that already diverge from
    /// their declared default, leaving everything else alone. This is
    /// the keystone of the "inner edits survive a chain rebuild"
    /// invariant: a rebuild caused by an inner-param edit doesn't
    /// move the outer slot, so the cache compare matches and the
    /// binding skips writing.
    pub fn seed_from_bindings(&mut self, bindings: &[ResolvedBinding]) {
        self.entries.clear();
        self.entries.reserve(bindings.len());
        for b in bindings {
            self.entries
                .push(BindingCacheEntry::Applied(b.default_value));
        }
    }
}

// ─── Lookups + editor projections ────────────────────────────────────

/// Read a host-visible parameter's current value by stable id. O(n)
/// over the unified slice — n is typically <10, so the scan is faster
/// than an `AHashMap` lookup at this scale and avoids per-effect
/// allocation.
///
/// Used by effects that need to inspect a param value outside the
/// normal [`apply_bindings`] flow (e.g. `should_skip` predicates).
/// Returns `None` if the id matches no binding or the binding's
/// `source_id` isn't in the manifest.
pub fn binding_value(
    bindings: &[ResolvedBinding],
    values: &ParamManifest,
    id: &str,
) -> Option<f32> {
    let b = bindings.iter().find(|b| b.id == id)?;
    values.get(b.source_id.as_ref()).map(|p| p.value)
}

/// Walk the slot's unified bindings, a composite handle, and the live
/// graph to produce the editor-facing list of
/// [`OuterParamRouting`](crate::node_graph::OuterParamRouting) entries
/// — one per outer slider whose value gets written into an inner-node
/// param every frame. Static and user bindings both surface.
///
/// Both runtime routing styles are recognized:
/// - [`ResolvedTarget::Composite`] — resolved via
///   [`CompositeHandle::inner_routing_for`].
/// - [`ResolvedTarget::Node`] — `(node_id, param)` is taken directly
///   off the binding.
///
/// [`ResolvedTarget::Custom`] is skipped; it has no introspectable
/// destination, so the editor can't surface it.
pub fn outer_routings_from_bindings(
    bindings: &[ResolvedBinding],
    handle: Option<&crate::node_graph::composites::CompositeHandle>,
    graph: &Graph,
) -> Vec<crate::node_graph::OuterParamRouting> {
    let id_to_handle: ahash::AHashMap<u32, String> = graph
        .handles()
        .map(|(h, id)| (id.0, h.to_string()))
        .collect();
    let mut out = Vec::with_capacity(bindings.len());
    for b in bindings {
        let (node_id, inner_param) = match &b.target {
            ResolvedTarget::Composite { outer_name } => {
                let Some(h) = handle else {
                    // Composite-target binding on an effect that
                    // doesn't expose a `CompositeHandle` — can't
                    // resolve. Skip rather than guess.
                    continue;
                };
                let Some((n, p)) = h.inner_routing_for(outer_name.as_ref()) else {
                    continue;
                };
                (n, Cow::Borrowed(p))
            }
            ResolvedTarget::Node { node, param } => (*node, param.clone()),
            ResolvedTarget::Custom(_) => continue,
        };
        let Some(handle_str) = id_to_handle.get(&node_id.0) else {
            // Inner node has no stable handle — without it the editor
            // can't match it to a snapshot row, so the routing is
            // un-surfaceable. Skip silently.
            continue;
        };
        out.push(crate::node_graph::OuterParamRouting {
            outer_label: b.label.to_string(),
            outer_param_id: b.id.to_string(),
            node_handle: handle_str.clone(),
            inner_param: inner_param.to_string(),
            source: match b.source {
                BindingSource::Static => crate::node_graph::OuterParamSource::Static,
                BindingSource::User => crate::node_graph::OuterParamSource::User,
            },
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::params::Param;

    /// Build a single id-keyed manifest param for test [`ParamManifest`]
    /// literals — mirrors the outer-card slot the binding runtime now
    /// reads by id instead of by position.
    fn slot(id: &str, value: f32, exposed: bool) -> Param {
        let mut p = Param::bundled(ParamSpecDef {
            id: id.into(),
            name: id.into(),
            min: 0.0,
            max: 1.0,
            default_value: value,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: vec![],
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
            card_visible: true,
        });
        p.value = value;
        p.base = value;
        p.exposed = exposed;
        p
    }

    #[test]
    fn from_static_folds_scale_offset_into_reshape() {
        let feedback = NodeInstanceId(7);
        let node_map = node_map_for(feedback);
        // Identity scale/offset → no reshape, so every un-folded preset
        // binding stays byte-identical.
        let plain = static_amount_binding();
        let rb = ResolvedBinding::from_static(&plain, &node_map).unwrap();
        assert!(
            rb.reshape.is_none(),
            "identity scale/offset must carry no reshape"
        );
        // A folded deg→rad affine_scalar (scale = π/180): the reshape must
        // reproduce the node's `a * scale` exactly. Card stores 85°, the
        // consumer must see 85·π/180 rad, byte-identical to the old node.
        let mut folded = static_amount_binding();
        folded.scale = std::f32::consts::PI / 180.0;
        let rb = ResolvedBinding::from_static(&folded, &node_map).unwrap();
        let reshape = rb.reshape.expect("non-identity scale carries a reshape");
        let expected = 85.0_f32 * std::f32::consts::PI / 180.0;
        assert!(
            (reshape.apply(85.0) - expected).abs() < 1e-6,
            "folded affine must be byte-identical, got {}",
            reshape.apply(85.0)
        );
    }

    #[test]
    fn reshape_invert_curve_and_identity() {
        use manifold_core::macro_bank::MacroCurve;
        // Identity (Linear, no invert, scale 1 / offset 0): passes through.
        let id = Reshape {
            min: 0.0,
            max: 10.0,
            invert: false,
            curve: MacroCurve::Linear,
            scale: 1.0,
            offset: 0.0,
        };
        assert!((id.apply(2.5) - 2.5).abs() < 1e-4);
        // Invert: 25% of the range becomes 75%.
        let inv = Reshape {
            min: 0.0,
            max: 10.0,
            invert: true,
            curve: MacroCurve::Linear,
            scale: 1.0,
            offset: 0.0,
        };
        assert!((inv.apply(2.5) - 7.5).abs() < 1e-4);
        // SCurve (Hermite 3t^2-2t^3): n=0.25 -> 0.15625 -> *10 = 1.5625.
        let s = Reshape {
            min: 0.0,
            max: 10.0,
            invert: false,
            curve: MacroCurve::SCurve,
            scale: 1.0,
            offset: 0.0,
        };
        assert!((s.apply(2.5) - 1.5625).abs() < 1e-3);
        // Degenerate range: passthrough, no divide-by-zero.
        let deg = Reshape {
            min: 5.0,
            max: 5.0,
            invert: false,
            curve: MacroCurve::Exponential,
            scale: 1.0,
            offset: 0.0,
        };
        assert!((deg.apply(42.0) - 42.0).abs() < 1e-6);
        // Folded affine (deg->rad): no invert/curve, so the normalize+clamp is
        // skipped and scale/offset apply to the RAW value, unclamped. 85 ->
        // 85*pi/180 = 1.4835 rad; a past-max 400 -> 6.981 (clamp would have
        // pinned it to the slider max — it must not).
        let conv = Reshape {
            min: 0.0,
            max: 360.0,
            invert: false,
            curve: MacroCurve::Linear,
            scale: std::f32::consts::PI / 180.0,
            offset: 0.0,
        };
        assert!((conv.apply(85.0) - 85.0 * std::f32::consts::PI / 180.0).abs() < 1e-5);
        assert!((conv.apply(400.0) - 400.0 * std::f32::consts::PI / 180.0).abs() < 1e-4);
    }
    use crate::node_graph::boundary_nodes::Source;
    // AffineTransform stands in for the legacy stateful-feedback
    // fixture: it has multiple `Float` params (`scale`, `translate_x`,
    // `translate_y`, `rotation`) plus port-shadow inputs, which exercise
    // the static / user / fan-out / cache code paths. `Mix` carries an
    // `Enum` `mode` param and is the fixture for the `EnumRound`
    // routing test.
    use crate::node_graph::primitives::{AffineTransform, FEEDBACK_TYPE_ID, Mix};

    // ---- Conversion tests ----

    #[test]
    fn float_passes_through_unchanged() {
        assert_eq!(convert_param_value(ParamConvert::Float, 0.0), ParamValue::Float(0.0));
        assert_eq!(convert_param_value(ParamConvert::Float, 0.5), ParamValue::Float(0.5));
        assert_eq!(convert_param_value(ParamConvert::Float, -1.5), ParamValue::Float(-1.5));
        assert_eq!(convert_param_value(ParamConvert::Float, 42.0), ParamValue::Float(42.0));
    }

    #[test]
    fn int_round_uses_half_away_from_zero() {
        // f32::round is half-away-from-zero, NOT banker's rounding.
        // Document the actual behavior so it's expected.
        assert_eq!(convert_param_value(ParamConvert::IntRound, 0.0), ParamValue::Float(0.0));
        assert_eq!(convert_param_value(ParamConvert::IntRound, 0.4), ParamValue::Float(0.0));
        assert_eq!(convert_param_value(ParamConvert::IntRound, 0.5), ParamValue::Float(1.0));
        assert_eq!(convert_param_value(ParamConvert::IntRound, 0.6), ParamValue::Float(1.0));
        assert_eq!(convert_param_value(ParamConvert::IntRound, -0.5), ParamValue::Float(-1.0));
        assert_eq!(convert_param_value(ParamConvert::IntRound, 2.5), ParamValue::Float(3.0));
    }

    #[test]
    fn bool_threshold_at_half() {
        assert_eq!(
            convert_param_value(ParamConvert::BoolThreshold, 0.0),
            ParamValue::Bool(false)
        );
        assert_eq!(
            convert_param_value(ParamConvert::BoolThreshold, 0.5),
            ParamValue::Bool(false)
        );
        assert_eq!(
            convert_param_value(ParamConvert::BoolThreshold, 0.5001),
            ParamValue::Bool(true)
        );
        assert_eq!(
            convert_param_value(ParamConvert::BoolThreshold, 1.0),
            ParamValue::Bool(true)
        );
        assert_eq!(
            convert_param_value(ParamConvert::BoolThreshold, -0.5),
            ParamValue::Bool(false)
        );
    }

    #[test]
    fn enum_round_clamps_negatives_to_zero() {
        assert_eq!(convert_param_value(ParamConvert::EnumRound, 0.0), ParamValue::Enum(0));
        assert_eq!(convert_param_value(ParamConvert::EnumRound, 1.4), ParamValue::Enum(1));
        assert_eq!(convert_param_value(ParamConvert::EnumRound, 1.6), ParamValue::Enum(2));
        assert_eq!(convert_param_value(ParamConvert::EnumRound, -1.0), ParamValue::Enum(0));
        assert_eq!(convert_param_value(ParamConvert::EnumRound, -0.4), ParamValue::Enum(0));
    }

    // EnumRemap and FloatTransform variants were removed in Phase 4
    // of the bindings unification plan — their curation moved into
    // the primitives. The corresponding tests were dropped with the
    // variants.

    // ---- Resolution helpers ----

    fn static_amount_binding() -> ParamBinding {
        ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.5,
            target: ParamTarget::Node {
                node_id: NodeId::new("feedback"),
                param: Cow::Borrowed("scale"),
            },
            convert: ParamConvert::Float,
            scale: 1.0,
            offset: 0.0,
            min: 0.0,
            max: 1.0,
            curve: manifold_core::macro_bank::MacroCurve::Linear,
            invert: false,
        }
    }

    /// The node-id resolution map: pairs the runtime node with the stable
    /// id `"feedback"` the bindings target.
    fn node_map_for(feedback: NodeInstanceId) -> Vec<(NodeId, NodeInstanceId)> {
        vec![(NodeId::new("feedback"), feedback)]
    }

    /// Add an `AffineTransform` under handle `"feedback"` AND stamp its
    /// stable node id to match — mirrors what `instantiate_def` does, so
    /// the node-id resolvers can find it.
    fn add_feedback_node(g: &mut Graph) -> NodeInstanceId {
        let id = g.add_node_named("feedback", Box::new(AffineTransform::new()));
        g.set_node_id(id, NodeId::new("feedback"));
        id
    }

    fn resolved_feedback_amount(feedback: NodeInstanceId) -> ResolvedBinding {
        ResolvedBinding {
            id: Cow::Borrowed("amount"),
            label: Cow::Borrowed("Amount"),
            default_value: 0.5,
            target: ResolvedTarget::Node {
                node: feedback,
                param: Cow::Borrowed("scale"),
            },
            convert: ParamConvert::Float,
            source: BindingSource::Static,
            source_id: Cow::Borrowed("amount"),
            reshape: None,
            wraps_angle: false,
        }
    }

    #[test]
    fn from_static_resolves_node_id_to_node() {
        let mut g = Graph::new();
        let _src = g.add_node(Box::new(Source::new()));
        let feedback = add_feedback_node(&mut g);
        let rb = ResolvedBinding::from_static(&static_amount_binding(), &node_map_for(feedback))
            .expect("node id present");
        match rb.target {
            ResolvedTarget::Node { node, param } => {
                assert_eq!(node, feedback);
                assert_eq!(param.as_ref(), "scale");
            }
            _ => panic!("expected Node target after resolution"),
        }
        assert_eq!(rb.source, BindingSource::Static);
        assert_eq!(rb.default_value, 0.5);
    }

    #[test]
    fn from_static_returns_none_when_node_id_missing() {
        // Missing node id in the splice map → orphan binding, dropped
        // at chain build time.
        let mut g = Graph::new();
        let _feedback = add_feedback_node(&mut g);
        let nope: Vec<(NodeId, NodeInstanceId)> = vec![];
        assert!(ResolvedBinding::from_static(&static_amount_binding(), &nope).is_none());
    }

    #[test]
    fn from_user_resolves_node_id_and_pulls_static_param_name() {
        let mut g = Graph::new();
        let feedback = add_feedback_node(&mut g);
        let core = manifold_core::effects::UserParamBinding {
            id: "user.feedback.zoom.1".to_string(),
            label: "User Zoom".to_string(),
            node_id: NodeId::new("feedback"),
            legacy_node_handle: None,
            inner_param: "translate_x".to_string(),
            min: 0.9,
            max: 1.1,
            default_value: 0.95,
            convert: manifold_core::effects::ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        };
        let rb = ResolvedBinding::from_user(&core, &g, &node_map_for(feedback))
            .expect("user binding hydrates");
        match rb.target {
            ResolvedTarget::Node { node, param } => {
                assert_eq!(node, feedback);
                assert_eq!(param.as_ref(), "translate_x"); // pulled off AffineTransform's ParamDef list as a &'static str
            }
            _ => panic!("user bindings always resolve to Node target"),
        }
        assert_eq!(rb.source, BindingSource::User);
    }

    #[test]
    fn from_user_returns_none_for_unknown_node_id() {
        let mut g = Graph::new();
        let _feedback = add_feedback_node(&mut g);
        let core = manifold_core::effects::UserParamBinding {
            id: "user.nope".to_string(),
            label: "Nope".to_string(),
            node_id: NodeId::new("no_such_node"),
            legacy_node_handle: None,
            inner_param: "translate_x".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.5,
            convert: manifold_core::effects::ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        };
        let node_map: Vec<(NodeId, NodeInstanceId)> = vec![];
        assert!(ResolvedBinding::from_user(&core, &g, &node_map).is_none());
    }

    #[test]
    fn from_user_returns_none_for_unknown_inner_param() {
        let mut g = Graph::new();
        let feedback = add_feedback_node(&mut g);
        let core = manifold_core::effects::UserParamBinding {
            id: "user.feedback.bogus.1".to_string(),
            label: "Bogus".to_string(),
            node_id: NodeId::new("feedback"),
            legacy_node_handle: None,
            inner_param: "bogus_param".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.5,
            convert: manifold_core::effects::ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        };
        assert!(ResolvedBinding::from_user(&core, &g, &node_map_for(feedback)).is_none());
    }

    // ---- apply() routing tests ----

    #[test]
    fn apply_node_target_writes_param_to_graph() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(AffineTransform::new()));
        let binding = resolved_feedback_amount(feedback);
        binding.apply(&mut g, None, 0.75).unwrap();
        let inst = g.get_node(feedback).unwrap();
        assert_eq!(inst.params.get("scale"), Some(&ParamValue::Float(0.75)));
    }

    /// PARAM_RANGE_CONTRACT_DESIGN.md D3/invariant table: a hint (declared
    /// `range`, the display-only slider span) never restricts a write
    /// through the real param-binding write path — the same path a card
    /// binding write takes. `AffineTransform.scale`'s declared hint is
    /// `[0.1, 5.0]`; writing 50.0 (10× past the hint max) through
    /// `ResolvedBinding::apply` must read back intact, unclamped.
    #[test]
    fn apply_writes_out_of_hint_value_intact_unclamped() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(AffineTransform::new()));
        let binding = resolved_feedback_amount(feedback);
        binding.apply(&mut g, None, 50.0).unwrap();
        let inst = g.get_node(feedback).unwrap();
        assert_eq!(
            inst.params.get("scale"),
            Some(&ParamValue::Float(50.0)),
            "a value 10x past scale's declared hint max (5.0) must round-trip \
             through the write boundary unclamped — hints are display spans, \
             never restrictions"
        );
    }

    #[test]
    fn apply_node_target_doesnt_need_handle() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(AffineTransform::new()));
        let binding = resolved_feedback_amount(feedback);
        // None handle should be fine for Node target.
        assert!(binding.apply(&mut g, None, 0.5).is_ok());
    }

    #[test]
    fn apply_to_unknown_param_returns_graph_error() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(AffineTransform::new()));
        let binding = ResolvedBinding {
            id: Cow::Borrowed("nonexistent"),
            label: Cow::Borrowed("Nonexistent"),
            default_value: 0.0,
            target: ResolvedTarget::Node {
                node: feedback,
                param: Cow::Borrowed("nonexistent"),
            },
            convert: ParamConvert::Float,
            source: BindingSource::Static,
            source_id: Cow::Borrowed("nonexistent"),
            reshape: None,
            wraps_angle: false,
        };
        let err = binding.apply(&mut g, None, 0.5).unwrap_err();
        assert!(matches!(err, GraphError::ParamNotFound { .. }));
    }

    #[test]
    fn enum_round_routes_correctly_to_a_real_node() {
        // Verifies the full path: f32 → EnumRound → ParamValue::Enum →
        // graph.set_param. Using Mix's `mode` param (7-value Enum:
        // Lerp / Screen / Add / Max / Multiply / Difference / Overlay).
        let mut g = Graph::new();
        let mix = g.add_node(Box::new(Mix::new()));
        let binding = ResolvedBinding {
            id: Cow::Borrowed("mode"),
            label: Cow::Borrowed("Mode"),
            default_value: 0.0,
            target: ResolvedTarget::Node {
                node: mix,
                param: Cow::Borrowed("mode"),
            },
            convert: ParamConvert::EnumRound,
            source: BindingSource::Static,
            source_id: Cow::Borrowed("mode"),
            reshape: None,
            wraps_angle: false,
        };
        binding.apply(&mut g, None, 0.0).unwrap();
        assert_eq!(
            g.get_node(mix).unwrap().params.get("mode"),
            Some(&ParamValue::Enum(0))
        );
        binding.apply(&mut g, None, 2.0).unwrap();
        assert_eq!(
            g.get_node(mix).unwrap().params.get("mode"),
            Some(&ParamValue::Enum(2))
        );
    }

    #[test]
    fn binding_id_is_independent_of_label_and_target_param() {
        // The ID is the stable mapping key. It's allowed (and expected
        // in some cases) to differ from both the slider label and the
        // inner-node param name. Test confirms nothing in the routing
        // code conflates the three.
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(AffineTransform::new()));
        let binding = ResolvedBinding {
            id: Cow::Borrowed("blend_strength"),
            label: Cow::Borrowed("Blend Strength"),
            default_value: 0.5,
            target: ResolvedTarget::Node {
                node: feedback,
                param: Cow::Borrowed("scale"),
            },
            convert: ParamConvert::Float,
            source: BindingSource::Static,
            source_id: Cow::Borrowed("blend_strength"),
            reshape: None,
            wraps_angle: false,
        };
        binding.apply(&mut g, None, 0.42).unwrap();
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("scale"),
            Some(&ParamValue::Float(0.42))
        );
        assert!(
            g.get_node(feedback)
                .unwrap()
                .params
                .get("blend_strength")
                .is_none()
        );
    }

    #[test]
    fn wraps_angle_loops_applied_value_onto_tau() {
        use std::f32::consts::TAU;
        let mut g = Graph::new();
        let node = g.add_node(Box::new(AffineTransform::new()));
        // A binding flagged as an angle knob loops the applied value onto
        // [0, TAU) at the write boundary. The target slot is a plain float;
        // the test pins the wrap arithmetic itself, independent of whether
        // the slot is semantically an angle.
        let binding = ResolvedBinding {
            id: Cow::Borrowed("rot"),
            label: Cow::Borrowed("Rotation"),
            default_value: 0.0,
            target: ResolvedTarget::Node {
                node,
                param: Cow::Borrowed("scale"),
            },
            convert: ParamConvert::Float,
            source: BindingSource::User,
            source_id: Cow::Borrowed("rot"),
            reshape: None,
            wraps_angle: true,
        };
        // 2.5 turns in -> 0.5 turn out.
        binding.apply(&mut g, None, TAU * 2.5).unwrap();
        let got = match g.get_node(node).unwrap().params.get("scale") {
            Some(ParamValue::Float(v)) => *v,
            other => panic!("expected float, got {other:?}"),
        };
        assert!((got - TAU * 0.5).abs() < 1e-4, "wrap 2.5 turns: got {got}");
        // A negative angle maps into the positive range: -0.1 -> TAU - 0.1.
        binding.apply(&mut g, None, -0.1).unwrap();
        let got2 = match g.get_node(node).unwrap().params.get("scale") {
            Some(ParamValue::Float(v)) => *v,
            other => panic!("expected float, got {other:?}"),
        };
        assert!(
            (got2 - (TAU - 0.1)).abs() < 1e-4,
            "wrap negative: got {got2}"
        );
        // In-range value is untouched (byte-identical for normal slider use).
        binding.apply(&mut g, None, 1.0).unwrap();
        let got3 = match g.get_node(node).unwrap().params.get("scale") {
            Some(ParamValue::Float(v)) => *v,
            other => panic!("expected float, got {other:?}"),
        };
        assert_eq!(got3, 1.0, "in-range value must pass through untouched");
    }

    // ---- apply_bindings + cache tests ----

    #[test]
    fn apply_bindings_iterates_with_default_fallback() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(AffineTransform::new()));
        let bindings = vec![
            resolved_feedback_amount(feedback),
            ResolvedBinding {
                id: Cow::Borrowed("zoom"),
                label: Cow::Borrowed("Zoom"),
                default_value: 0.95,
                target: ResolvedTarget::Node {
                    node: feedback,
                    param: Cow::Borrowed("translate_x"),
                },
                convert: ParamConvert::Float,
                source: BindingSource::Static,
                source_id: Cow::Borrowed("zoom"),
                reshape: None,
                wraps_angle: false,
            },
        ];

        // Provide only one value — second falls back to default 0.95
        // (its `source_id`, "zoom", isn't in the manifest at all).
        apply_bindings(
            &bindings,
            &mut g,
            None,
            &ParamManifest::from_params(vec![slot("amount", 0.5, true)]),
            &mut LastAppliedCache::new(),
        );
        let inst = g.get_node(feedback).unwrap();
        assert_eq!(inst.params.get("scale"), Some(&ParamValue::Float(0.5)));
        assert_eq!(inst.params.get("translate_x"), Some(&ParamValue::Float(0.95)));
    }

    /// Combined static + user bindings in one slice. Both halves
    /// apply on a single `apply_bindings` call. After Phase 1 there is
    /// no second slice to forget.
    #[test]
    fn apply_bindings_walks_static_then_user_in_one_slice() {
        let mut g = Graph::new();
        let feedback = add_feedback_node(&mut g);

        let static_rb = resolved_feedback_amount(feedback);
        let core_ub = manifold_core::effects::UserParamBinding {
            id: "user.feedback.zoom.1".to_string(),
            label: "User Zoom".to_string(),
            node_id: NodeId::new("feedback"),
            legacy_node_handle: None,
            inner_param: "translate_x".to_string(),
            min: 0.9,
            max: 1.1,
            default_value: 0.95,
            convert: manifold_core::effects::ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        };
        let user_rb = ResolvedBinding::from_user(&core_ub, &g, &node_map_for(feedback)).unwrap();
        let bindings = vec![static_rb, user_rb];

        // manifest: amount=0.5, user.feedback.zoom.1=1.05
        apply_bindings(
            &bindings,
            &mut g,
            None,
            &ParamManifest::from_params(vec![
                slot("amount", 0.5, true),
                slot("user.feedback.zoom.1", 1.05, true),
            ]),
            &mut LastAppliedCache::new(),
        );
        let inst = g.get_node(feedback).unwrap();
        assert_eq!(inst.params.get("scale"), Some(&ParamValue::Float(0.5)));
        assert_eq!(inst.params.get("translate_x"), Some(&ParamValue::Float(1.05)));
    }

    #[test]
    fn apply_bindings_user_tail_falls_back_to_binding_default() {
        let mut g = Graph::new();
        let feedback = add_feedback_node(&mut g);
        let static_rb = resolved_feedback_amount(feedback);
        let core_ub = manifold_core::effects::UserParamBinding {
            id: "user.feedback.zoom.1".to_string(),
            label: "User Zoom".to_string(),
            node_id: NodeId::new("feedback"),
            legacy_node_handle: None,
            inner_param: "translate_x".to_string(),
            min: 0.9,
            max: 1.1,
            default_value: 0.97,
            convert: manifold_core::effects::ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        };
        let user_rb = ResolvedBinding::from_user(&core_ub, &g, &node_map_for(feedback)).unwrap();
        let bindings = vec![static_rb, user_rb];

        // manifest lacks the user id entirely: user tail falls back to
        // binding's `default_value` (0.97).
        apply_bindings(
            &bindings,
            &mut g,
            None,
            &ParamManifest::from_params(vec![slot("amount", 0.5, true)]),
            &mut LastAppliedCache::new(),
        );
        let inst = g.get_node(feedback).unwrap();
        assert_eq!(inst.params.get("translate_x"), Some(&ParamValue::Float(0.97)));
    }

    #[test]
    fn binding_value_finds_id_in_either_tier() {
        let mut g = Graph::new();
        let feedback = add_feedback_node(&mut g);
        let static_rb = resolved_feedback_amount(feedback);
        let core_ub = manifold_core::effects::UserParamBinding {
            id: "user.feedback.zoom.1".to_string(),
            label: "User Zoom".to_string(),
            node_id: NodeId::new("feedback"),
            legacy_node_handle: None,
            inner_param: "translate_x".to_string(),
            min: 0.9,
            max: 1.1,
            default_value: 0.95,
            convert: manifold_core::effects::ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        };
        let user_rb = ResolvedBinding::from_user(&core_ub, &g, &node_map_for(feedback)).unwrap();
        let bindings = vec![static_rb, user_rb];
        let values = ParamManifest::from_params(vec![
            slot("amount", 0.5, true),
            slot("user.feedback.zoom.1", 1.07, true),
        ]);
        assert_eq!(binding_value(&bindings, &values, "amount"), Some(0.5));
        assert_eq!(
            binding_value(&bindings, &values, "user.feedback.zoom.1"),
            Some(1.07)
        );
        assert_eq!(binding_value(&bindings, &values, "nope"), None);
    }

    /// Architectural guard that mirrors the generator-side regression
    /// `fan_out_binding_writes_every_target_with_the_same_outer_value`.
    /// Effects don't currently express fan-out (one outer slot →
    /// multiple inner-node params) — the 1:1 invariant holds by
    /// construction today. But the apply loop must not assume binding
    /// position equals source position, so a future shape change can't
    /// silently strand a fan-out target on its default. This test
    /// constructs two bindings sharing a `source_id` and proves
    /// both inner targets receive the same outer value from one manifest
    /// param.
    #[test]
    fn apply_bindings_supports_fan_out_when_two_bindings_share_source_index() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(AffineTransform::new()));
        // Two distinct inner targets, both reading from the "amount" param.
        let bindings = vec![
            ResolvedBinding {
                id: Cow::Borrowed("amount"),
                label: Cow::Borrowed("Amount"),
                default_value: 0.0,
                target: ResolvedTarget::Node {
                    node: feedback,
                    param: Cow::Borrowed("scale"),
                },
                convert: ParamConvert::Float,
                source: BindingSource::Static,
                source_id: Cow::Borrowed("amount"),
                reshape: None,
                wraps_angle: false,
            },
            ResolvedBinding {
                id: Cow::Borrowed("zoom"),
                label: Cow::Borrowed("Zoom (shared source)"),
                default_value: 0.0,
                target: ResolvedTarget::Node {
                    node: feedback,
                    param: Cow::Borrowed("translate_x"),
                },
                convert: ParamConvert::Float,
                source: BindingSource::Static,
                source_id: Cow::Borrowed("amount"), // same manifest param as `amount`
                reshape: None,
                wraps_angle: false,
            },
        ];
        // ONE outer value, applied to BOTH inner targets.
        apply_bindings(
            &bindings,
            &mut g,
            None,
            &ParamManifest::from_params(vec![slot("amount", 0.42, true)]),
            &mut LastAppliedCache::new(),
        );
        let inst = g.get_node(feedback).unwrap();
        assert_eq!(
            inst.params.get("scale"),
            Some(&ParamValue::Float(0.42)),
            "first binding (amount) must receive the outer value",
        );
        assert_eq!(
            inst.params.get("translate_x"),
            Some(&ParamValue::Float(0.42)),
            "second binding (zoom) sharing source_index=0 must ALSO receive \
             the outer value 0.42, NOT the binding's default. Pre-source_index, \
             the positional walk would have read values[1] = past-end → default \
             0.0, which is the bug class this guard locks shut.",
        );
    }

    /// First apply writes; second apply with the same outer value
    /// skips. Validates the per-frame no-op invariant that lets inner
    /// edits survive against an unchanging outer routing.
    #[test]
    fn apply_bindings_skips_when_outer_value_unchanged() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(AffineTransform::new()));
        let bindings = vec![resolved_feedback_amount(feedback)];
        let values = ParamManifest::from_params(vec![slot("amount", 0.5, true)]);
        let mut cache = LastAppliedCache::new();

        // 1st apply: write should land — amount goes from 0.0 default
        // → 0.5.
        apply_bindings(&bindings, &mut g, None, &values, &mut cache);
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("scale"),
            Some(&ParamValue::Float(0.5)),
        );
        assert_eq!(cache.entries[0], BindingCacheEntry::Applied(0.5));

        // Simulate the inspector editing the inner directly while
        // the outer slot is at rest.
        g.set_param(feedback, "scale", ParamValue::Float(0.9))
            .unwrap();

        // 2nd apply with the same outer value: skip — inner edit
        // must survive.
        apply_bindings(&bindings, &mut g, None, &values, &mut cache);
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("scale"),
            Some(&ParamValue::Float(0.9)),
            "skip-on-unchanged must not overwrite the inner edit",
        );
    }

    /// When the outer slot changes (drag, envelope, driver), the
    /// binding writes again and overwrites any inner edit. Confirms
    /// the outer reclaims control as soon as it moves.
    #[test]
    fn apply_bindings_writes_when_outer_value_changes() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(AffineTransform::new()));
        let bindings = vec![resolved_feedback_amount(feedback)];
        let mut cache = LastAppliedCache::new();

        apply_bindings(
            &bindings,
            &mut g,
            None,
            &ParamManifest::from_params(vec![slot("amount", 0.5, true)]),
            &mut cache,
        );
        // Inner edit.
        g.set_param(feedback, "scale", ParamValue::Float(0.9))
            .unwrap();
        // Outer slot moves: 0.5 → 0.25. Binding writes.
        apply_bindings(
            &bindings,
            &mut g,
            None,
            &ParamManifest::from_params(vec![slot("amount", 0.25, true)]),
            &mut cache,
        );
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("scale"),
            Some(&ParamValue::Float(0.25)),
            "outer change must overwrite the inner edit",
        );
        assert_eq!(cache.entries[0], BindingCacheEntry::Applied(0.25));
    }

    /// Per-frame outer animation (envelope / driver) writes every
    /// frame the value advances. Confirms the cache doesn't trap
    /// active automation.
    #[test]
    fn apply_bindings_keeps_writing_under_continuous_automation() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(AffineTransform::new()));
        let bindings = vec![resolved_feedback_amount(feedback)];
        let mut cache = LastAppliedCache::new();

        for (i, v) in [0.10_f32, 0.20, 0.30, 0.40].iter().enumerate() {
            apply_bindings(
                &bindings,
                &mut g,
                None,
                &ParamManifest::from_params(vec![slot("amount", *v, true)]),
                &mut cache,
            );
            assert_eq!(
                g.get_node(feedback).unwrap().params.get("scale"),
                Some(&ParamValue::Float(*v)),
                "frame {i}: animated outer must overwrite inner",
            );
        }
    }

    /// Pre-seeded cache + first apply: the value the def installed
    /// is preserved on the next frame when the outer slot is at its
    /// declared default. Mirrors the chain-rebuild-after-inner-edit
    /// case end-to-end.
    #[test]
    fn seeded_cache_preserves_hydrated_inner_against_outer_default() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(AffineTransform::new()));
        let bindings = vec![resolved_feedback_amount(feedback)];
        // Default = 0.5 from `resolved_feedback_amount`. Constructor
        // would seed cache to `Applied(0.5)` — simulate that.
        let mut cache = LastAppliedCache::new();
        cache.seed_from_bindings(&bindings);

        // Pretend hydrate just installed inner amount = 0.9.
        g.set_param(feedback, "scale", ParamValue::Float(0.9))
            .unwrap();

        // First apply with the catalog-default outer (0.5): cache
        // already says we applied 0.5, so the binding skips and the
        // hydrated value persists.
        apply_bindings(
            &bindings,
            &mut g,
            None,
            &ParamManifest::from_params(vec![slot("amount", 0.5, true)]),
            &mut cache,
        );
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("scale"),
            Some(&ParamValue::Float(0.9)),
            "seeded cache must not overwrite the hydrated value when outer is at default",
        );
    }

    #[test]
    fn seeded_cache_lets_outer_drag_reclaim_control() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(AffineTransform::new()));
        let bindings = vec![resolved_feedback_amount(feedback)];
        let mut cache = LastAppliedCache::new();
        cache.seed_from_bindings(&bindings);

        g.set_param(feedback, "scale", ParamValue::Float(0.9))
            .unwrap();

        // First apply with outer at default — seeded cache matches,
        // skip, hydrated value persists.
        apply_bindings(
            &bindings,
            &mut g,
            None,
            &ParamManifest::from_params(vec![slot("amount", 0.5, true)]),
            &mut cache,
        );
        // Outer moves to 0.2 → cache says last applied was 0.5,
        // values differ, write fires, inner reclaims.
        apply_bindings(
            &bindings,
            &mut g,
            None,
            &ParamManifest::from_params(vec![slot("amount", 0.2, true)]),
            &mut cache,
        );
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("scale"),
            Some(&ParamValue::Float(0.2)),
            "outer-drag after a seeded-cache skip must reclaim control",
        );
    }

    /// Regression: repeated chain rebuilds (apply_graph_def firing every
    /// frame) must not stop outer-card slider drags from propagating.
    /// Reseeding the cache from the bindings is idempotent — first
    /// apply after each reseed only writes when outer differs from
    /// declared default.
    #[test]
    fn repeated_seed_does_not_block_outer_drag() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(AffineTransform::new()));
        let bindings = vec![resolved_feedback_amount(feedback)];
        let mut cache = LastAppliedCache::new();
        cache.seed_from_bindings(&bindings);

        cache.seed_from_bindings(&bindings); // simulate rebuild
        apply_bindings(
            &bindings,
            &mut g,
            None,
            &ParamManifest::from_params(vec![slot("amount", 0.7, true)]),
            &mut cache,
        );
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("scale"),
            Some(&ParamValue::Float(0.7)),
        );

        cache.seed_from_bindings(&bindings);
        apply_bindings(
            &bindings,
            &mut g,
            None,
            &ParamManifest::from_params(vec![slot("amount", 0.7, true)]),
            &mut cache,
        );
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("scale"),
            Some(&ParamValue::Float(0.7)),
            "repeated reseed must not strand the inner at the wrong value",
        );
    }

    /// `clear_tail` drops only the user-tail entries — the static
    /// prefix's cache entries (and their skip-on-unchanged claim) must
    /// survive a user-binding rehydrate.
    #[test]
    fn clear_tail_preserves_static_prefix_cache_entries() {
        let mut cache = LastAppliedCache::new();
        cache.entries = vec![
            BindingCacheEntry::Applied(0.5),
            BindingCacheEntry::Applied(0.7),
            BindingCacheEntry::Applied(0.9),
        ];
        cache.clear_tail(1); // n_static = 1
        assert_eq!(cache.entries, vec![BindingCacheEntry::Applied(0.5)]);
    }

    #[test]
    fn unused_type_id_constant_compiles() {
        // Suppress unused-import warning for FEEDBACK_TYPE_ID and
        // document the stable type-id contract — saved graphs reference
        // this string, so it must not drift.
        assert_eq!(FEEDBACK_TYPE_ID, "node.feedback");
    }
}
