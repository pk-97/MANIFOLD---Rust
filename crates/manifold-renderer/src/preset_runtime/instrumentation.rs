//! Preview / dump / profiling surface of [`PresetRuntime`] — the
//! churn-quiet editor-facing instrumentation facet. Extracted from
//! preset_runtime.rs (Wave 3 P3-R, design D3).

use super::*;

impl PresetRuntime {
    /// Aim the authoring-time output preview at `node_id` within effect
    /// `effect_id`, or clear it. Resolves the editor's stable [`NodeId`] to
    /// the runtime node via the owning effect's `node_map`. A `None` node id,
    /// or an `effect_id` this chain doesn't hold, clears capture — so a chain
    /// that isn't the watched one contributes no stale preview. Call before
    /// [`Self::run`]; the preserved texture is then read via
    /// [`Self::preview_texture`].
    pub fn set_preview_target(&mut self, effect_id: &EffectId, node_id: Option<&NodeId>) {
        use crate::node_graph::PreviewEncoding;
        // `(target instance, encoding)`, resolved against the owning slot.
        let resolved: Option<(NodeInstanceId, PreviewEncoding)> = node_id.and_then(|nid| {
            self.effect_nodes
                .iter()
                .find(|slot| &slot.effect_id == effect_id)
                .and_then(|slot| {
                    // Direct node hit: prefer the propagated kind (so a blur of
                    // a field reads as a field); fall back to single-node derive.
                    if let Some((_, instance)) =
                        slot.node_map.iter().find(|(mapped, _)| mapped == nid)
                    {
                        let enc = slot
                            .preview_kinds
                            .get(nid)
                            .copied()
                            .unwrap_or_else(|| self.encoding_for_instance(*instance, None));
                        return Some((*instance, enc));
                    }
                    // A selected group container isn't in `node_map` (it
                    // flattened away); resolve it to its primary texture-output
                    // producer. The group's output port name is the strongest
                    // signal (`forceField`); else the producer's propagated kind.
                    slot.group_preview_map
                        .iter()
                        .find(|(group, _, _)| group == nid)
                        .and_then(|(_, producer, port)| {
                            slot.node_map
                                .iter()
                                .find(|(mapped, _)| mapped == producer)
                                .map(|(_, instance)| {
                                    let enc = PreviewEncoding::from_port_name(port)
                                        .or_else(|| slot.preview_kinds.get(producer).copied())
                                        .unwrap_or(PreviewEncoding::Color);
                                    (*instance, enc)
                                })
                        })
                })
        });
        let (target, encoding) = match resolved {
            Some((instance, encoding)) => (Some(instance), encoding),
            None => (None, PreviewEncoding::Color),
        };
        self.preview_encoding = encoding;
        self.executor.set_preview_target(target);
    }

    /// Single-node fallback when no propagated kind is on hand: derive from the
    /// runtime node's `type_id` and first output port off the live graph.
    fn encoding_for_instance(
        &self,
        inst: NodeInstanceId,
        port_override: Option<&str>,
    ) -> crate::node_graph::PreviewEncoding {
        let Some(n) = self.graph.get_node(inst) else {
            return crate::node_graph::PreviewEncoding::Color;
        };
        let port = port_override
            .or_else(|| n.node.outputs().first().map(|p| p.name.as_ref()))
            .unwrap_or("out");
        crate::node_graph::PreviewEncoding::derive(n.node.type_id().as_str(), port)
    }

    /// How the previewed node's output should be rendered (flow wheel / lift /
    /// raw). `Color` when this chain holds no watched node.
    pub fn preview_encoding(&self) -> crate::node_graph::PreviewEncoding {
        self.preview_encoding
    }

    /// Live scalar I/O of the previewed node — for the editor's value inspector
    /// when the watched node has no image output.
    pub fn preview_scalar_io(&self) -> crate::node_graph::PreviewScalarIo {
        (
            self.executor.preview_scalar_inputs().to_vec(),
            self.executor.preview_scalar_outputs().to_vec(),
        )
    }

