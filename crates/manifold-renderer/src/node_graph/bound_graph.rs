//! [`BoundGraph`] — the shared per-frame binding lifecycle for a graph that an
//! outer card drives.
//!
//! Effects and generators both run a node graph whose inner-node params are
//! driven by outer-card sliders through [`ResolvedBinding`]s. The *apply
//! primitives* ([`apply_bindings`], [`LastAppliedCache`], [`ResolvedBinding`])
//! have been shared for a while, but the **orchestration around them** — resolve,
//! cache, push def overrides, re-assert bindings — was written twice (the effect
//! chain in `effect_chain_graph.rs`, the generator in `json_graph_generator.rs`),
//! and the two copies drifted. That drift is a bug factory:
//!
//! - The generator's "push def inner-param overrides" path forgot to clear the
//!   binding cache afterward, so a bound slider (OilyFluid Speed) stuck at the
//!   baked def value whenever a node-position edit landed — the effect chain
//!   cleared it, the generator didn't (`903deaa8`).
//!
//! `BoundGraph` is the single home for that orchestration so it can't diverge
//! again. It owns the resolved binding list + the skip-on-unchanged cache, and
//! exposes the two operations both runtimes need every frame:
//!
//! - [`BoundGraph::apply`] — push the outer-card values through the bindings
//!   (skipping unchanged slots).
//! - [`BoundGraph::apply_inner_overrides`] — push a def's inner-node param values
//!   into the live graph **and clear the cache** so the live bindings re-assert
//!   over them on the next [`apply`](BoundGraph::apply). Pushing the def value
//!   into *every* inner node — including the ones a slider drives — is why the
//!   cache must be cleared: without it, the next `apply` sees the outer value
//!   unchanged and skips the write, leaving the bound inner param stuck at the
//!   def default. The clear lives *inside* this method, so neither runtime can
//!   forget it.
//!
//! Binding *construction* still differs per side (effects build a static prefix +
//! a user tail off `PresetInstance`; generators resolve everything from the def's
//! `preset_metadata.bindings`), and the note-reshape / user-tail rebuild paths
//! still live in each runtime for now — those fold in as the storage unification
//! (`docs/PRESET_UNIFICATION_PLAN.md` Phases 1b/4-struct) progresses. This is the
//! first shared seam: the per-frame apply + override-and-clear.

use ahash::AHashMap;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::ParamManifest;

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::param_binding::{
    LastAppliedCache, ResolvedBinding, ResolvedTarget, apply_binding_defaults, apply_bindings,
};
use crate::node_graph::parameters::ParamValue;

/// The resolved binding list for one card-driven graph plus its skip-on-unchanged
/// cache. Owned by an effect's `EffectSlot` and a `JsonGraphGenerator` alike.
pub struct BoundGraph {
    /// Outer-card → inner-node bindings, resolved against the live graph. For
    /// effects this is the static prefix (`bindings[..n_static]`) followed by the
    /// user tail; for generators it is the preset's full binding list. Either way
    /// [`apply_bindings`] walks it against `cache` in lockstep with the host's
    /// `param_values`.
    pub bindings: Vec<ResolvedBinding>,
    /// Per-binding skip-on-unchanged cache. Seeded with each binding's declared
    /// default at construction so a freshly-built graph only writes the slots that
    /// already diverge from their default.
    pub cache: LastAppliedCache,
    /// The fused view's retarget map: `(original node_id, param)` →
    /// `(fused node_id, n{i}_field)`. Empty on an unfused graph (the live editor
    /// path). When a card renders FUSED, an inner-node param edit / undo targets a
    /// node that was collapsed into the fused kernel and so no longer appears in the
    /// `node_map` [`apply_inner_param_overrides`] resolves against — the override
    /// silently no-ops and the stale baked value keeps rendering until an unrelated
    /// rebuild (BUG-006). [`apply_inner_overrides`](Self::apply_inner_overrides)
    /// consults this on a `node_map` miss to route the value onto the fused field,
    /// the same repoint the card + user bindings already go through. Populated by
    /// the chain builder from `view.fused_retarget` right after construction.
    pub fused_retarget: AHashMap<(String, String), (NodeId, String)>,
    /// Every def-baked node param this graph's card bindings threw away at
    /// construction (see [`find_shadowed_def_params`]). Non-empty means someone
    /// wrote a value into a node param that a card owns, and the card won. The
    /// host drains this into its structured diagnostic channel
    /// (`ChainError::CardBindingShadowsDefParam`) right after construction, which
    /// is also where the logging lives — only the host knows which preset this is,
    /// and therefore whether the finding is a known one ([`is_baseline_shadow`]).
    pub shadowed_def_params: Vec<ShadowedDefParam>,
}

