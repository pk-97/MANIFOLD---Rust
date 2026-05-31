//! Loader shim that turns a JSON-defined generator graph into a
//! runtime-executable bundle implementing the standard [`Generator`]
//! trait.
//!
//! A generator preset JSON sits at
//! `crates/manifold-renderer/assets/generator-presets/*.json` and uses
//! the same [`EffectGraphDef`] schema as effect presets. The
//! distinguishing convention is the boundary nodes:
//!
//! - Effect graphs start with `system.source` (texture in).
//! - Generator graphs start with `system.generator_input` (timing
//!   scalars in) and end with `system.final_output` (texture out).
//!
//! `JsonGraphGenerator` owns the compiled `Graph`, the `ExecutionPlan`,
//! and an `Executor` with a real [`MetalBackend`]. At each frame
//! [`Generator::render`] updates the GeneratorInput's params, installs
//! the host-provided target texture as the FinalOutput's source slot,
//! then runs the graph against the host's `GpuEncoder`.

use manifold_core::{
    Beats, GeneratorTypeId, Seconds,
    effect_graph_def::{EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef},
};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};

use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::{
    BindingSource, EffectGraphDefExt, ExecutionPlan, Executor, FINAL_OUTPUT_TYPE_ID, FrameTime,
    GENERATOR_INPUT_TYPE_ID, Graph, GraphError, LastAppliedCache, LoadError, MetalBackend,
    NodeInstanceId, ParamValue, PrimitiveRegistry, ResolvedBinding, ResolvedTarget, ResourceId,
    Slot, StateStore, apply_binding_defaults, apply_bindings, compile,
};
use crate::render_target::RenderTarget;
use manifold_core::effects::ParamSlot;

/// Errors produced when loading a generator preset.
#[derive(Debug)]
pub enum JsonGeneratorLoadError {
    /// JSON parsing failed.
    Json(serde_json::Error),
    /// The schema document failed to construct a Graph.
    Load(LoadError),
    /// The compiled graph had a static error (cycle, type mismatch, …).
    Compile(GraphError),
    /// The preset's JSON contains no `system.generator_input` node.
    /// Generator graphs must include one — it's how time/beat/aspect
    /// reach the primitives.
    MissingGeneratorInput,
    /// The preset's JSON contains no `system.final_output` node, or it
    /// isn't wired. Without it the graph has no terminal sink and the
    /// target texture has nowhere to land.
    MissingFinalOutput,
    /// A primitive declared an `Array<T>` output but
    /// `EffectNode::array_output_capacity` returned `None` for that
    /// port. Loud fail instead of silently leaving the buffer
    /// unallocated — downstream consumers would otherwise read an
    /// empty wire and produce nothing (the silent-black-output bug
    /// class). Fix by adding `max_capacity` to the producing
    /// primitive's params, or by overriding `array_output_capacity`
    /// to derive size from a sibling forward-dependency input.
    UnsizedArrayOutput {
        node_type: String,
        port: String,
    },
    /// Sibling of `UnsizedArrayOutput` for Texture3D — a primitive
    /// declared a Texture3D output but
    /// [`EffectNode::texture_3d_output_dims`] returned `None` for that
    /// port. Pre-binding Texture3D resources is a hard contract (no
    /// lazy-alloc), so a missing sizing implementation can't go silent.
    UnsizedTexture3DOutput {
        node_type: String,
        port: String,
    },
    /// Post-allocation catch-all: walking every `Array<T>` resource
    /// in the compiled plan, this one has no bound slot or no
    /// underlying buffer. The cause-layer errors above (e.g.
    /// `UnsizedArrayOutput`) catch the specific reasons we've
    /// enumerated; this audit catches anything we haven't — alias
    /// chain breaks, canvas-dim-zero skips, future allocation
    /// branches that fail silently. The architectural invariant is
    /// that `compile()` either returns a plan with every resource
    /// bound, or returns `Err`. No third state. Cause messages above
    /// explain WHY; this one guarantees COMPLETENESS.
    UnboundArrayResource {
        producer_handle: Option<String>,
        producer_node_type: String,
        producer_port: String,
        cause: &'static str,
    },
}

impl std::fmt::Display for JsonGeneratorLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(e) => write!(f, "JSON parse error: {e}"),
            Self::Load(e) => write!(f, "graph load error: {e}"),
            Self::Compile(e) => write!(f, "graph compile error: {e:?}"),
            Self::MissingGeneratorInput => write!(
                f,
                "preset has no `{GENERATOR_INPUT_TYPE_ID}` node — required for generator graphs"
            ),
            Self::MissingFinalOutput => write!(
                f,
                "preset has no `{FINAL_OUTPUT_TYPE_ID}` node, or it is not wired"
            ),
            Self::UnsizedArrayOutput { node_type, port } => write!(
                f,
                "primitive `{node_type}` Array<T> output port `{port}` has no \
                 concrete size — `array_output_capacity` returned None. \
                 Add a `max_capacity` param, or override the method to derive \
                 size from a forward-dep input (not a state-capture port)."
            ),
            Self::UnsizedTexture3DOutput { node_type, port } => write!(
                f,
                "primitive `{node_type}` Texture3D output port `{port}` has no \
                 concrete dims — `texture_3d_output_dims` returned None. \
                 Add `vol_res` / `vol_depth` params, or override the method to \
                 derive dims from a forward-dep input."
            ),
            Self::UnboundArrayResource {
                producer_handle,
                producer_node_type,
                producer_port,
                cause,
            } => {
                let handle_part = match producer_handle {
                    Some(h) => format!(" (handle `{h}`)"),
                    None => String::new(),
                };
                write!(
                    f,
                    "Array<T> output of `{producer_node_type}.{producer_port}`{handle_part} \
                     has no bound buffer after chain build: {cause}. \
                     This is the post-allocation audit catching a wire \
                     whose source resource was never pre-bound — downstream \
                     consumers would read an empty buffer and render silently \
                     wrong. The cause-layer error printed above (if any) \
                     explains the specific failure; this audit guarantees \
                     no plan with dangling resources reaches the executor."
                )
            }
        }
    }
}

impl std::error::Error for JsonGeneratorLoadError {}

impl From<serde_json::Error> for JsonGeneratorLoadError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<LoadError> for JsonGeneratorLoadError {
    fn from(e: LoadError) -> Self {
        Self::Load(e)
    }
}

impl From<GraphError> for JsonGeneratorLoadError {
    fn from(e: GraphError) -> Self {
        Self::Compile(e)
    }
}