    /// Live (post-binding-apply, post-modulation) scalar param values for every
    /// node of `effect_id`, keyed by stable [`NodeId`]. Lets the editor canvas
    /// reflect what a card slider / driver / Ableton / envelope is doing to each
    /// inner knob *this frame*, instead of the frozen authoring def that the
    /// structural `from_def` snapshot carries (it only rebuilds on `graph_version`,
    /// so modulation never moved it). Card bindings apply via
    /// [`BoundGraph::apply`](crate::node_graph::BoundGraph) → `graph.set_param`,
    /// which writes the reshaped value straight into the node's param map, so
    /// reading it back here is exactly what the executor just ran with. Empty
    /// when this chain doesn't hold `effect_id`. Cheap: param names are
    /// `&'static`, so only the small `Vec`s allocate per frame.
    ///
    /// A param whose same-named input port carries a connected scalar wire
    /// reads [`Executor::live_scalar_input`](crate::node_graph::Executor::live_scalar_input)
    /// first — the executor's per-frame wire-value tap — falling back to the
    /// param map only when unwired. Mirrors
    /// [`EffectNodeContext::scalar_or_param`](crate::node_graph::effect_node::EffectNodeContext::scalar_or_param)'s
    /// port-shadows-param resolution order, so the editor's live value tap
    /// doesn't freeze on a wire-driven scalar param while the render keeps
    /// moving (PARAM_TWO_WAY_BINDING_DESIGN.md P2 D5).
    pub fn live_node_params(&self, effect_id: &EffectId) -> crate::node_graph::LiveNodeParams {
        let Some(slot) = self.effect_nodes.iter().find(|s| &s.effect_id == effect_id) else {
            return Vec::new();
        };
        slot.node_map
            .iter()
            .filter_map(|(node_id, inst)| {
                let n = self.graph.get_node(*inst)?;
                let values = n
                    .node
                    .parameters()
                    .iter()
                    .map(|pd| {
                        let v = self
                            .executor
                            .live_scalar_input(*inst, pd.name.as_ref())
                            .unwrap_or_else(|| {
                                n.params
                                    .get(pd.name.as_ref())
                                    .map(crate::node_graph::param_default_to_f32)
                                    .unwrap_or_else(|| {
                                        crate::node_graph::param_default_to_f32(&pd.default)
                                    })
                            });
                        (crate::node_graph::intern_name(&pd.name), v)
                    })
                    .collect();
                Some((node_id.clone(), values))
            })
            .collect()
    }

    /// Generator convenience: a generator runtime holds exactly one effect (the
    /// whole generator), so its live params are [`Self::live_node_params`] for
    /// that single slot. Empty for an effect-chain runtime with no slots.
    pub fn live_node_params_watched(&self) -> crate::node_graph::LiveNodeParams {
        match self.effect_nodes.first() {
            Some(slot) => {
                let eid = slot.effect_id.clone();
                self.live_node_params(&eid)
            }
            None => Vec::new(),
        }
    }

    /// Clear any preview capture on this chain. Called each frame for chains
    /// that don't hold the watched effect so a stale target doesn't keep a
    /// texture pinned.
    pub fn clear_preview_target(&mut self) {
        self.executor.set_preview_target(None);
        self.preview_encoding = crate::node_graph::PreviewEncoding::Color;
    }

    /// The preview target's captured output texture from the most recent
    /// [`Self::run`], if this chain holds the watched node and it produced a
    /// texture. `None` otherwise (no target, target pruned, or non-texture
    /// output). See [`Executor::preview_resource`](crate::node_graph::Executor::preview_resource).
    pub fn preview_texture(&self) -> Option<&GpuTexture> {
        let res = self.executor.preview_resource()?;
        let slot = self.executor.backend().slot_for(res)?;
        self.executor.backend().texture_2d(slot)
    }

    /// Enable one-shot "dump every output" mode iff this chain holds
    /// `dump_effect`; otherwise disable it. Call each frame with the requested
    /// effect (or `None`) so only the watched effect's chain pays the cost.
    /// This is the Cmd+D disk dump (whole graph); the editor thumbnail atlas
    /// uses [`Self::set_dump_visible`] instead (only the visible nodes).
    pub fn set_dump(&mut self, dump_effect: Option<&EffectId>) {
        let on =
            dump_effect.is_some_and(|eid| self.effect_nodes.iter().any(|s| &s.effect_id == eid));
        self.executor.set_dump_all(on);
    }

    /// Set (or clear) the continuous thumbnail-atlas dump for this chain —
    /// record only the nodes the editor canvas can currently show, resolved
    /// from their stable [`NodeId`]s to runtime instances via the owning slot's
    /// `node_map`. A `visible` id that names a selected group resolves to its
    /// primary-output producer via `group_preview_map`, mirroring
    /// [`Self::set_preview_target`]. `effect_id` selects the owning effect slot;
    /// pass `None` for a generator runtime (one graph, every slot eligible). A
    /// chain that doesn't hold the requested effect clears its set, so only the
    /// watched chain pays. Hidden / off-scope nodes are simply absent from the
    /// set, so they keep memoization and their textures recycle (sub-changes
    /// A + B).
    pub fn set_dump_visible(&mut self, effect_id: Option<&EffectId>, visible: &[NodeId]) {
        let mut set: ahash::AHashSet<NodeInstanceId> = ahash::AHashSet::new();
        let mut matched = effect_id.is_none();
        for slot in &self.effect_nodes {
            if let Some(eid) = effect_id {
                if &slot.effect_id != eid {
                    continue;
                }
                matched = true;
            }
            for nid in visible {
                if let Some((_, instance)) =
                    slot.node_map.iter().find(|(mapped, _)| mapped == nid)
                {
                    set.insert(*instance);
                } else if let Some((_, producer, _)) =
                    slot.group_preview_map.iter().find(|(group, _, _)| group == nid)
                    && let Some((_, instance)) =
                        slot.node_map.iter().find(|(mapped, _)| mapped == producer)
                {
                    set.insert(*instance);
                }
            }
        }
        self.executor.set_dump_set(if matched { Some(set) } else { None });
    }

