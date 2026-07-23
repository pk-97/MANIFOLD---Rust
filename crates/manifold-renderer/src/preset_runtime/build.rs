//! Generator/effect-chain construction path for [`PresetRuntime`] —
//! the `from_*` constructors and the build-time graph-assembly helpers.
//! Extracted from preset_runtime.rs (Wave 3 P3-R, design D3).

use super::*;

/// Map a [`crate::node_graph::PreAllocationError`] into [`JsonGeneratorLoadError`].
fn generator_error_from_prealloc(
    e: crate::node_graph::PreAllocationError,
) -> JsonGeneratorLoadError {
    use crate::node_graph::PreAllocationError as P;
    match e {
        P::UnsizedArrayOutput { node_type, port, .. } => {
            JsonGeneratorLoadError::UnsizedArrayOutput { node_type, port }
        }
        P::UnsizedTexture3DOutput { node_type, port, .. } => {
            JsonGeneratorLoadError::UnsizedTexture3DOutput { node_type, port }
        }
        P::UnboundArrayResource {
            producer_handle,
            producer_node_type,
            producer_port,
            cause,
        } => JsonGeneratorLoadError::UnboundArrayResource {
            producer_handle,
            producer_node_type,
            producer_port,
            cause,
        },
    }
}

/// Topology hash — captures only the layout-affecting fields of
/// `effects` + `groups`. Per-frame param values, drivers,
/// envelopes, AND continuous wet/dry values are EXCLUDED so live
/// modulation / live wet-dry slider drags don't trigger rebuilds.
///
/// Every enabled group with effects always emits a Mix sub-graph
/// (see `try_build`), so `wet_dry`'s value — discrete OR
/// continuous — never affects topology. The previous design
/// hashed `(wet_dry < 1.0)`; rebuilds across that boundary wiped
/// primitive state (Bloom mip pyramids, Watercolor feedback) every
/// time modulation drove `wet_dry` through 1.0.
///
/// **Skip-on-zero state is layout-affecting.** `try_build` walks
/// active effects and drops any whose `is_skipped_for(view.skip_mode, …, fx)`
/// returns `true`, so flipping that predicate (typically by dragging
/// `amount` off / onto 0) changes which effects appear in the graph.
/// We hash the predicate's current result per effect so the rebuild
/// fires when the user drags `amount` away from 0 — without it the
/// freshly-added effect would never enter the graph until the user
/// toggled `enabled` (which IS in the hash) to force a rebuild.
pub(super) fn compute_topology_hash(
    effects: &[PresetInstance],
    groups: &[EffectGroup],
    width: u32,
    height: u32,
    preview_effect: Option<&EffectId>,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = ahash::AHasher::default();
    for fx in effects {
        fx.id.as_str().hash(&mut h);
        fx.effect_type().as_str().hash(&mut h);
        fx.enabled.hash(&mut h);
        // "3D Shading" (`docs/DEPTH_RELIGHT_DESIGN.md` D8/P7): the toggle and
        // `height_from` change template topology (relight off = no template;
        // height_from changes the height tap), so they stay in the rebuild key.
        // The float D3 knobs are now live uniforms written per-frame, so they
        // must NOT be hashed — otherwise a knob drag would rebuild the chain.
        fx.relight_active().hash(&mut h);
        if fx.relight_active() {
            fx.relight_params.height_from.hash(&mut h);
        }
        // Watched (open-in-editor) target: folded into the rebuild key so
        // opening or closing the editor rebuilds exactly the chain holding this
        // effect, flipping it fused ⇄ unfused (the gate at `should_render_fused`
        // only re-runs on rebuild). Membership-local — `preview_effect` that
        // isn't among these effects hashes `false` for every one, so unrelated
        // chains are untouched and don't churn when the editor opens elsewhere.
        (preview_effect == Some(&fx.id)).hash(&mut h);
        match fx.group_id.as_ref() {
            Some(g) => g.as_str().hash(&mut h),
            None => "".hash(&mut h),
        }
        // Per-card graph divergence — but keyed on the *structure* version,
        // not the snapshot `graph_version`. Only a topology change (node/wire
        // add or remove, full revert) bumps this and forces a rebuild that
        // wipes primitive state. A value-only param edit or a node move bumps
        // only `graph_version` (for the UI snapshot) and is applied in place by
        // `run`'s `apply_inner_param_overrides`, so feedback/sim state survives.
        fx.graph_structure_version.hash(&mut h);
        // Skip-on-zero predicate state — see the doc-comment above.
        // Effects without a `LoadedPresetView` are ignored here
        // (legacy fallback); `try_build` will short-circuit anyway.
        if let Some(view) = loaded_preset_view_by_id(fx.effect_type()) {
            is_skipped_for(view.skip_mode, &view.type_id, fx).hash(&mut h);
        }
    }
    for g in groups {
        g.id.as_str().hash(&mut h);
        g.enabled.hash(&mut h);
    }
    width.hash(&mut h);
    height.hash(&mut h);
    h.finish()
}