/// A JSON-defined generator graph compiled and ready to execute.
///
/// Construction is one-shot: parse JSON, build Graph, compile plan,
/// locate the boundary nodes. Per-frame work is then minimal — set
/// frame context, install the target texture, run the executor.
pub struct JsonGraphGenerator {
    type_id: GeneratorTypeId,
    pub graph: Graph,
    pub plan: ExecutionPlan,
    executor: Executor,
    generator_input_id: NodeInstanceId,
    /// Runtime id of the `system.final_output` node. Used by hosts that
    /// need to pre-bind a target texture to its input resource.
    pub final_output_id: NodeInstanceId,
    /// Resource id that feeds `final_output.in` — the host pre-binds
    /// the target texture here.
    pub final_output_input_resource: ResourceId,
    /// Slot that holds the FinalOutput's source texture. `Some` for
    /// production-path generators (constructed via
    /// `from_json_str_with_device`), where construction-time pre-bind
    /// reserves the slot with a placeholder. `None` for the
    /// mock-backend test path, where there's no GPU and the slot is
    /// never used.
    final_output_slot: Option<Slot>,
    /// Texture format threaded through to placeholder allocation on
    /// resize. `None` for the mock-backend test path (which doesn't
    /// touch the GPU on resize).
    target_format: Option<GpuTextureFormat>,
    /// Outer-card → inner-node bindings resolved at construction time,
    /// using the SAME [`ResolvedBinding`] type, [`apply_bindings`] loop,
    /// and [`LastAppliedCache`] the effect chain uses. Each render frame
    /// walks these via `apply_bindings`, which pushes the corresponding
    /// `GeneratorContext::params[i]` into the target node's param,
    /// skipping writes whose outer value hasn't changed (so per-card
    /// inner edits survive at-rest sliders — the same property effects
    /// have) and logging structured errors on routing failures instead
    /// of silently dropping. Empty when the preset declares no bindings.
    bindings: Vec<ResolvedBinding>,
    /// Skip-on-unchanged cache parallel to `bindings`. Seeded with each
    /// binding's declared default at construction so a freshly-built
    /// generator only writes outer→inner for slots that already diverge
    /// from their default.
    binding_cache: LastAppliedCache,
    /// String-typed outer-card → inner-node bindings. Each entry routes
    /// one entry from the host's `clip.string_params` map (looked up by
    /// the binding's `source_key`) into the target node's String param.
    /// Falls back to `default` when the host map is empty or doesn't
    /// contain the key. Empty when the preset declares no
    /// string-bindings.
    string_bindings: Vec<StringBindingResolution>,
    /// Per-instance persistent state for stateful primitives in the
    /// graph (Feedback prev-frame textures, ArrayFeedback prev-frame
    /// buffers, EnvelopeFollower smoothed values, etc.). The runtime
    /// keys state by `(NodeInstanceId, OwnerKey)`; for generators we
    /// use a single `owner_key = 0` because the `JsonGraphGenerator`
    /// instance is itself per-layer and gets dropped on generator
    /// rebuild, so the NodeInstanceId alone uniquely identifies a
    /// primitive's state.
    ///
    /// State lifecycle:
    ///   - Allocated lazily by primitives on first frame.
    ///   - Wiped by [`Self::reset_state`] (export warmup re-seek).
    ///   - Dropped entirely on generator rebuild (`Self::from_def`).
    state_store: StateStore,
}

/// One resolved String outer-card → inner-node binding. The String
/// binding path stays bespoke (the shared `apply_bindings` loop is
/// float-only); source is keyed by name
/// (lookup into the host's `clip.string_params` map), no convert
/// because String → String is a pass-through.
struct StringBindingResolution {
    target_node: NodeInstanceId,
    target_param: String,
    /// Key into the host's `clip.string_params` map. The presetMetadata
    /// `stringBindings` `id` field — same identity as the matching
    /// `stringParams` entry's `id`.
    source_key: String,
    default: String,
}

impl JsonGraphGenerator {
    /// Parse a generator-preset JSON string and compile it. The
    /// resulting struct owns the runtime graph and is ready to execute
    /// (modulo wiring an executor + backend).
    pub fn from_json_str(
        json: &str,
        registry: &PrimitiveRegistry,
    ) -> Result<Self, JsonGeneratorLoadError> {
        let doc: EffectGraphDef = serde_json::from_str(json)?;
        Self::from_def(doc, registry)
    }

    /// Build a generator from an already-parsed `EffectGraphDef`. Same
    /// path as [`Self::from_json_str`] minus the JSON parse step. Used
    /// when a per-layer override needs to drive rendering without
    /// round-tripping through serde — see `GeneratorRegistry::create_with_override`.
    pub fn from_def(
        doc: EffectGraphDef,
        registry: &PrimitiveRegistry,
    ) -> Result<Self, JsonGeneratorLoadError> {
        if doc.version > EFFECT_GRAPH_VERSION_WITH_METADATA {
            return Err(JsonGeneratorLoadError::Load(LoadError::UnsupportedVersion {
                found: doc.version,
                max: EFFECT_GRAPH_VERSION_WITH_METADATA,
            }));
        }

        // Pull a type_id from the preset metadata if present, otherwise
        // fall back to the document `name`. Both are stable strings the
        // GeneratorRegistry can key by. Clone out to an owned String so
        // we can pass `doc` by value into `into_graph` below.
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
        let type_id = GeneratorTypeId::from_string(type_id_str);

        // Validate boundary-node presence on the JSON document BEFORE
        // building the runtime graph — `compile()` would fail with
        // `RequiredInputUnwired` on a missing FinalOutput-source wire,
        // surfacing a less informative error than the explicit
        // boundary-node check.
        if !doc
            .nodes
            .iter()
            .any(|n| n.type_id == GENERATOR_INPUT_TYPE_ID)
        {
            return Err(JsonGeneratorLoadError::MissingGeneratorInput);
        }
        if !doc
            .nodes
            .iter()
            .any(|n| n.type_id == FINAL_OUTPUT_TYPE_ID)
        {
            return Err(JsonGeneratorLoadError::MissingFinalOutput);
        }

        // Capture the binding specs + outer-card param ids before
        // `into_graph` consumes `doc`. The id list resolves each
        // binding's `source_index` (which outer slider it draws from)
        // below — keyed by id rather than position so a single slider
        // can fan out to multiple inner-node params.
        let binding_specs: Vec<manifold_core::effect_graph_def::BindingDef> = doc
            .preset_metadata
            .as_ref()
            .map(|m| m.bindings.clone())
            .unwrap_or_default();
        let outer_param_index: ahash::AHashMap<String, usize> = doc
            .preset_metadata
            .as_ref()
            .map(|m| {
                m.params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (p.id.clone(), i))
                    .collect()
            })
            .unwrap_or_default();
        let string_binding_specs: Vec<manifold_core::effect_graph_def::StringBindingDef> = doc
            .preset_metadata
            .as_ref()
            .map(|m| m.string_bindings.clone())
            .unwrap_or_default();

        let mut graph = doc.into_graph(registry)?;
        let plan = compile(&graph)?;