/// One def-baked node param value that a card binding overwrote at build.
///
/// The finding for BUG-1l7f (imported-def param no-op): setting a param on a
/// node whose card binding carries an AUTHORED default is a silent revert —
/// [`apply_binding_defaults`] plants that default over it the moment the graph is
/// bound, so the value never reaches evaluation. It cost `rt_r3_heldout_gltf` its
/// whole existence (it set `rt_enabled` on an imported def's `render_scene` node
/// and compared two pure-raster renders while asserting an RT conclusion) and
/// cost a second lane a misdiagnosis on top.
///
/// A MIRRORED default no longer plants (BUG-ji6q), so scene-vocabulary params —
/// transforms, materials, lights, cameras, the `render_scene` RT toggles — keep
/// the node value and are never reported here. What remains is every authored
/// default: a preset's declared resting value, and an import's curated bindings
/// (`env_mode`, `ssao_intensity`, the sun fan-outs, `env_intensity`).
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowedDefParam {
    /// Stable id of the node whose param was overwritten.
    pub node_id: String,
    /// The param name on that node.
    pub param: String,
    /// What the def asked for.
    pub def_value: ParamValue,
    /// The card param that owns it — the route that DOES work.
    pub binding_id: String,
    /// What the binding wrote instead.
    pub binding_default: ParamValue,
}

impl std::fmt::Display for ShadowedDefParam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "node `{}`.{} was set to {:?} in the def, but card param `{}` owns it and \
             wrote {:?} at build — the def value never reaches evaluation. Drive it \
             through the card manifest (`{}`) instead of the node param.",
            self.node_id,
            self.param,
            self.def_value,
            self.binding_id,
            self.binding_default,
            self.binding_id,
        )
    }
}

impl BoundGraph {
    /// Build from an already-resolved binding list, seeding the cache and planting
    /// each binding's declared default into its inner-node target so the cache's
    /// `Applied(default)` claim is true on the first frame (see
    /// [`apply_binding_defaults`]).
    ///
    /// `def` is the graph's own def — the one `graph` was built from — and is
    /// what makes the default-planting audible: every value it bakes into a
    /// card-owned node param is about to be thrown away, so
    /// [`find_shadowed_def_params`] records it on
    /// [`Self::shadowed_def_params`] for the host to surface. It is a required
    /// argument rather than a setter so a new call site can't be written that
    /// silently skips the check; pass `None` only where there is genuinely no def
    /// (a hand-built graph in a unit test).
    pub fn new(
        bindings: Vec<ResolvedBinding>,
        graph: &mut Graph,
        def: Option<&EffectGraphDef>,
    ) -> Self {
        let shadowed_def_params = def
            .map(|d| find_shadowed_def_params(d, &bindings, graph))
            .unwrap_or_default();
        let mut cache = LastAppliedCache::new();
        cache.seed_from_bindings(&bindings);
        apply_binding_defaults(&bindings, graph, None);
        Self {
            bindings,
            cache,
            fused_retarget: AHashMap::default(),
            shadowed_def_params,
        }
    }

    /// Push the host's outer-card values through the bindings, skipping slots whose
    /// outer value hasn't changed since last frame. The per-frame hot call.
    pub fn apply(&mut self, graph: &mut Graph, values: &ParamManifest) {
        apply_bindings(&self.bindings, graph, None, values, &mut self.cache);
    }