/// State tracked for an open partial-wet-dry group during
/// `try_build`'s walk over active effects. Captures the pre-group
/// node + port so the Mix's `a` (dry) input wires from the same
/// source as the group's first effect, and the group's `wet_dry`
/// value so the Mix's `amount` param can be set at build time.
pub(super) struct OpenGroup {
    pub(super) group_id: EffectGroupId,
    pub(super) pre_node: NodeInstanceId,
    pub(super) pre_port: &'static str,
    pub(super) wet_dry: f32,
}

/// Emit the Mix sub-graph for a closing partial-wet-dry group:
/// `dry = pre_group_output`, `wet = last_effect_output`,
/// `out = lerp(dry, wet, wet_dry)`. Returns the Mix node id and
/// its output port (`"out"`).
pub(super) fn close_mix_group(
    graph: &mut Graph,
    closing: &OpenGroup,
    last_effect: (NodeInstanceId, &'static str),
) -> Option<(NodeInstanceId, &'static str)> {
    let mix_id = graph.add_node(Box::new(Mix::new()));
    // Mode = Lerp (0) — matches legacy `WetDryLerpPipeline`'s
    // `lerp(dry, wet, wet_dry)`.
    graph.set_param(mix_id, "mode", ParamValue::Enum(0)).ok()?;
    graph
        .set_param(mix_id, "amount", ParamValue::Float(closing.wet_dry))
        .ok()?;
    // Mix.a = dry (pre-group input). Already wired into the
    // group's first effect via this same output port — output
    // ports can fan out to many input ports, so adding a second
    // consumer is legal.
    graph
        .connect((closing.pre_node, closing.pre_port), (mix_id, "a"))
        .ok()?;
    // Mix.b = wet (post-group result).
    graph.connect(last_effect, (mix_id, "b")).ok()?;
    Some((mix_id, "out"))
}

/// Result of `assign_texture2d_slots`: one physical slot per logical
/// resource (with sharing for non-overlapping lifetimes), plus the
/// dedicated source slot and the total slot count.
pub(super) struct SlotAssignment {
    pub(super) resource_to_slot: AHashMap<ResourceId, Slot>,
    /// Dedicated slot for the upstream input texture. Held across the
    /// frame (the chain `replace_texture_2d`s a clone of the input
    /// into this slot's `RenderTarget` each frame), never recycled
    /// for intermediate writes — sharing would corrupt the upstream
    /// caller's texture when a later effect writes its output.
    pub(super) source_slot: Slot,
    /// Total physical slots needed = slots actually allocated.
    pub(super) slot_count: u32,
    /// Allocation dims per slot, indexed by `Slot.0`. Canvas-sized for the
    /// shared ping-pong slots; dedicated slots for held/persistent
    /// resources take the resource's resolved dims (a 256×1 LUT strip must
    /// not pin a canvas-sized texture for the chain's lifetime).
    pub(super) slot_dims: Vec<(u32, u32)>,
}

/// Walk the plan in topological order, mirroring the executor's
/// acquire/release ordering, to compute the minimum set of physical
/// slots needed for every `Texture2D` resource. The `source_resource`
/// is bound to slot 0 up-front and never returned to the free pool
/// (so other resources can't write through it later).
///
/// Persistent resources — those identified by
/// [`ExecutionPlan::persistent_resources`] as carrying state across
/// frame boundaries — also get dedicated, non-recyclable slots. The
/// per-frame producer write and the per-frame consumer read must
/// land on the SAME physical texture (that's the feedback loop), but
/// that physical texture must not be shared with any other resource
/// whose lifetime overlaps the persistent's full-frame window —
/// otherwise an intermediate write through a recycled slot would
/// clobber the carry-over before the consumer reads it next frame.
/// Pre-allocating dedicated slots is the simplest correctness fix:
/// the slot never enters the free pool, so no other resource can be
/// assigned to it later in the simulator's walk.
///
/// The simulator's slot ids are dense `0..K`. The caller maps them to
/// real backend slots 1:1 via `allocate_slot`.
pub(super) fn assign_texture2d_slots(
    plan: &ExecutionPlan,
    source_resource: ResourceId,
    canvas_dims: (u32, u32),
) -> SlotAssignment {
    let mut resource_to_slot: AHashMap<ResourceId, Slot> = AHashMap::default();
    let source_slot = Slot(0);
    resource_to_slot.insert(source_resource, source_slot);
    let mut next_slot: u32 = 1;
    let mut slot_dims: Vec<(u32, u32)> = vec![canvas_dims];

    // Pre-allocate dedicated slots for every persistent AND held
    // Texture2D resource BEFORE the topological walk. These slots stay
    // out of the free pool for the rest of the simulation. Persistent:
    // the feedback loop's producer/consumer must share the carry-over
    // texture without any intermediate write aliasing it. Held (memo-
    // latched LUTs etc.): the executor serves the latched write on
    // every later frame while upstream transient steps keep re-running
    // — a shared slot would be stomped each frame. Dedicated slots are allocated at the
    // resource's RESOLVED dims, so a 256×1 LUT strip costs 256×1, not
    // a canvas-sized texture.
    let dedicated_set: std::collections::HashSet<ResourceId> = plan
        .persistent_resources()
        .iter()
        .chain(plan.held_resources())
        .filter(|&&res_id| {
            res_id != source_resource
                && plan
                    .resource_type(res_id)
                    .map(|ty| ty.is_texture_2d())
                    .unwrap_or(false)
        })
        .copied()
        .collect();
    for &res_id in &dedicated_set {
        let slot = Slot(next_slot);
        next_slot += 1;
        slot_dims.push(crate::node_graph::execution::resolve_dims(
            plan, res_id, canvas_dims,
        ));
        resource_to_slot.insert(res_id, slot);
    }

    let mut free_pool: Vec<Slot> = Vec::new();

    for step in plan.steps() {
        // Acquire output slots — pop from free pool or grow.
        for &(_, res_id) in &step.outputs {
            if res_id == source_resource {
                continue;
            }
            if dedicated_set.contains(&res_id) {
                // Dedicated slot pre-allocated above. The producer's
                // write goes through `resource_to_slot[res_id]` at
                // runtime; nothing to do in the simulator.
                continue;
            }
            if !plan
                .resource_type(res_id)
                .map(|ty| ty.is_texture_2d())
                .unwrap_or(false)
            {
                continue;
            }
            let slot = free_pool.pop().unwrap_or_else(|| {
                let s = Slot(next_slot);
                next_slot += 1;
                slot_dims.push(canvas_dims);
                s
            });
            resource_to_slot.insert(res_id, slot);
        }
        // Release dead resources — return slots to the free pool.
        for &res_id in &step.free_after {
            if res_id == source_resource {
                // Source slot is dedicated. Never recycled.
                continue;
            }
            if dedicated_set.contains(&res_id) {
                // Dedicated (persistent/held) slots never enter the free
                // pool. (Compile-time invariant: neither kind appears in
                // any step's `free_after` — this guard is defensive.)
                continue;
            }
            if !plan
                .resource_type(res_id)
                .map(|ty| ty.is_texture_2d())
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(&slot) = resource_to_slot.get(&res_id) {
                free_pool.push(slot);
            }
        }
    }

    debug_assert_eq!(slot_dims.len(), next_slot as usize);
    SlotAssignment {
        resource_to_slot,
        source_slot,
        slot_count: next_slot,
        slot_dims,
    }
}

impl PresetRuntime {
    /// Parse a generator-preset JSON string and compile it (mock backend —
    /// fine for unit tests; production uses [`Self::from_json_str_with_device`]).
    pub fn from_json_str(
        json: &str,
        registry: &PrimitiveRegistry,
    ) -> Result<Self, JsonGeneratorLoadError> {
        let doc: EffectGraphDef = serde_json::from_str(json)?;
        // Mock-backend/test convenience path: no per-instance manifest in scope,
        // so the reshape reads the def's own (fresh) `preset_metadata.params`.
        Self::from_def(doc, registry, None)
    }

    /// Build a generator from an already-parsed [`EffectGraphDef`]. Same path
    /// as [`Self::from_json_str`] minus the JSON parse step.
    ///
    /// `manifest` is the live per-instance [`ParamManifest`] (`Layer.gen_params
    /// .params`) when this build is a rebuild of an on-project generator, else
    /// `None` for a standalone build (thumbnails / `check_presets` /
    /// `freeze_profile` / gltf import / freeze proofs). When present, each
    /// param's reshape (min/max/curve/invert) is sourced from the manifest
    /// `spec` — the single authority (D4) — instead of the graph's
    /// `preset_metadata.params` shadow, which is only re-derived from the
    /// manifest at serialize time (D12) and is therefore stale between a
    /// calibration and the next save (BUG-078). `None` keeps reading the
    /// shadow, correct for a fresh-from-disk def whose shadow is accurate.
    pub fn from_def(
        doc: EffectGraphDef,
        registry: &PrimitiveRegistry,
        manifest: Option<&ParamManifest>,
    ) -> Result<Self, JsonGeneratorLoadError> {
        if doc.version > EFFECT_GRAPH_VERSION_WITH_METADATA {
            return Err(JsonGeneratorLoadError::Load(LoadError::UnsupportedVersion {
                found: doc.version,
                max: EFFECT_GRAPH_VERSION_WITH_METADATA,
            }));
        }

        let type_id_str: String = match doc.preset_metadata.as_ref() {
            Some(m) => m.id.as_str().to_string(),
            None => match doc.name.clone() {
                Some(n) => n,
                None => {
                    return Err(JsonGeneratorLoadError::Load(LoadError::InvalidWire {
                        wire_index: 0,
                        reason: "generator preset must declare either a top-level `name` or \
                                 `presetMetadata.id`"
                            .into(),
                    }));
                }
            },
        };
        let type_id = PresetTypeId::from_string(type_id_str);

        // Validate boundary-node presence on the JSON document BEFORE building
        // the runtime graph — `compile()` would fail with a less informative
        // `RequiredInputUnwired` on a missing FinalOutput-source wire.
        if !doc.nodes.iter().any(|n| n.type_id == GENERATOR_INPUT_TYPE_ID) {
            return Err(JsonGeneratorLoadError::MissingGeneratorInput);
        }
        if !doc.nodes.iter().any(|n| n.type_id == FINAL_OUTPUT_TYPE_ID) {
            return Err(JsonGeneratorLoadError::MissingFinalOutput);
        }

        // Capture the binding specs + outer-card param ids before `into_graph`
        // consumes `doc`. The id list resolves each binding's `source_index`
        // (which outer slider it draws from) — keyed by id rather than position
        // so a single slider can fan out to multiple inner-node params.
        let binding_specs: Vec<manifold_core::effect_graph_def::BindingDef> = doc
            .preset_metadata
            .as_ref()
            .map(|m| m.bindings.clone())
            .unwrap_or_default();
        // Per-param slider response (preset curve/invert/range), matching the
        // effect path's `ResolvedBinding::from_static` no-note reshape. Identity
        // for every shipped preset. `param_id -> (min, max, curve, invert)`.
        //
        // Base layer: the graph's `preset_metadata.params` shadow — correct for
        // a standalone build (`manifest = None`), whose def is fresh-from-disk.
        let mut param_reshape: ahash::AHashMap<
            String,
            (f32, f32, manifold_core::macro_bank::MacroCurve, bool),
        > = doc
            .preset_metadata
            .as_ref()
            .map(|m| {
                m.params
                    .iter()
                    .map(|p| (p.id.clone(), (p.min, p.max, p.curve, p.invert)))
                    .collect()
            })
            .unwrap_or_default();
        // BUG-078: the shadow above is derived from the per-instance manifest
        // only at serialize time (D12), so between a calibration and the next
        // save it carries the pre-calibration range. When the live manifest is
        // available (the generator_renderer rebuild path threads it here), its
        // `spec` is the authority for each param's reshape (D4) — overlay it,
        // manifest-wins-per-id, so a post-calibration structural rebuild honors
        // the fresh range/curve/invert. The effect path already does this via
        // `synth_user_binding` reading `self.params`; this is the generator's
        // equivalent for its shared (stock + user) binding path.
        if let Some(manifest) = manifest {
            for p in manifest.iter() {
                param_reshape.insert(
                    p.spec.id.clone(),
                    (p.spec.min, p.spec.max, p.spec.curve, p.spec.invert),
                );
            }
        }
        let string_binding_specs: Vec<manifold_core::effect_graph_def::StringBindingDef> = doc
            .preset_metadata
            .as_ref()
            .map(|m| m.string_bindings.clone())
            .unwrap_or_default();

        // Group → producer map for the node-output preview, captured before
        // `into_graph` flattens the groups away.
        let group_preview_map = manifold_core::flatten::group_output_producer_map(&doc);
        // Flattened once, shared by the node-output preview kind propagation
        // AND the BUG-104 trigger-shadow class check below — both need the
        // group-boundary-free view of the graph.
        let flat_doc = manifold_core::flatten::flatten_groups(&doc).ok();
        let preview_kinds = flat_doc
            .as_ref()
            .map(crate::node_graph::PreviewEncoding::propagate)
            .unwrap_or_default();
        let mut chain_errors: Vec<ChainError> = Vec::new();
        if let Some(flat) = flat_doc.as_ref() {
            for finding in crate::node_graph::trigger_shadow_lint::find_trigger_shadow_findings(flat)
            {
                if crate::node_graph::trigger_shadow_lint::is_allowlisted(
                    type_id.as_str(),
                    &finding.node_id,
                ) {
                    continue;
                }
                record_chain_error(
                    &mut chain_errors,
                    ChainError::TriggerShadowsContinuousBinding {
                        node_id: finding.node_id,
                        port: finding.port,
                        shadowed_source: finding.shadowed_source,
                    },
                );
            }
        }

        let mut graph = doc.into_graph(registry)?;
        let plan = compile(&graph)?;

        // Re-locate the boundary nodes by runtime id now that we have the live
        // graph.
        let generator_input_id = graph
            .nodes()
            .find(|inst| inst.node.type_id().as_str() == GENERATOR_INPUT_TYPE_ID)
            .map(|inst| inst.id)
            .ok_or(JsonGeneratorLoadError::MissingGeneratorInput)?;
        // BUG-125: `.find()` over the graph's unordered node map is only safe
        // when at most one node matches — count first so a second
        // `final_output` is rejected loudly at load instead of one of the
        // two being picked nondeterministically per process.
        let final_output_count = graph
            .nodes()
            .filter(|inst| inst.node.type_id().as_str() == FINAL_OUTPUT_TYPE_ID)
            .count();
        if final_output_count > 1 {
            return Err(JsonGeneratorLoadError::MultipleFinalOutputs {
                count: final_output_count,
            });
        }
        let final_output_id = graph
            .nodes()
            .find(|inst| inst.node.type_id().as_str() == FINAL_OUTPUT_TYPE_ID)
            .map(|inst| inst.id)
            .ok_or(JsonGeneratorLoadError::MissingFinalOutput)?;
        // Walk the plan for the FinalOutput step, pull its `in` input resource —
        // that's what the host pre-binds the target texture to.
        let final_output_input_resource = plan
            .steps()
            .iter()
            .find(|s| s.node == final_output_id)
            .and_then(|s| s.inputs.iter().find(|(n, _)| *n == "in"))
            .map(|(_, res)| *res)
            .ok_or(JsonGeneratorLoadError::MissingFinalOutput)?;

        // Resolve the binding specs against the live graph into the SHARED
        // `ResolvedBinding` type — the same one the effect chain uses — so the
        // per-frame apply runs through `BoundGraph::apply` (skip-on-unchanged
        // cache + structured error logging). Bindings whose node id / param
        // doesn't resolve are warned + dropped.
        use manifold_core::effect_graph_def::BindingTarget;
        let bindings: Vec<ResolvedBinding> = binding_specs
            .iter()
            .filter_map(|b| match &b.target {
                BindingTarget::Node { node_id, param } => {
                    let inst_id = graph.instance_by_node_id(node_id)?;
                    let inst = graph.get_node(inst_id)?;
                    let static_param = inst
                        .node
                        .parameters()
                        .iter()
                        .map(|p| crate::node_graph::intern_name(&p.name))
                        .find(|name| *name == param.as_str())
                        .or_else(|| {
                            log::warn!(
                                "PresetRuntime(gen): binding id `{}` targets node `{node_id}`.`{param}` \
                                 but that param doesn't exist on the node — dropping binding.",
                                b.id,
                            );
                            None
                        })?;
                    let (rmin, rmax, rcurve, rinvert) = param_reshape
                        .get(b.id.as_str())
                        .copied()
                        .unwrap_or((0.0, 1.0, manifold_core::macro_bank::MacroCurve::Linear, false));
                    let reshape = crate::node_graph::Reshape::from_preset_response(
                        rmin, rmax, rcurve, rinvert, b.scale, b.offset,
                    );
                    Some(ResolvedBinding::assemble(
                        std::borrow::Cow::Owned(b.id.clone()),
                        std::borrow::Cow::Owned(b.label.clone()),
                        b.default_value,
                        ResolvedTarget::Node {
                            node: inst_id,
                            param: std::borrow::Cow::Borrowed(static_param),
                        },
                        b.convert,
                        if b.user_added {
                            BindingSource::User
                        } else {
                            BindingSource::Static
                        },
                        std::borrow::Cow::Owned(b.id.clone()),
                        reshape,
                        false,
                    ))
                }
                BindingTarget::Composite { .. } => None,
            })
            .collect();

        // Hand the resolved bindings to the shared `BoundGraph` (seeds the
        // skip-cache + plants each binding's declared default).
        let bound = BoundGraph::new(bindings, &mut graph);
        // Stable NodeId → live instance over the whole graph.
        let node_map: Vec<(NodeId, NodeInstanceId)> = graph
            .nodes()
            .filter(|n| !n.node_id.as_str().is_empty())
            .map(|n| (n.node_id.clone(), n.id))
            .collect();

        let string_bindings: Vec<StringBindingResolution> = string_binding_specs
            .iter()
            .filter_map(|b| match &b.target {
                BindingTarget::Node { node_id, param } => {
                    let inst_id = graph.instance_by_node_id(node_id)?;
                    Some(StringBindingResolution {
                        target_node: inst_id,
                        target_param: param.clone(),
                        source_key: b.id.clone(),
                        default: b.default_value.clone(),
                        def_value: flat_doc
                            .as_ref()
                            .and_then(|flat| def_string_param_value(flat, node_id, param)),
                    })
                }
                BindingTarget::Composite { .. } => None,
            })
            .collect();

        // The generator's single segment. Most `EffectSlot` fields are inert
        // for a generator (it has no chain index, no per-frame user-tail
        // rehydrate — its host rebuilds on structure change); the live ones are
        // `bound`, `node_map`, `generator_input_node`, and the preview maps.
        let segment = EffectSlot {
            effect_id: EffectId::default(),
            effect_type: type_id.clone(),
            legacy_index: 0,
            handles: Vec::new(),
            node_map,
            group_preview_map,
            preview_kinds,
            applied_graph_version: 0,
            bound,
            user_bindings_version: 0,
            // Generators rebuild through their own registry lifecycle, not the
            // chain dispatcher's prior-runtime handoff — no harvest key.
            def_content_key: 0,
            generator_input_node: Some(generator_input_id),
            card_prefix: String::new(),
            relight_writes: Vec::new(),
        };

        let seeded_forced_epoch = graph.forced_outputs_epoch();
        let mut g = Self {
            graph,
            plan,
            last_forced_outputs_epoch: seeded_forced_epoch,
            executor: Executor::with_mock(),
            effect_nodes: vec![segment],
            group_mix_nodes: Vec::new(),
            io: PresetIo::Generate {
                generator_input_id,
                final_output_input_resource,
                final_output_slot: None,
            },
            width: 0,
            height: 0,
            topology_hash: 0,
            built_generation: 0,
            pending_segments: false,
            built_segment_generation: 0,
            state_store: StateStore::new(),
            errors: chain_errors,
            preview_encoding: crate::node_graph::PreviewEncoding::default(),
            type_id: Some(type_id),
            target_format: None,
            string_bindings,
        };
        g.apply_string_defaults();
        Ok(g)
    }

    /// Parse + compile + wire to a real [`MetalBackend`] for production
    /// rendering. Pre-binds a 1×1 placeholder at the FinalOutput-source slot so
    /// per-frame `render()` only swaps the borrowed texture (no hot-path alloc).
    pub fn from_json_str_with_device(
        json: &str,
        registry: &PrimitiveRegistry,
        device: std::sync::Arc<GpuDevice>,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        manifest: Option<&ParamManifest>,
    ) -> Result<Self, JsonGeneratorLoadError> {
        let doc: EffectGraphDef = serde_json::from_str(json)?;
        Self::from_def_with_device(doc, registry, device, width, height, format, manifest)
    }

    /// Same as [`Self::from_json_str_with_device`] but skips the JSON parse.
    /// `manifest` follows the [`Self::from_def`] contract: the live per-instance
    /// [`ParamManifest`] on a project-generator rebuild, `None` standalone.
    pub fn from_def_with_device(
        doc: EffectGraphDef,
        registry: &PrimitiveRegistry,
        device: std::sync::Arc<GpuDevice>,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        manifest: Option<&ParamManifest>,
    ) -> Result<Self, JsonGeneratorLoadError> {
        let mut g = Self::from_def(doc, registry, manifest)?;
        g.width = width;
        g.height = height;
        let mut backend = MetalBackend::new(std::sync::Arc::clone(&device), width, height, format);
        let PresetIo::Generate {
            final_output_input_resource,
            ..
        } = g.io
        else {
            unreachable!("from_def always produces Generate IO");
        };
        // Pre-bind a 1×1 placeholder at the FinalOutput-source slot so the slot
        // exists across frames; `install_target` swaps in the host's real target
        // via `replace_texture_2d` each render call.
        let placeholder =
            RenderTarget::new(&device, 1, 1, format, "preset_runtime_target_owner");
        let slot = backend.pre_bind_texture_2d(final_output_input_resource, placeholder);
        if let PresetIo::Generate {
            final_output_slot, ..
        } = &mut g.io
        {
            *final_output_slot = Some(slot);
        }
        g.target_format = Some(format);

        // Pre-allocate every Array<T> buffer + Texture3D volume the compiled
        // plan declares, then run the post-allocation audit — the same shared
        // pipeline the effect chain uses.
        crate::node_graph::pre_allocate_resources(&g.graph, &g.plan, &device, &mut backend)
            .map_err(generator_error_from_prealloc)?;

        g.executor = Executor::new(Box::new(backend));
        Ok(g)
    }

}