        // Re-locate the boundary nodes by runtime id now that we have
        // the live graph.
        let generator_input_id = graph
            .nodes()
            .find(|inst| inst.node.type_id().as_str() == GENERATOR_INPUT_TYPE_ID)
            .map(|inst| inst.id)
            .ok_or(JsonGeneratorLoadError::MissingGeneratorInput)?;

        let final_output_id = graph
            .nodes()
            .find(|inst| inst.node.type_id().as_str() == FINAL_OUTPUT_TYPE_ID)
            .map(|inst| inst.id)
            .ok_or(JsonGeneratorLoadError::MissingFinalOutput)?;

        // Walk the plan for the FinalOutput step, pull its `in` input
        // resource — that's what the host pre-binds the target texture
        // to.
        let final_output_input_resource = plan
            .steps()
            .iter()
            .find(|s| s.node == final_output_id)
            .and_then(|s| s.inputs.iter().find(|(n, _)| *n == "in"))
            .map(|(_, res)| *res)
            .ok_or(JsonGeneratorLoadError::MissingFinalOutput)?;

        // Resolve the captured binding specs against the live graph's
        // handle map into the SHARED `ResolvedBinding` type — the same
        // one the effect chain uses — so the per-frame apply runs through
        // `apply_bindings` (skip-on-unchanged cache + structured error
        // logging) instead of a bespoke generator-only loop. Bindings
        // whose handle / param doesn't resolve are warned + dropped.
        use manifold_core::effect_graph_def::BindingTarget;
        let handle_map: ahash::AHashMap<&str, NodeInstanceId> =
            graph.handles().collect();
        let bindings: Vec<ResolvedBinding> = binding_specs
            .iter()
            .filter_map(|b| match &b.target {
                BindingTarget::HandleNode { handle, param } => {
                    let node_id = *handle_map.get(handle.as_str())?;
                    let source_index = match outer_param_index.get(b.id.as_str()) {
                        Some(idx) => *idx,
                        None => {
                            log::warn!(
                                "JsonGraphGenerator: binding id `{}` (target \
                                 `{handle}.{param}`) has no matching outer-card param \
                                 — the binding will always emit its default ({}). \
                                 Add a `params` entry with id=`{}` or remove the binding.",
                                b.id, b.default_value, b.id,
                            );
                            return None;
                        }
                    };
                    // Pull the canonical `&'static str` param name off the
                    // target node's `ParamDef` list (same trick as
                    // `ResolvedBinding::from_user`) so the resolved target
                    // carries a stable name with no per-binding leak.
                    let inst = graph.get_node(node_id)?;
                    let static_param = inst
                        .node
                        .parameters()
                        .iter()
                        .map(|p| p.name)
                        .find(|name| *name == param.as_str())
                        .or_else(|| {
                            log::warn!(
                                "JsonGraphGenerator: binding id `{}` targets \
                                 `{handle}.{param}` but that param doesn't exist on the \
                                 node — dropping binding.",
                                b.id,
                            );
                            None
                        })?;
                    Some(ResolvedBinding {
                        id: std::borrow::Cow::Owned(b.id.clone()),
                        label: std::borrow::Cow::Owned(b.label.clone()),
                        default_value: b.default_value,
                        target: ResolvedTarget::Node {
                            node: node_id,
                            param: static_param,
                        },
                        convert: b.convert,
                        source: if b.user_added {
                            BindingSource::User
                        } else {
                            BindingSource::Static
                        },
                        source_index,
                        // Generator bindings don't carry a card reshape or
                        // angle-loop yet (matches the effect-side default for
                        // non-Angle params; generator-side angle wrap is a
                        // future follow-up if a looping generator knob needs it).
                        reshape: None,
                        wraps_angle: false,
                    })
                }
                BindingTarget::Composite { .. } => None,
            })
            .collect();

        // Seed the skip-on-unchanged cache + plant each binding's declared
        // default into its inner-node target now — mirrors the effect
        // chain's chain-build seed (closes the "inner node starts at the
        // primitive default instead of the binding default" gap that the
        // bespoke generator path previously had).
        let mut binding_cache = LastAppliedCache::new();
        binding_cache.seed_from_bindings(&bindings);
        apply_binding_defaults(&bindings, &mut graph, None);

        let string_bindings: Vec<StringBindingResolution> = string_binding_specs
            .iter()
            .filter_map(|b| match &b.target {
                BindingTarget::HandleNode { handle, param } => {
                    let node_id = *handle_map.get(handle.as_str())?;
                    Some(StringBindingResolution {
                        target_node: node_id,
                        target_param: param.clone(),
                        source_key: b.id.clone(),
                        default: b.default_value.clone(),
                    })
                }
                BindingTarget::Composite { .. } => None,
            })
            .collect();