    /// Clear any thumbnail-atlas dump set on this chain (atlas off, or this
    /// chain isn't the watched one).
    pub fn clear_dump_set(&mut self) {
        self.executor.set_dump_set(None);
    }

    /// After a `run` with dump mode on, every captured Texture2D output that
    /// belongs to effect `effect_id`, as `(node_id, port, type_id, texture)`.
    /// Filtered to the watched effect's nodes via its `node_map` so the dump is
    /// one effect's pipeline, not the whole spliced chain.
    pub fn dump_textures(
        &self,
        effect_id: &EffectId,
    ) -> Vec<(String, String, String, &GpuTexture)> {
        let Some(slot) = self.effect_nodes.iter().find(|s| &s.effect_id == effect_id) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (node, port, _res, tex) in self.executor.dump_resources() {
            // Only this effect's nodes (reverse-map runtime id → stable NodeId).
            let Some((node_id, _)) = slot.node_map.iter().find(|(_, niid)| niid == node) else {
                continue;
            };
            // Texture pinned at record time, immune to the end-of-frame swap.
            let Some(tex) = tex.as_ref() else {
                continue;
            };
            let type_id = self
                .graph
                .get_node(*node)
                .map(|inst| inst.node.type_id().as_str().to_string())
                .unwrap_or_default();
            out.push((node_id.to_string(), port.to_string(), type_id, tex));
        }
        out
    }