    /// Push `def`'s inner-node param values into the live `graph` for every node
    /// present in `node_map`, then clear the binding cache so the live bindings
    /// re-assert over what was just written on the next [`apply`](Self::apply).
    ///
    /// This is the single home for an editing-time graph-version edit (a value or
    /// position change that doesn't rebuild the graph). The cache clear is the
    /// load-bearing part: the override pushes the def value into *every* inner
    /// node, including ones a slider drives, so without re-asserting the bindings
    /// a bound param would stick at the def value (the OilyFluid Speed snap-back).
    /// Both runtimes route through here, so the clear can't be forgotten on one
    /// side and not the other.
    pub fn apply_inner_overrides(
        &mut self,
        graph: &mut Graph,
        node_map: &[(NodeId, NodeInstanceId)],
        def: Option<&EffectGraphDef>,
    ) {
        self.apply_inner_overrides_prefixed(graph, node_map, def, "");
    }

    /// [`Self::apply_inner_overrides`], but with `def`'s node ids translated
    /// through `prefix` before every `node_map` / `fused_retarget` lookup —
    /// the segment case (BUG-111). A card that is a member of a fused
    /// multi-card SEGMENT has its own def's node ids UNPREFIXED, but the
    /// segment's `node_map` (built from the concatenated segment def) and
    /// `fused_retarget` (populated from the segment view's retarget map) are
    /// both keyed with the `c{i}.` per-card prefix
    /// (`freeze::segment::card_prefix`). Without translating here, a
    /// surviving node `foo` is `c{i}.foo` in `node_map` and a fused-away node
    /// isn't in `fused_retarget` either — every override silently no-ops.
    /// `prefix` is `""` for a non-segment (whole-card or whole-generator)
    /// slot, so this is a no-op clone-free path there.
    pub fn apply_inner_overrides_prefixed(
        &mut self,
        graph: &mut Graph,
        node_map: &[(NodeId, NodeInstanceId)],
        def: Option<&EffectGraphDef>,
        prefix: &str,
    ) {
        apply_inner_param_overrides(def, node_map, graph, &self.fused_retarget, prefix);
        self.cache.clear();
    }
}

/// Every param `def` bakes onto a node that one of `bindings` owns, where the
/// baked value is not what the binding is about to write.
///
/// This is the silent-revert detector for BUG-1l7f. The write path is
/// [`apply_binding_defaults`], which plants each binding's `default_value` onto
/// its target the moment a [`BoundGraph`] is constructed: a def-baked value on a
/// card-owned param is therefore dead on arrival, with no error and no wrong
/// pixel until much later. Findings are reported, never repaired — whether the
/// node-param route SHOULD win over the card default is a live-performance
/// semantics decision (cards are the performer's surface), not something this
/// function gets to make.
///
/// Exactly the disagreements that matter, and nothing else:
/// - only bindings whose default is AUTHORED. A `default_mirrors_node_param`
///   binding is a stamp-time snapshot of the node param, and
///   [`apply_binding_defaults`] no longer plants those at all (BUG-ji6q), so the
///   node value stands and there is nothing to report. This is the discriminator
///   that keeps the diagnostic pointed at the class that is still silent: an
///   AUTHORED default — a number someone chose, carrying the binding's
///   `scale`/`offset` fold — really does overwrite whatever the def baked.
/// - only params the def EXPLICITLY declares. A primitive running at its own
///   `ParamDef::default` while the binding declares a different number is the
///   normal, intended case (SoftFocus's outer `radius` 6.0 over `Blur.radius`
///   4.0) — the def says nothing there, so there is nothing to shadow.
/// - compared as [`ParamValue`]s through [`ResolvedBinding::write_value`], so
///   the comparison is against what the binding actually writes — reshape,
///   angle wrap and convert included — not against the raw slider float. This is
///   what keeps an authored unit fold off the report: FluidSim2D's `curl`
///   declares 85.0 degrees against `rotation_final.a = 1.4835` radians, one
///   number seen through the fold, not a disagreement.
/// - `Trigger` bindings are skipped: their storage is a monotonic counter, so a
///   def-baked number is never a value anyone meant to hold.
///
/// Groups are flattened first, matching [`apply_inner_param_overrides`], so a
/// group-interface param resolves to its concrete inner node.
pub fn find_shadowed_def_params(
    def: &EffectGraphDef,
    bindings: &[ResolvedBinding],
    graph: &Graph,
) -> Vec<ShadowedDefParam> {
    use crate::node_graph::param_binding::ParamConvert;

    let Ok(flat) = manifold_core::flatten::flatten_groups(def) else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    for binding in bindings {
        let ResolvedTarget::Node { node, param } = &binding.target else {
            continue;
        };
        // A mirrored default never plants, so it can't shadow anything.
        if binding.default_mirrors_node_param || matches!(binding.convert, ParamConvert::Trigger) {
            continue;
        }
        let Some(node_id) = graph.get_node(*node).map(|inst| inst.node_id.clone()) else {
            continue;
        };
        let Some(def_node) = flat.nodes.iter().find(|n| n.node_id == node_id) else {
            continue;
        };
        let Some(def_value) = def_node.params.get(param.as_ref()) else {
            continue;
        };
        let def_value: ParamValue = def_value.clone().into();
        let binding_default = binding.write_value(binding.default_value);
        if param_values_agree(&def_value, &binding_default) {
            continue;
        }
        findings.push(ShadowedDefParam {
            node_id: node_id.as_str().to_string(),
            param: param.as_ref().to_string(),
            def_value,
            binding_id: binding.id.to_string(),
            binding_default,
        });
    }
    findings
}