        // Apply default values immediately so the inner-node String
        // params don't sit at their primitive-declared defaults until
        // the first `set_string_params` call. The host calls
        // `set_string_params` once per frame, but for code paths that
        // read inner state before the first frame (parity tests, the
        // editor inspector, etc.) the binding-default should already
        // be live.
        let mut g = Self {
            type_id,
            graph,
            plan,
            executor: Executor::with_mock(),
            generator_input_id,
            final_output_id,
            final_output_input_resource,
            final_output_slot: None,
            target_format: None,
            bindings,
            binding_cache,
            string_bindings,
            state_store: StateStore::new(),
        };
        g.apply_string_defaults();
        Ok(g)
    }

    /// Parse + compile + wire to a real [`MetalBackend`] for production
    /// rendering. Same as [`Self::from_json_str`] but the executor uses
    /// a `MetalBackend` allocated against `device` at the given render
    /// resolution + format. Pre-binds a 1×1 placeholder RenderTarget
    /// at the FinalOutput-source slot so per-frame `render()` only has
    /// to swap the borrowed texture (no allocation on the hot path).
    /// The resulting generator implements [`Generator`].
    ///
    /// Safety: the `&GpuDevice` is cached internally as a raw pointer
    /// (mirroring the `GeneratorRenderer::device_ptr` pattern). The
    /// caller must keep `device` alive for the returned generator's
    /// lifetime — in production that's the content-thread-owned
    /// `Option<GpuDevice>` field on `ContentPipeline`, which exists
    /// for the program's lifetime.
    pub fn from_json_str_with_device(
        json: &str,
        registry: &PrimitiveRegistry,
        device: &GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
    ) -> Result<Self, JsonGeneratorLoadError> {
        let doc: EffectGraphDef = serde_json::from_str(json)?;
        Self::from_def_with_device(doc, registry, device, width, height, format)
    }

    /// Same as [`Self::from_json_str_with_device`] but skips the JSON
    /// parse step — used by `GeneratorRegistry::create_with_override`
    /// when a per-layer override `EffectGraphDef` should drive
    /// rendering instead of the bundled preset JSON.
    pub fn from_def_with_device(
        doc: EffectGraphDef,
        registry: &PrimitiveRegistry,
        device: &GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
    ) -> Result<Self, JsonGeneratorLoadError> {
        let mut g = Self::from_def(doc, registry)?;
        let mut backend = MetalBackend::new(device, width, height, format);
        // Pre-bind a 1×1 placeholder at the FinalOutput-source slot so
        // the slot exists across frames; `install_target` swaps in the
        // host's real target via `replace_texture_2d` each render call.
        let placeholder = RenderTarget::new(
            device,
            1,
            1,
            format,
            "json_graph_generator_target_owner",
        );
        let slot = backend.pre_bind_texture_2d(g.final_output_input_resource, placeholder);
        g.final_output_slot = Some(slot);
        g.target_format = Some(format);

        // Pre-allocate every Array<T> buffer + Texture3D volume the
        // compiled plan declares, then run the post-allocation audit.
        // All three steps run inside `graph_loader::pre_allocate_resources`
        // — the canonical pipeline that both the generator path (here)
        // and the effect chain path (`ChainGraph::try_build`) share, so
        // any feature added to one applies to the other automatically.
        // See `crates/manifold-renderer/src/node_graph/graph_loader.rs`.
        crate::node_graph::pre_allocate_resources(&g.graph, &g.plan, device, &mut backend)
            .map_err(generator_error_from_prealloc)?;

        g.executor = Executor::new(Box::new(backend));
        Ok(g)
    }

    /// Stable identity for the GeneratorRegistry.
    pub fn type_id(&self) -> &GeneratorTypeId {
        &self.type_id
    }

    /// Test-only handle to the executor's backend. Used by
    /// `generator_renderer`'s regression tests to assert post-rebuild
    /// canvas dims match the host. Not on the hot path.
    #[cfg(test)]
    pub(crate) fn backend_for_test(&self) -> &dyn crate::node_graph::Backend {
        self.executor.backend()
    }

    /// Replace the internal executor — used when the host wires a real
    /// `MetalBackend` (the default executor uses `MockBackend` which is
    /// fine for tests but won't allocate real GPU textures).
    pub fn set_executor(&mut self, executor: Executor) {
        self.executor = executor;
    }

    /// Install the host-provided target texture as the source for
    /// `final_output.in` via `replace_texture_2d` — a single atomic
    /// retain on the host's `MTLTexture`, no allocation. The slot was
    /// pre-bound to a placeholder at construction time
    /// (`from_json_str_with_device`); this just swaps the borrowed
    /// view each frame.
    ///
    /// Panics if the backend isn't a `MetalBackend` (mock-backend mode
    /// never reaches this — `Generator::render` is the only caller and
    /// the registry only hands out mock-backed instances inside tests).
    fn install_target(&mut self, target: &GpuTexture) {
        let metal = self
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<MetalBackend>())
            .expect(
                "JsonGraphGenerator::install_target requires a MetalBackend — use \
                 from_json_str_with_device to construct the production path",
            );
        let slot = self.final_output_slot.expect(
            "JsonGraphGenerator final_output_slot must be pre-bound — \
             construct via from_json_str_with_device",
        );
        metal.replace_texture_2d(slot, target.clone());
    }

    /// Update the `system.generator_input` node's per-frame context by
    /// setting its float params (`time`, `beat`, `aspect`,
    /// `trigger_count`, `anim_progress`, `output_width`,
    /// `output_height`). Called by the host once per frame before
    /// [`Self::execute_frame`]. Uses the standard `Graph::set_param`
    /// path — no downcasting, no special-case plumbing.
    #[allow(clippy::too_many_arguments)]
    pub fn set_frame_context(
        &mut self,
        time: f32,
        beat: f32,
        aspect: f32,
        trigger_count: f32,
        anim_progress: f32,
        output_width: f32,
        output_height: f32,
    ) {
        let id = self.generator_input_id;
        let _ = self.graph.set_param(id, "time", ParamValue::Float(time));
        let _ = self.graph.set_param(id, "beat", ParamValue::Float(beat));
        let _ = self.graph.set_param(id, "aspect", ParamValue::Float(aspect));
        let _ = self.graph.set_param(
            id,
            "trigger_count",
            ParamValue::Float(trigger_count),
        );
        let _ = self.graph.set_param(
            id,
            "anim_progress",
            ParamValue::Float(anim_progress),
        );
        let _ = self.graph.set_param(
            id,
            "output_width",
            ParamValue::Float(output_width),
        );
        let _ = self.graph.set_param(
            id,
            "output_height",
            ParamValue::Float(output_height),
        );
    }

    /// Push the host's slider values through the preset's bindings to
    /// the matching inner-node params. Each binding reads from the
    /// outer-card slider whose `id` it was declared with — resolved to
    /// a `source_index` into `values` at construction time. Two
    /// bindings sharing an `id` (one slider fanning out to multiple
    /// inner-node params) both pick up the same `values[source_index]`,
    /// not just the first one. Missing entries fall back to the
    /// binding's `default_value`. No-op if the preset declared no
    /// bindings.
    pub fn apply_param_values(&mut self, values: &[f32]) {
        // Route through the shared `apply_bindings` loop. It indexes
        // `values[binding.source_index]` (so a single outer slider can
        // fan out to multiple inner params), falls back to each binding's
        // `default_value` when the slot is missing, skips writes whose
        // outer value is unchanged since last frame (per-card inner edits
        // survive at-rest sliders), and logs structured errors on routing
        // failure. `apply_bindings` takes `&[ParamSlot]`; wrap the host's
        // float bus into exposed slots (the exposure flag is irrelevant to
        // the apply — only `.value` is read). The wrap is a small
        // stack-ish Vec (≤ MAX_GEN_PARAMS); the FrameContext work folds it
        // away later.
        let slots: Vec<ParamSlot> = values.iter().map(|v| ParamSlot::exposed(*v)).collect();
        apply_bindings(
            &self.bindings,
            &mut self.graph,
            None,
            &slots,
            &mut self.binding_cache,
        );
    }

    /// Push the host's per-clip string overrides through the preset's
    /// `stringBindings` to the matching inner-node String params. Keys
    /// absent from `values` fall back to the binding's declared
    /// default. No-op if the preset declared no string-bindings.
    ///
    /// Called once per frame from `Generator::set_string_params`.
    pub fn apply_string_params(&mut self, values: Option<&std::collections::BTreeMap<String, String>>) {
        for binding in &self.string_bindings {
            let v: String = values
                .and_then(|m| m.get(binding.source_key.as_str()))
                .cloned()
                .unwrap_or_else(|| binding.default.clone());
            let _ = self.graph.set_param(
                binding.target_node,
                &binding.target_param,
                ParamValue::String(std::sync::Arc::new(v)),
            );
        }
    }

    /// Seed every string binding with its declared default. Called once
    /// at construction so inner-node String params are populated before
    /// the host's first `set_string_params` call.
    fn apply_string_defaults(&mut self) {
        for binding in &self.string_bindings {
            let _ = self.graph.set_param(
                binding.target_node,
                &binding.target_param,
                ParamValue::String(std::sync::Arc::new(binding.default.clone())),
            );
        }
    }

    /// Run one frame against the configured executor. Mock-backend mode
    /// is fine for unit tests; production use installs a Metal
    /// backend via [`Self::set_executor`] before calling.
    pub fn execute_frame(&mut self, time: FrameTime) {
        self.executor
            .execute_frame(&mut self.graph, &self.plan, time);
    }
}