    /// Captured `Array` (storage-buffer) outputs of effect `effect_id` after a
    /// dump `run`, with their channel layout — the array counterpart of
    /// [`Self::dump_textures`].
    pub fn dump_arrays(&self, effect_id: &EffectId) -> Vec<crate::compositor::ArrayDump<'_>> {
        use crate::node_graph::ports::{ChannelElementType, PortType, std430_layout};
        let kind = |t: ChannelElementType| match t {
            ChannelElementType::F32 => "f32",
            ChannelElementType::I32 => "i32",
            ChannelElementType::U32 => "u32",
            ChannelElementType::Vec2F => "vec2f",
            ChannelElementType::Vec3F => "vec3f",
            ChannelElementType::Vec4F => "vec4f",
        };
        let Some(slot) = self.effect_nodes.iter().find(|s| &s.effect_id == effect_id) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for &(node, port, res) in self.executor.dump_array_resources() {
            let Some((node_id, _)) = slot.node_map.iter().find(|(_, niid)| *niid == node) else {
                continue;
            };
            let Some(PortType::Array(at)) = self.plan.resource_type(res) else {
                continue;
            };
            let Some(buffer) = self
                .executor
                .backend()
                .slot_for(res)
                .and_then(|s| self.executor.backend().array_buffer(s))
            else {
                continue;
            };
            let (offsets, _, _) = std430_layout(at.specs);
            let fields = at
                .specs
                .iter()
                .zip(offsets)
                .map(|(spec, off)| {
                    let name = spec
                        .name
                        .debug_name()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("ch@{off}"));
                    (name, kind(spec.ty), off)
                })
                .collect();
            let type_id = self
                .graph
                .get_node(node)
                .map(|inst| inst.node.type_id().as_str().to_string())
                .unwrap_or_default();
            out.push(crate::compositor::ArrayDump {
                name: node_id.to_string(),
                port: port.to_string(),
                type_id,
                buffer,
                item_size: at.item_size,
                fields,
            });
        }
        out
    }

    /// Test-only handle to the executor's backend (post-rebuild canvas-dim
    /// assertions). Not on the hot path.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn backend_for_test(&self) -> &dyn crate::node_graph::Backend {
        self.executor.backend()
    }

    /// Enable/disable one-shot "dump every output" mode on the executor
    /// (preserve every Texture2D output for one frame). Generator path; the
    /// effect chain uses [`Self::set_dump`] (gated by effect id).
    pub fn set_dump_all(&mut self, on: bool) {
        self.executor.set_dump_all(on);
    }

    /// Enable/disable per-step attribution profiling on this chain's executor
    /// (PERF_BUDGET_GATE_DESIGN P2 / D6). Off by default — one branch per
    /// step on the live path.
    pub fn set_profiling(&mut self, on: bool) {
        self.executor.set_profiling(on);
    }

    /// Set this chain's instance identity for profiled tags (D6 correction):
    /// `fx:{layer_id}`, `gen:{layer_id}`, `master`, `led:{...}`. Called by the
    /// owning compositor/generator-renderer at chain-insertion time.
    pub fn set_profile_scope(&mut self, scope: &str) {
        self.executor.set_profile_scope(scope);
    }

    /// Drain this chain's per-step CPU profiles recorded on the last profiled
    /// frame (each entry's `tag` is the scoped GPU-span join key).
    pub fn take_step_profiles(&mut self) -> Vec<crate::node_graph::StepProfile> {
        self.executor.take_step_profiles()
    }

    /// Aim the authoring-time node-output preview at the editor's stable
    /// [`NodeId`](manifold_core::NodeId) within this generator, or clear it. A
    /// selected *group* container resolves to its primary texture-output
    /// producer (groups flatten away, so a direct lookup misses).
    pub fn set_preview_node(&mut self, node_id: Option<&manifold_core::NodeId>) {
        use crate::node_graph::PreviewEncoding;
        let mut encoding = PreviewEncoding::Color;
        // The generator's single segment carries the preview maps.
        let target = node_id.and_then(|nid| {
            // Direct node hit.
            if let Some(inst) = self.graph.instance_by_node_id(nid) {
                encoding = self
                    .effect_nodes
                    .first()
                    .and_then(|s| s.preview_kinds.get(nid).copied())
                    .unwrap_or_else(|| self.encoding_for_instance(inst, None));
                return Some(inst);
            }
            // Group container: capture its producer.
            let seg = self.effect_nodes.first()?;
            if let Some((_, producer, port)) =
                seg.group_preview_map.iter().find(|(group, _, _)| group == nid)
                && let Some(inst) = self.graph.instance_by_node_id(producer)
            {
                encoding = PreviewEncoding::from_port_name(port)
                    .or_else(|| seg.preview_kinds.get(producer).copied())
                    .unwrap_or(PreviewEncoding::Color);
                return Some(inst);
            }
            None
        });
        self.preview_encoding = encoding;
        self.executor.set_preview_target(target);
    }

    /// After a `render` with dump mode on, every captured Texture2D output as
    /// `(node_id, port, type_id, texture)` — the generator's whole pipeline (no
    /// per-effect filter, unlike the chain's [`Self::dump_textures`]).
    pub fn dump_textures_all(&self) -> Vec<(String, String, String, &GpuTexture)> {
        let mut out = Vec::new();
        for (node, port, _res, tex) in self.executor.dump_resources() {
            // Texture pinned at record time, immune to the end-of-frame swap.
            let Some(tex) = tex.as_ref() else {
                continue;
            };
            let (name, type_id) = self
                .graph
                .get_node(*node)
                .map(|inst| {
                    (
                        inst.node_id.to_string(),
                        inst.node.type_id().as_str().to_string(),
                    )
                })
                .unwrap_or_default();
            out.push((name, port.to_string(), type_id, tex));
        }
        out
    }

    /// Whole-graph `Array` dump (generator path) — the array counterpart of
    /// [`Self::dump_textures_all`].
    pub fn dump_arrays_all(&self) -> Vec<crate::compositor::ArrayDump<'_>> {
        use crate::node_graph::ports::{ChannelElementType, PortType, std430_layout};
        let kind = |t: ChannelElementType| match t {
            ChannelElementType::F32 => "f32",
            ChannelElementType::I32 => "i32",
            ChannelElementType::U32 => "u32",
            ChannelElementType::Vec2F => "vec2f",
            ChannelElementType::Vec3F => "vec3f",
            ChannelElementType::Vec4F => "vec4f",
        };
        let mut out = Vec::new();
        for &(node, port, res) in self.executor.dump_array_resources() {
            let Some(PortType::Array(at)) = self.plan.resource_type(res) else {
                continue;
            };
            let Some(buffer) = self
                .executor
                .backend()
                .slot_for(res)
                .and_then(|s| self.executor.backend().array_buffer(s))
            else {
                continue;
            };
            let (offsets, _, _) = std430_layout(at.specs);
            let fields = at
                .specs
                .iter()
                .zip(offsets)
                .map(|(spec, off)| {
                    let name = spec
                        .name
                        .debug_name()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("ch@{off}"));
                    (name, kind(spec.ty), off)
                })
                .collect();
            let (name, type_id) = self
                .graph
                .get_node(node)
                .map(|inst| {
                    (
                        inst.node_id.to_string(),
                        inst.node.type_id().as_str().to_string(),
                    )
                })
                .unwrap_or_default();
            out.push(crate::compositor::ArrayDump {
                name,
                port: port.to_string(),
                type_id,
                buffer,
                item_size: at.item_size,
                fields,
            });
        }
        out
    }

}