/// Float params compare with a loose relative tolerance. A def carries
/// hand-written JSON literals and a binding default carries a computed constant
/// — FluidSim2D bakes `rotation_final.a = 1.4835` against a `curl` default of
/// `1.4835298` (85° in radians, truncated by whoever typed the JSON). Those are
/// one number, and reporting them would train readers to ignore the log, which
/// is the only thing this diagnostic has. `1e-4` still separates every real
/// disagreement in the shipped corpus (the closest is 3.0 vs 3.5). Every other
/// variant compares exactly, including a variant mismatch (a def baking a `Vec3`
/// under a `Float` binding is its own bug worth seeing).
fn param_values_agree(def_value: &ParamValue, binding_default: &ParamValue) -> bool {
    match (def_value, binding_default) {
        (ParamValue::Float(a), ParamValue::Float(b)) => {
            (a - b).abs() <= 1e-4 * a.abs().max(b.abs()).max(1.0)
        }
        _ => def_value == binding_default,
    }
}

/// Shipped presets that already bake a card-owned node param, with the value the
/// card overwrites it with. Every entry is dead authoring residue — the def value
/// has never reached evaluation on any build, so the shipped look is the card
/// default in the second column — and deleting it from the JSON is behaviour-
/// preserving. Tracked as its own cleanup (BUG-imsu, shadow-baseline preset cleanup);
/// until then they are listed here so the diagnostic log stays readable in the
/// live rig, and `tests/card_binding_shadow_corpus.rs` fails if a NEW preset
/// joins the list or if an entry here stops reproducing.
///
/// `(preset type id, node id, param)`.
const SHADOW_BASELINE: &[(&str, &str, &str)] = &[
    // The bypass-residue pattern: the def bakes the effect's mix/amount at 0.0
    // (off) while the card ships it at 1.0 (on). The card wins, so these all
    // render fully wet.
    ("ColorGrade", "grade_mix", "amount"),
    ("Dither", "dither", "amount"),
    ("EdgeDetect", "edge_detect", "amount"),
    ("Glitch", "amount_value", "value"),
    ("Strobe", "gate", "amount"),
    ("VoronoiPrism", "composite", "amount"),
    // Baked at the primitive's own default while the card ships a different
    // number: SoftFocus renders radius 6.0, not the def's 4.0.
    ("SoftFocus", "blur", "radius"),
    ("FluidSim2D", "display_tone", "contrast"),
    ("ParticleText", "display_tone", "contrast"),
    ("OilyFluid", "vel_speed_mul", "b"),
];

/// True when this finding is a known shipped disagreement (see
/// [`SHADOW_BASELINE`]). The build paths consult this before recording a
/// diagnostic so the pre-existing corpus doesn't drown the new ones.
pub fn is_baseline_shadow(preset_type_id: &str, finding: &ShadowedDefParam) -> bool {
    SHADOW_BASELINE.iter().any(|(preset, node, param)| {
        *preset == preset_type_id && *node == finding.node_id && *param == finding.param
    })
}

/// [`SHADOW_BASELINE`] as `(preset, node, param)` triples, for the corpus test's
/// stale-entry check.
pub fn shadow_baseline_entries() -> &'static [(&'static str, &'static str, &'static str)] {
    SHADOW_BASELINE
}