impl Generator for JsonGraphGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &self.type_id
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        target: &GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        // 1. Push the per-frame timing into the system.generator_input
        // node's params. Downstream primitives read these as scalar
        // wires.
        self.set_frame_context(
            ctx.time as f32,
            ctx.beat as f32,
            ctx.aspect,
            ctx.trigger_count as f32,
            ctx.anim_progress,
            ctx.output_width as f32,
            ctx.output_height as f32,
        );

        // 2. Push the host's outer-card slider values through the
        // preset's bindings. Without this the inner-node params stay at
        // their JSON defaults and the user's slider drags do nothing —
        // it was the cause of the "Plasma looks frozen" bug.
        let values = &ctx.params[..ctx.param_count.min(ctx.params.len() as u32) as usize];
        self.apply_param_values(values);

        // 3. Install the host's target as the FinalOutput's source slot.
        // First call pre-binds + swaps; later calls swap in place.
        self.install_target(target);

        // 3. Run the graph through the state-aware executor entry so
        // stateful primitives (Feedback, ArrayFeedback, EnvelopeFollower,
        // Smoothing, Temporal) work without per-frame panics. State is
        // keyed by (NodeInstanceId, owner_key=0); the generator instance
        // is itself per-layer, so the NodeInstanceId alone uniquely
        // identifies state inside this graph.
        let frame_time = FrameTime {
            beats: Beats(ctx.beat),
            seconds: Seconds(ctx.time),
            delta: Seconds(ctx.dt as f64),
            frame_count: 0,
        };
        self.executor.execute_frame_with_state(
            &mut self.graph,
            &self.plan,
            frame_time,
            gpu,
            &mut self.state_store,
            /* owner_key */ 0,
        );

        // No anim_progress tracking inside the JSON graph (yet) — pass
        // the host's value through. Future iteration: surface a node
        // that emits anim_progress and pipe its output to the
        // generator's return value.
        ctx.anim_progress
    }

    fn set_string_params(
        &mut self,
        params: Option<&std::collections::BTreeMap<String, String>>,
    ) {
        self.apply_string_params(params);
    }

    fn reset_state(&mut self, _device: &GpuDevice) {
        // Two parallel state stores need clearing:
        // 1. Per-primitive `extra_fields` state — ArrayFeedback's prev
        //    buffer, RenderLines' anim_progress, ClipTriggerCycle's
        //    last_emitted, plus any pipeline caches the macro emits.
        //    Each primitive's `clear_state()` is the canonical reset
        //    hook for these.
        // 2. The runtime's `StateStore` — temporal::Feedback's prev-
        //    frame textures, EnvelopeFollower / Smoothing accumulators,
        //    array_feedback's prev-frame buffer in the store.
        //
        // Both have to fire because they hold distinct slices of state
        // for the same logical "this primitive's per-instance memory."
        // The Rust generator side (Plasma, FluidSim, etc.) hoists all
        // state into the struct itself, so a single `clear_state` is
        // enough — the graph side splits it across two surfaces.
        for inst in self.graph.nodes_mut() {
            inst.node.clear_state();
        }
        self.state_store.cleanup_all();
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn resize(&mut self, device: &GpuDevice, width: u32, height: u32) {
        // Push the new dims into the MetalBackend so future lazy-alloc
        // acquires get textures at the host's render resolution. Without
        // this, every intermediate texture stays frozen at the
        // construction-time size and the final pass writing into a
        // larger host target only fills the original sub-rect (the
        // "top-left corner only" rendering bug).
        let Some(format) = self.target_format else {
            // Mock-backend test path — no GPU, no resources to invalidate.
            return;
        };
        let Some(metal) = self
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<MetalBackend>())
        else {
            return;
        };
        metal.resize(width, height);
        // `resize` wiped every pinned binding (including the
        // final-output placeholder) so the slot index is stale. Pre-bind
        // a fresh 1×1 placeholder; `install_target` will swap in the
        // host's real target on the next frame.
        let placeholder = RenderTarget::new(
            device,
            1,
            1,
            format,
            "json_graph_generator_target_owner",
        );
        let slot = metal.pre_bind_texture_2d(self.final_output_input_resource, placeholder);
        self.final_output_slot = Some(slot);
        // `resize` also wiped every pinned `Array<T>` buffer and every
        // `Texture3D` volume (the chain build's vertex/particle/density
        // pre-allocations). Re-run the canonical pre-allocate pass so
        // downstream primitives don't render against an empty wire —
        // symptom is a black generator output on the first frame after
        // a project load, only recovering when the user edits the graph.
        // Log any sizing failure rather than propagating it — `resize`
        // runs on the hot path and has no error return; the original
        // load-time check already caught the same condition.
        if let Err(e) =
            crate::node_graph::pre_allocate_resources(&self.graph, &self.plan, device, metal)
        {
            log::warn!("JsonGraphGenerator::resize re-allocation failed: {e}");
        }
    }
}