/// Map a fused-space shadow finding back to the original def's address.
///
/// On a fused build the def handed to [`find_shadowed_def_params`] is the fused
/// def, so a card-owned param reports as `fused_region_0.n5_amount` — while
/// [`SHADOW_BASELINE`] and anyone reading the log know it as `grade_mix.amount`.
/// The baseline check never matched the fused name, so every fused rebuild of a
/// baselined preset re-logged known dead residue as a fresh `[chain-error]`.
/// Reverse the view's retarget (`(original node, param) → (fused node, field)`)
/// so the baseline lookup and the recorded finding both speak the original
/// names. Segment retarget keys carry the card's `c{i}.` namespace; callers
/// strip that prefix for the baseline lookup exactly as before. No match
/// (unfused build, or a finding on a node that survived fusion) returns the
/// finding unchanged.
pub fn unretarget_shadow(
    finding: &ShadowedDefParam,
    retarget: &AHashMap<(String, String), (NodeId, String)>,
) -> ShadowedDefParam {
    for ((orig_node, orig_param), (fused_id, field)) in retarget {
        if fused_id.as_str() == finding.node_id && *field == finding.param {
            return ShadowedDefParam {
                node_id: orig_node.clone(),
                param: orig_param.clone(),
                ..finding.clone()
            };
        }
    }
    finding.clone()
}