/// Map a [`crate::node_graph::PreAllocationError`] into the existing
/// [`JsonGeneratorLoadError`] surface so `from_def_with_device`'s
/// public error type is unchanged.
fn generator_error_from_prealloc(e: crate::node_graph::PreAllocationError) -> JsonGeneratorLoadError {
    use crate::node_graph::PreAllocationError as P;
    match e {
        P::UnsizedArrayOutput {
            node_type, port, ..
        } => JsonGeneratorLoadError::UnsizedArrayOutput { node_type, port },
        P::UnsizedTexture3DOutput {
            node_type, port, ..
        } => JsonGeneratorLoadError::UnsizedTexture3DOutput { node_type, port },
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


#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::PrimitiveRegistry;

    use manifold_core::Beats;
    use manifold_core::Seconds;

    // (Audit-fires-on-unbound and audit-accepts-fully-bound regressions
    // now live in `node_graph::graph_loader::tests` — same contract,
    // closer to the shared implementation.)

    /// Regression for the "Lissajous repeats back-to-back in
    /// clip-trigger mode" bug. The preset declares two bindings keyed
    /// by the same outer-card id (`clip_trigger` → `mux_x.selector`
    /// AND `clip_trigger` → `mux_y.selector`). Before the source-index
    /// fix, `apply_param_values` indexed `values` by binding position,
    /// so the 12th binding fell off the end of the 11-element
    /// `values` slice and `mux_y.selector` stayed pinned at its
    /// default 0.0 — meaning Y stayed on the LFO while X cycled
    /// through the frequency-ratio table. Adjacent ratio rows share
    /// the `a` value (rows 0/1, 3/4, 6/7), so two consecutive triggers
    /// produced visually identical curves whenever the slow LFO_y
    /// hadn't moved far between them.
    ///
    /// The assertion: drive `clip_trigger = 1.0` and confirm BOTH
    /// mux selectors land on 1.0, not just the first.
    #[test]
    fn fan_out_binding_writes_every_target_with_the_same_outer_value() {
        let json = include_str!(
            "../../assets/generator-presets/Lissajous.json"
        );
        let mut g = JsonGraphGenerator::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("Lissajous preset must load");

        let mux_x_id = g
            .graph
            .handles()
            .find(|(h, _)| *h == "mux_x")
            .map(|(_, id)| id)
            .expect("Lissajous declares a `mux_x` handle");
        let mux_y_id = g
            .graph
            .handles()
            .find(|(h, _)| *h == "mux_y")
            .map(|(_, id)| id)
            .expect("Lissajous declares a `mux_y` handle");

        // Outer-card slider order from Lissajous.json `params[]`:
        //   freq_x_rate, freq_y_rate, phase_rate, line, show_verts,
        //   vert_size, animate, speed, window, scale, clip_trigger
        // The last value (index 10) is `clip_trigger = 1.0` —
        // enabling the ratio-stepped mode that drives both muxes.
        g.apply_param_values(&[
            0.13, 0.09, 0.07, 0.002, 1.0,
            1.0, 0.0, 1.0, 0.1, 1.0,
            1.0,
        ]);

        let mux_x = g.graph.get_node(mux_x_id).unwrap();
        assert!(
            matches!(
                mux_x.params.get("selector"),
                Some(ParamValue::Float(v)) if (*v - 1.0).abs() < 1e-5
            ),
            "mux_x.selector should be 1.0, got {:?}",
            mux_x.params.get("selector"),
        );
        let mux_y = g.graph.get_node(mux_y_id).unwrap();
        assert!(
            matches!(
                mux_y.params.get("selector"),
                Some(ParamValue::Float(v)) if (*v - 1.0).abs() < 1e-5
            ),
            "mux_y.selector should be 1.0 (fan-out from same `clip_trigger` \
             outer slider as mux_x), got {:?}. If this is 0.0, the binding \
             is incorrectly indexed by position instead of by source id.",
            mux_y.params.get("selector"),
        );
    }

    /// Regression test for the "Plasma looks frozen" bug: outer-card
    /// slider values must reach the inner-node param via the preset's
    /// declared bindings.
    #[test]
    fn apply_param_values_routes_into_inner_node_params() {
        let json = include_str!(
            "../../assets/generator-presets/Plasma.json"
        );
        let mut g = JsonGraphGenerator::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("Plasma preset must load");

        // Find the plasma node's runtime id (handle = "plasma").
        let plasma_id = g
            .graph
            .handles()
            .find(|(h, _)| *h == "plasma")
            .map(|(_, id)| id)
            .expect("Plasma preset declares a node with handle `plasma`");

        // Push values in the same order Plasma's presetMetadata.params
        // declares them: pattern, complexity, contrast, speed, scale, clip_trigger.
        g.apply_param_values(&[3.0, 0.75, 0.42, 2.5, 1.5, 1.0]);

        // The bindings should have updated each target param.
        let inst = g.graph.get_node(plasma_id).unwrap();
        assert!(matches!(
            inst.params.get("complexity"),
            Some(ParamValue::Float(v)) if (*v - 0.75).abs() < 1e-5
        ));
        assert!(matches!(
            inst.params.get("contrast"),
            Some(ParamValue::Float(v)) if (*v - 0.42).abs() < 1e-5
        ));
        assert!(matches!(
            inst.params.get("speed"),
            Some(ParamValue::Float(v)) if (*v - 2.5).abs() < 1e-5
        ));
        assert!(matches!(
            inst.params.get("scale"),
            Some(ParamValue::Float(v)) if (*v - 1.5).abs() < 1e-5
        ));
    }

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }


    /// Smoke test for the loader path. A minimal generator preset:
    /// `generator_input → uv_field → final_output`. Parses to a
    /// runtime graph, compiles, and executes against the mock backend.
    #[test]
    fn trivial_passthrough_generator_loads_and_executes() {
        let json = r#"{
            "version": 1,
            "name": "TestPassthrough",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.uv_field", "handle": "uv" },
                { "id": 2, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;

        let mut preset =
            JsonGraphGenerator::from_json_str(json, &PrimitiveRegistry::with_builtin())
                .expect("trivial generator preset must load");
        assert_eq!(preset.type_id().as_str(), "TestPassthrough");

        preset.set_frame_context(1.5, 0.5, 1.78, 4.0, 0.25, 1920.0, 1080.0);
        preset.execute_frame(frame_time());
    }

    /// `JsonGraphGenerator` holds a `Graph` which doesn't impl Debug —
    /// so we can't use `Result::expect_err` against the Ok variant
    /// (which would need to format the contained value). Tests below
    /// destructure the Result by hand instead.
    fn unwrap_err(
        r: Result<JsonGraphGenerator, JsonGeneratorLoadError>,
    ) -> JsonGeneratorLoadError {
        match r {
            Ok(_) => panic!("expected JsonGeneratorLoadError, got Ok(JsonGraphGenerator)"),
            Err(e) => e,
        }
    }

    #[test]
    fn missing_generator_input_is_a_clean_error() {
        // No `system.generator_input` node — wrong-shaped preset.
        let json = r#"{
            "version": 1,
            "name": "Bad",
            "nodes": [
                { "id": 0, "typeId": "system.final_output" }
            ],
            "wires": []
        }"#;
        let err = unwrap_err(JsonGraphGenerator::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        ));
        assert!(
            matches!(err, JsonGeneratorLoadError::MissingGeneratorInput),
            "got {err:?}"
        );
    }

    /// Integration smoke test for the infra session as a whole: a
    /// generator graph that exercises `system.generator_input`,
    /// `node.mux_texture` (variadic ports), and two `node.wgsl_compute`
    /// branches — D1 + D2 wired together in one preset.
    ///
    /// (The original third axis here was an `outputFormats` override
    /// on a legacy `node.wgsl_compute_0in_1tex` branch; with the
    /// legacy variants deleted in Phase 4b and the generic
    /// `node.wgsl_compute` deriving its format from its WGSL source,
    /// that capability surface no longer applies to this test.)
    #[test]
    fn infra_session_integration_smoke_test() {
        let json = r#"{
            "version": 2,
            "name": "InfraSmoke",
            "presetMetadata": {
                "id": "InfraSmoke",
                "displayName": "Infra Smoke",
                "category": "Diagnostic",
                "oscPrefix": "infra_smoke",
                "params": [],
                "bindings": []
            },
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.wgsl_compute", "handle": "branch_a" },
                { "id": 2, "typeId": "node.wgsl_compute", "handle": "branch_b" },
                { "id": 3, "typeId": "node.mux_texture", "handle": "mux" },
                { "id": 4, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "trigger_count", "toNode": 3, "toPort": "selector" },
                { "fromNode": 1, "fromPort": "output_tex", "toNode": 3, "toPort": "in_0" },
                { "fromNode": 2, "fromPort": "output_tex", "toNode": 3, "toPort": "in_1" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;

        let preset = JsonGraphGenerator::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("infra smoke preset must load");
        assert_eq!(preset.type_id().as_str(), "InfraSmoke");
        // `node.wgsl_compute` requires a GpuEncoder for dispatch, so
        // the original `execute_frame` step here is dropped — the
        // legacy variant this test originally used was CPU-only. The
        // load + compile validation above is the load-bearing piece.
    }

    /// Load the bundled `ComputeStrangeAttractor.json` preset from
    /// disk and execute it. First decomposition of the particle
    /// generator family — wires the full pipeline (seed[OnceOnReset]
    /// → integrate_attractor → scatter[Discard] → resolve →
    /// reinhard) plus the canvas-area-scale brightness compensation
    /// chain. If the schema is malformed or any binding's target
    /// inner-node param can't be resolved this fails immediately
    /// instead of running with silent stale defaults.
    #[test]
    fn bundled_strange_attractor_loads_and_compiles() {
        // After the 2026-05-26 decomposition the simulate path is
        // `node.wgsl_compute` (JSON-editable shader) which strictly
        // declares `requires().gpu_encoder = true`. The chain-build
        // path needs a GpuDevice for pre-allocation; the actual GPU
        // execution path is covered by Generator::render in
        // production. This test asserts the preset loads, the WGSL
        // introspects (uniform layout, port shape, Particle alias
        // detection), and the chain pre-allocator wires every Array<T>
        // / Texture2D resource without error.
        let device = crate::test_device();
        let json = include_str!(
            "../../assets/generator-presets/ComputeStrangeAttractor.json"
        );
        let preset = JsonGraphGenerator::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            &device,
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
        )
        .expect("bundled ComputeStrangeAttractor must load + compile");
        assert_eq!(preset.type_id().as_str(), "ComputeStrangeAttractor");
    }

    /// Load + compile the bundled `Plasma.json` preset from disk. This
    /// is the JSON Plasma that supersedes the legacy Rust factory. After
    /// the 2026-05-29 decomposition Plasma's pattern path is a single
    /// `node.wgsl_compute` (JSON-editable shader) which declares
    /// `requires().gpu_encoder = true`, so the CPU `execute_frame` path
    /// no longer applies — GPU execution is covered by
    /// `bundled_generator_presets::every_bundled_preset_executes_one_frame`
    /// (and `Generator::render` in production). This asserts the preset
    /// loads, the WGSL introspects, and the chain pre-allocator wires
    /// every resource without error. The `Plasma` type_id binding is
    /// load-bearing: it's what makes the editor cog populate for existing
    /// Plasma layers and what causes the registry to pick this JSON over
    /// the Rust factory at runtime.
    #[test]
    fn bundled_plasma_loads_and_compiles() {
        let device = crate::test_device();
        let json = include_str!(
            "../../assets/generator-presets/Plasma.json"
        );
        let preset = JsonGraphGenerator::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            &device,
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
        )
        .expect("bundled Plasma must load + compile");
        assert_eq!(preset.type_id().as_str(), "Plasma");
    }

    /// Regression for the "loaded project renders black until the user
    /// opens the graph editor" bug: `JsonGraphGenerator::resize` calls
    /// through to `MetalBackend::resize`, which wipes every pinned
    /// binding — including the `Array<T>` output buffers the chain-build
    /// pre-allocated. Before the fix, `resize` only re-pre-bound the
    /// final-output placeholder; the curve-vertex buffer stayed unbound
    /// and downstream `render_lines` saw an empty wire, producing a
    /// black frame. The host's first `resize_gpu` call after project
    /// load is what triggered the bug in production.
    ///
    /// Asserts that every `Array<T>` resource that was bound after
    /// construction is still bound (and points at a real buffer) after
    /// a `resize`.
    #[test]
    fn resize_re_pre_allocates_array_buffers() {
        use crate::node_graph::{Backend, PortType};
        let device = crate::test_device();
        let json = include_str!(
            "../../assets/generator-presets/Lissajous.json"
        );
        let mut g = JsonGraphGenerator::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            &device,
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
        )
        .expect("Lissajous preset must load");

        let array_resources: Vec<ResourceId> = (0..g.plan.resource_count() as u32)
            .map(ResourceId)
            .filter(|id| matches!(g.plan.resource_type(*id), Some(PortType::Array(_))))
            .collect();
        assert!(
            !array_resources.is_empty(),
            "Lissajous preset must produce at least one Array<T> wire \
             (the curve-vertex buffer) — otherwise the regression isn't \
             exercising the bug",
        );

        // All Array resources bound + backed by a real buffer at construction.
        {
            let metal = g
                .executor
                .backend_mut()
                .as_any_mut()
                .and_then(|a| a.downcast_mut::<MetalBackend>())
                .expect("production path constructs a MetalBackend");
            for &res in &array_resources {
                let slot = metal
                    .slot_for(res)
                    .unwrap_or_else(|| panic!("Array resource {res:?} unbound after construction"));
                assert!(
                    Backend::array_buffer(metal, slot).is_some(),
                    "Array resource {res:?} has no backing buffer after construction",
                );
            }
        }

        // Simulate the host's first resize_gpu call (project's actual
        // render resolution diverges from the 1920x1080 construction
        // default).
        Generator::resize(&mut g, &device, 1280, 720);

        // Same bindings must survive.
        let metal = g
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("production path constructs a MetalBackend");
        for &res in &array_resources {
            let slot = metal.slot_for(res).unwrap_or_else(|| {
                panic!(
                    "Array resource {res:?} unbound after resize — \
                     the pre-allocate pass didn't re-run",
                )
            });
            assert!(
                Backend::array_buffer(metal, slot).is_some(),
                "Array resource {res:?} has no backing buffer after resize",
            );
        }
    }

    /// Architectural regression: a stateful array simulator that
    /// declares `aliased_array_io()` must have its `in` and `out`
    /// ports resolved to the **same physical slot** by the chain
    /// build, and downstream consumers' input slots must equal that
    /// same slot. Pre-fix the chain builder allocated separate
    /// buffers per wire, so `integrate.in`, `integrate.out`, and
    /// `scatter.particles` were three different MTLBuffers — every
    /// downstream consumer saw zero data and the entire Strange
    /// Attractor pipeline rendered black.
    ///
    /// This test pins the fix at the chain-build level: the same
    /// slot identity is the invariant that lets the buffer-flow
    /// model carry stateful simulator data correctly.
    #[test]
    fn aliased_array_io_routes_in_and_out_to_one_physical_slot() {
        use crate::node_graph::Backend;
        let device = crate::test_device();
        let json = include_str!(
            "../../assets/generator-presets/ComputeStrangeAttractor.json"
        );
        let mut g = JsonGraphGenerator::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            &device,
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
        )
        .expect("ComputeStrangeAttractor preset must load");

        // Find the wgsl_compute simulate node, scatter node — the
        // alias contract is about THIS edge: simulate.particles
        // (read_write storage maps to same-named in/out port pair),
        // and scatter.particles consumes simulate's output.
        let find_node = |type_id: &str| -> NodeInstanceId {
            for step in g.plan.steps() {
                let inst = g.graph.get_node(step.node).expect("step's node");
                if inst.node.type_id().as_str() == type_id {
                    return step.node;
                }
            }
            panic!("node `{type_id}` not in compiled plan");
        };
        let integrate_node = find_node("node.wgsl_compute");
        let scatter_node = find_node("node.scatter_particles");

        let resource_for = |node: NodeInstanceId, port: &str, is_input: bool| -> ResourceId {
            for step in g.plan.steps() {
                if step.node == node {
                    let ports = if is_input { &step.inputs } else { &step.outputs };
                    for &(name, id) in ports {
                        if name == port {
                            return id;
                        }
                    }
                }
            }
            panic!(
                "missing {} port `{port}` on node {node:?}",
                if is_input { "input" } else { "output" }
            );
        };

        let integrate_in_res = resource_for(integrate_node, "particles", true);
        let integrate_out_res = resource_for(integrate_node, "particles", false);
        let scatter_in_res = resource_for(scatter_node, "particles", true);

        let metal = g
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("production path constructs a MetalBackend");

        let in_slot = metal
            .slot_for(integrate_in_res)
            .expect("integrate.in must be bound after chain build");
        let out_slot = metal
            .slot_for(integrate_out_res)
            .expect("integrate.out must be bound after chain build");
        let scatter_slot = metal
            .slot_for(scatter_in_res)
            .expect("scatter.particles must be bound after chain build");

        assert_eq!(
            in_slot, out_slot,
            "aliased_array_io declared `in → out` but the chain builder \
             allocated separate slots — the simulator's in-place writes \
             would not be visible downstream",
        );
        assert_eq!(
            out_slot, scatter_slot,
            "integrate.out and scatter.particles must resolve to the same \
             slot (they share the wire), proving the aliased buffer flows \
             through downstream",
        );
        assert!(
            Backend::array_buffer(metal, in_slot).is_some(),
            "the shared slot must back a real GpuBuffer",
        );
    }

    /// Architectural regression: primitives whose output must align
    /// with the host canvas (scatter accumulators, future density
    /// grids) declare `canvas_sized_array_outputs()` and the chain
    /// builder sizes the buffer from `Backend::canvas_dims()` — not
    /// from hardcoded JSON params. Pre-fix, scatter's `width`/`height`
    /// were 1920/1080 in the JSON; at any other host resolution the
    /// splat coords only filled the top-left quadrant of the
    /// canvas-sized density texture downstream.
    ///
    /// This test loads the bundled Strange Attractor preset at two
    /// different canvas sizes and asserts the scatter accumulator
    /// buffer's byte size scales with the canvas, not with the JSON
    /// param.
    #[test]
    fn canvas_sized_array_outputs_scale_buffer_with_backend_canvas_dims() {
        use crate::node_graph::Backend;
        let device = crate::test_device();
        let json = include_str!(
            "../../assets/generator-presets/ComputeStrangeAttractor.json"
        );

        let cases = [(1280u32, 720u32), (3840u32, 2160u32)];
        for (w, h) in cases {
            let mut g = JsonGraphGenerator::from_json_str_with_device(
                json,
                &PrimitiveRegistry::with_builtin(),
                &device,
                w,
                h,
                GpuTextureFormat::Rgba16Float,
            )
            .expect("preset must load");

            let scatter = (|| {
                for step in g.plan.steps() {
                    let inst = g.graph.get_node(step.node).expect("step's node");
                    if inst.node.type_id().as_str() == "node.scatter_particles" {
                        return step.node;
                    }
                }
                panic!("scatter node missing");
            })();
            let accum_res = (|| {
                for step in g.plan.steps() {
                    if step.node == scatter {
                        for &(name, id) in &step.outputs {
                            if name == "accum" {
                                return id;
                            }
                        }
                    }
                }
                panic!("scatter.accum resource missing");
            })();

            let metal = g
                .executor
                .backend_mut()
                .as_any_mut()
                .and_then(|a| a.downcast_mut::<MetalBackend>())
                .expect("metal backend");
            let slot = metal.slot_for(accum_res).expect("scatter.accum unbound");
            let buf = Backend::array_buffer(metal, slot).expect("no backing buffer");
            let expected = (w as u64) * (h as u64) * 4;
            assert_eq!(
                buf.size, expected,
                "scatter.accum at canvas {w}x{h} should be {expected} bytes, got {}",
                buf.size,
            );
        }
    }

    /// Load the bundled `TrivialPassthrough.json` preset from disk and
    /// execute it. Confirms the generator-presets directory wiring,
    /// the on-disk schema, and the full loader path are aligned.
    #[test]
    fn bundled_trivial_passthrough_preset_loads_and_executes() {
        let json = include_str!(
            "../../assets/generator-presets/TrivialPassthrough.json"
        );
        let mut preset = JsonGraphGenerator::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("bundled TrivialPassthrough must load");
        assert_eq!(preset.type_id().as_str(), "TrivialPassthrough");
        preset.set_frame_context(0.0, 0.0, 1.78, 0.0, 0.0, 1920.0, 1080.0);
        preset.execute_frame(frame_time());
    }

    #[test]
    fn missing_final_output_is_a_clean_error() {
        let json = r#"{
            "version": 1,
            "name": "Bad",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input" }
            ],
            "wires": []
        }"#;
        let err = unwrap_err(JsonGraphGenerator::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        ));
        assert!(
            matches!(err, JsonGeneratorLoadError::MissingFinalOutput),
            "got {err:?}"
        );
    }
}