/// Push every inner-node param value declared in `def` into the live `graph`,
/// resolving each def node to its runtime instance through `node_map`. Flattens
/// groups first so a group-interface override routes onto its concrete inner node
/// (group nodes carry no runtime step). Edit-time only — never per frame.
///
/// Standalone (not a `BoundGraph` method) because the effect chain builds the
/// `node_map` from its multi-effect splice while a generator builds one over its
/// whole graph; both feed the same routine. Callers that also drive bindings
/// should use [`BoundGraph::apply_inner_overrides`] so the cache clear comes for
/// free.
///
/// `fused_retarget` maps `(original node_id, param)` → `(fused node_id, field)`
/// for a card that renders fused. A def node that was collapsed into a fused
/// kernel no longer appears in `node_map`; without the retarget the loop would
/// silently `continue` past it, so the edited value never reaches the live kernel
/// (BUG-006 — the def reverts on undo but the fused kernel keeps rendering the old
/// baked value until an unrelated rebuild). On a `node_map` miss each of the
/// node's params is routed through the retarget onto the fused kernel's uniform
/// field — the same repoint the card + user bindings already take. Empty on an
/// unfused graph, so the fast path is a plain per-node `node_map` hit.
///
/// `prefix` translates `def`'s (unprefixed, per-card) node ids into the address
/// space `node_map`/`fused_retarget` are keyed in before every lookup. `""` for
/// a whole-card or whole-generator slot (`node_map` is keyed identically to the
/// def there). For a fused multi-card SEGMENT member, `node_map` and
/// `fused_retarget` are both keyed with the `c{i}.` prefix
/// (`freeze::segment::card_prefix`) because they were built from the
/// concatenated segment def, while the per-card `def` passed in here is the
/// card's own (unprefixed) graph — pass that card's prefix so surviving AND
/// fused-away nodes both resolve (BUG-111).
pub fn apply_inner_param_overrides(
    def: Option<&EffectGraphDef>,
    node_map: &[(NodeId, NodeInstanceId)],
    graph: &mut Graph,
    fused_retarget: &AHashMap<(String, String), (NodeId, String)>,
    prefix: &str,
) {
    let Some(def) = def else { return };
    let Ok(flat) = manifold_core::flatten::flatten_groups(def) else {
        return;
    };
    for node in &flat.nodes {
        let prefixed;
        let lookup_id: &NodeId = if prefix.is_empty() {
            &node.node_id
        } else {
            prefixed = NodeId::new(format!("{prefix}{}", node.node_id.as_str()));
            &prefixed
        };
        if let Some((_, inst)) = node_map.iter().find(|(nid, _)| nid == lookup_id) {
            for (name, value) in &node.params {
                // `set_param` (not unchecked) so a dynamic-port param re-runs the
                // node's `reconfigure` hook, matching a live slider write.
                let _ = graph.set_param(*inst, name, value.clone().into());
            }
        } else if !fused_retarget.is_empty() {
            // Node was fused away: its stable id is gone from the live graph.
            // Route each param onto the fused kernel's uniform field so an inner
            // value edit / undo lands on the running kernel instead of no-op'ing
            // until an unrelated rebuild (BUG-006).
            let src = lookup_id.as_str();
            for (name, value) in &node.params {
                let Some((fused_id, field)) =
                    fused_retarget.get(&(src.to_string(), name.clone()))
                else {
                    continue;
                };
                let Some((_, inst)) = node_map.iter().find(|(nid, _)| nid == fused_id) else {
                    continue;
                };
                let _ = graph.set_param(*inst, field, value.clone().into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::collections::BTreeMap;

    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::{
        EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, ParamSpecDef, SerializedParamValue,
    };
    use manifold_core::params::{Param, ParamManifest};

    use crate::node_graph::param_binding::{BindingSource, ResolvedBinding, ResolvedTarget};
    use crate::node_graph::parameters::ParamValue;
    use crate::node_graph::primitives::AffineTransform;
    use crate::node_graph::{Graph, ParamConvert};

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

    fn scale_of(graph: &Graph, inst: NodeInstanceId) -> ParamValue {
        graph
            .get_node(inst)
            .and_then(|n| n.params.get("scale").cloned())
            .expect("affine exposes `scale`")
    }

    /// A def carrying a single node `"feedback"` whose `scale` param is set to
    /// `value` — the "baked def value" an editor value/position edit pushes.
    fn def_with_scale(value: f32) -> EffectGraphDef {
        let mut params = BTreeMap::new();
        params.insert("scale".to_string(), SerializedParamValue::Float { value });
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![EffectGraphNode {
                id: 0,
                node_id: NodeId::new("feedback"),
                type_id: "node.transform".to_string(),
                handle: Some("feedback".to_string()),
                params,
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: vec![],
        }
    }

    /// The anti-divergence property both runtimes now inherit: pushing a def's
    /// inner-param overrides MUST clear the binding cache, so a bound slider
    /// re-asserts its card value over the baked def value on the next apply.
    /// This is the structural form of the OilyFluid Speed snap-back fix — proved
    /// once on the shared unit instead of twice on each runtime.
    #[test]
    fn apply_inner_overrides_clears_cache_so_bound_value_re_asserts() {
        let mut graph = Graph::new();
        let feedback = graph.add_node_named("feedback", Box::new(AffineTransform::new()));
        graph.set_node_id(feedback, NodeId::new("feedback"));
        let node_map = vec![(NodeId::new("feedback"), feedback)];

        // One binding: outer slot 0 → feedback.scale.
        let binding = ResolvedBinding {
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
            default_mirrors_node_param: false,
        };
        let mut bound = BoundGraph::new(vec![binding], &mut graph, None);

        // Card drives scale to 0.3.
        bound.apply(
            &mut graph,
            &ParamManifest::from_params(vec![slot("amount", 0.3, true)]),
        );
        assert_eq!(scale_of(&graph, feedback), ParamValue::Float(0.3));

        // An editor value/position edit pushes the baked def value (0.9) into
        // every inner node, clobbering the bound scale.
        bound.apply_inner_overrides(&mut graph, &node_map, Some(&def_with_scale(0.9)));
        assert_eq!(
            scale_of(&graph, feedback),
            ParamValue::Float(0.9),
            "override must land the def value first",
        );

        // Re-apply the SAME card value. The cache was cleared by
        // `apply_inner_overrides`, so the binding re-writes 0.3 even though the
        // outer slot didn't move. Without the clear this would skip and the
        // slider would be stuck at 0.9 — the snap-back.
        bound.apply(
            &mut graph,
            &ParamManifest::from_params(vec![slot("amount", 0.3, true)]),
        );
        assert_eq!(
            scale_of(&graph, feedback),
            ParamValue::Float(0.3),
            "bound card value must re-assert over the def override after the \
             cache clear (the structural OilyFluid Speed fix)",
        );
    }

    /// A def node with `node_id` carrying a single `scale` param — stands in for
    /// the unfused editing surface the override reads.
    fn def_scale_named(node_id: &str, value: f32) -> EffectGraphDef {
        let mut d = def_with_scale(value);
        d.nodes[0].node_id = NodeId::new(node_id);
        d.nodes[0].handle = Some(node_id.to_string());
        d
    }

    /// BUG-006: when a card renders fused, its inner nodes are collapsed into one
    /// kernel and drop out of `node_map`. An in-place inner-param override (a value
    /// edit or an undo that bumps `graph_version` without a rebuild) must still
    /// reach the live kernel by routing through the fused view's retarget map —
    /// otherwise the edited def value silently never renders until an unrelated
    /// rebuild bakes it in.
    #[test]
    fn inner_override_routes_fused_away_node_through_retarget() {
        let mut graph = Graph::new();
        // The single fused kernel that replaced the card's atoms. Its uniform
        // field for the collapsed `gain` node's `scale` param is modelled here as
        // the AffineTransform `scale` param (a real fused field is `n{i}_scale`).
        let fused = graph.add_node_named("fused", Box::new(AffineTransform::new()));
        graph.set_node_id(fused, NodeId::new("fused_region_0"));
        // node_map holds ONLY surviving/fused nodes — the collapsed `gain` node is
        // absent, exactly as after `fuse_view_for`.
        let node_map = vec![(NodeId::new("fused_region_0"), fused)];

        // Seed the fused field to a known pre-edit value.
        graph
            .set_param(fused, "scale", ParamValue::Float(0.1))
            .unwrap();

        let def = def_scale_named("gain", 0.9);

        // Without a retarget (the pre-fix / unfused behaviour) the override can't
        // find `gain` in node_map and no-ops — the reproduction of the bug.
        let mut unfused = BoundGraph::new(vec![], &mut graph, None);
        unfused.apply_inner_overrides(&mut graph, &node_map, Some(&def));
        assert_eq!(
            scale_of(&graph, fused),
            ParamValue::Float(0.1),
            "sanity: with no retarget the fused-away override cannot land (the bug)",
        );

        // With the retarget populated (the fused chain path) the override routes
        // `gain.scale` onto the fused kernel's field and the edit lands.
        let mut bound = BoundGraph::new(vec![], &mut graph, None);
        bound.fused_retarget.insert(
            ("gain".to_string(), "scale".to_string()),
            (NodeId::new("fused_region_0"), "scale".to_string()),
        );
        bound.apply_inner_overrides(&mut graph, &node_map, Some(&def));
        assert_eq!(
            scale_of(&graph, fused),
            ParamValue::Float(0.9),
            "fused-away inner-param override must reach the live kernel via the \
             retarget map (BUG-006)",
        );
    }

    /// A binding whose target node is `feedback` and whose declared default is
    /// `default`, matching [`def_with_scale`]'s node.
    ///
    /// AUTHORED (`default_mirrors_node_param: false`), and that is the point of
    /// every test below: an authored default is the one that still plants over a
    /// def-baked value, so it is the only kind that can shadow one. The mirrored
    /// case has its own test.
    fn scale_binding(feedback: NodeInstanceId, default: f32) -> ResolvedBinding {
        ResolvedBinding {
            id: Cow::Borrowed("amount"),
            label: Cow::Borrowed("Amount"),
            default_value: default,
            target: ResolvedTarget::Node {
                node: feedback,
                param: Cow::Borrowed("scale"),
            },
            convert: ParamConvert::Float,
            source: BindingSource::Static,
            source_id: Cow::Borrowed("amount"),
            reshape: None,
            default_mirrors_node_param: false,
            wraps_angle: false,
        }
    }

    /// A graph holding one `feedback` node whose `scale` is already at the def's
    /// baked value — what `into_graph`/`splice_def_into_chain` hand `BoundGraph`.
    fn graph_with_baked_scale(baked: f32) -> (Graph, NodeInstanceId) {
        let mut graph = Graph::new();
        let feedback = graph.add_node_named("feedback", Box::new(AffineTransform::new()));
        graph.set_node_id(feedback, NodeId::new("feedback"));
        graph
            .set_param(feedback, "scale", ParamValue::Float(baked))
            .expect("affine exposes scale");
        (graph, feedback)
    }

    /// BUG-1l7f, the loud half: a value baked onto a card-owned node param is
    /// silently replaced by the card's default at build. The finding names the
    /// node, the param, both values and the card that won — and the graph really
    /// does hold the card's number afterwards, so the report is not theoretical.
    #[test]
    fn def_baked_value_on_a_card_owned_param_is_reported_and_really_overwritten() {
        let (mut graph, feedback) = graph_with_baked_scale(0.9);
        let def = def_with_scale(0.9);
        let bindings = vec![scale_binding(feedback, 0.5)];

        let findings = find_shadowed_def_params(&def, &bindings, &graph);
        assert_eq!(findings.len(), 1, "the one shadowed param must be reported");
        assert_eq!(findings[0].node_id, "feedback");
        assert_eq!(findings[0].param, "scale");
        assert_eq!(findings[0].def_value, ParamValue::Float(0.9));
        assert_eq!(findings[0].binding_id, "amount");
        assert_eq!(findings[0].binding_default, ParamValue::Float(0.5));

        let bound = BoundGraph::new(bindings, &mut graph, Some(&def));
        assert_eq!(
            bound.shadowed_def_params, findings,
            "construction must carry the same findings — the check can't be skipped",
        );
        assert_eq!(
            scale_of(&graph, feedback),
            ParamValue::Float(0.5),
            "and the card default really did overwrite the def value — this is the \
             silent revert the finding is reporting",
        );
    }

    /// The legitimate caller that must NOT be flagged: the card drives an inner
    /// param the def says nothing about, so the primitive's own default differs
    /// from the binding's declared default and the planting is the whole point
    /// (SoftFocus's outer `radius` 6.0 over `Blur.radius` 4.0). No finding, and
    /// the card value still lands.
    #[test]
    fn card_driving_a_param_the_def_does_not_bake_is_not_a_finding() {
        let (mut graph, feedback) = graph_with_baked_scale(4.0);
        let mut def = def_with_scale(0.0);
        def.nodes[0].params.clear(); // the def declares no params at all
        let bindings = vec![scale_binding(feedback, 6.0)];

        assert!(
            find_shadowed_def_params(&def, &bindings, &graph).is_empty(),
            "a primitive sitting at its own default is not a shadowed def value",
        );

        let bound = BoundGraph::new(bindings, &mut graph, Some(&def));
        assert!(bound.shadowed_def_params.is_empty());
        assert_eq!(
            scale_of(&graph, feedback),
            ParamValue::Float(6.0),
            "the card default must still be planted — this path is unchanged",
        );
    }

    /// The BUG-ji6q half: a MIRRORED default is a stamp-time snapshot of the node
    /// param, `apply_binding_defaults` skips planting it, and the def value stands.
    /// Nothing was shadowed, so nothing is reported — the same def+values that fire
    /// under an authored binding must stay silent under a mirrored one, or every
    /// imported scene exposure becomes a false finding.
    #[test]
    fn mirrored_defaults_are_never_reported_because_they_never_plant() {
        let (mut graph, feedback) = graph_with_baked_scale(0.9);
        let def = def_with_scale(0.9);
        let mut binding = scale_binding(feedback, 0.5);
        binding.default_mirrors_node_param = true;

        assert!(
            find_shadowed_def_params(&def, std::slice::from_ref(&binding), &graph).is_empty(),
            "a mirrored default doesn't plant, so it can't shadow the def value",
        );

        let bound = BoundGraph::new(vec![binding], &mut graph, Some(&def));
        assert!(bound.shadowed_def_params.is_empty());
        assert_eq!(
            scale_of(&graph, feedback),
            ParamValue::Float(0.9),
            "and the def value really does stand — the detector agrees with \
             apply_binding_defaults rather than second-guessing it (BUG-ji6q)",
        );
    }

    /// Trigger bindings carry a monotonic counter, not a value anyone holds, so a
    /// def-baked number under one is never the mistake this detector is for.
    #[test]
    fn trigger_bindings_are_never_reported() {
        let (graph, feedback) = graph_with_baked_scale(0.9);
        let def = def_with_scale(0.9);
        let mut binding = scale_binding(feedback, 0.5);
        binding.convert = ParamConvert::Trigger;

        assert!(find_shadowed_def_params(&def, &[binding], &graph).is_empty());
    }
}
