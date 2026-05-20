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
    EffectGraphDefExt, ExecutionPlan, Executor, FINAL_OUTPUT_TYPE_ID, FrameTime,
    GENERATOR_INPUT_TYPE_ID, Graph, GraphError, LoadError, MetalBackend, NodeInstanceId,
    ParamValue, PrimitiveRegistry, ResourceId, Slot, compile,
};
use crate::render_target::RenderTarget;

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
    /// Outer-card → inner-node bindings resolved at construction time.
    /// Each render frame walks these and pushes the corresponding
    /// `GeneratorContext::params[i]` into the target node's param.
    /// Empty when the preset declares no bindings.
    bindings: Vec<BindingResolution>,
}

/// One resolved outer-card → inner-node binding for a JSON generator
/// preset. Built once at construction by walking the preset's
/// `BindingDef` list against the live graph's handle map; subsequent
/// frames index this vec by `GeneratorContext::params[i]` position.
struct BindingResolution {
    target_node: NodeInstanceId,
    /// Param name on the target node. The graph's `set_param` API
    /// requires `&'static str`, so we Box::leak the param name once at
    /// construction. The leak is bounded — one per preset binding,
    /// times the number of generator constructions in a session — and
    /// matches the existing pattern in `loaded_preset_view`.
    target_param: &'static str,
    default: f32,
    convert: manifold_core::effects::ParamConvert,
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

        // Capture the binding specs before into_graph consumes `doc`.
        // These get resolved against the live graph's handle map below.
        let binding_specs: Vec<manifold_core::effect_graph_def::BindingDef> = doc
            .preset_metadata
            .as_ref()
            .map(|m| m.bindings.clone())
            .unwrap_or_default();

        let graph = doc.into_graph(registry)?;
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
        // handle map. Bindings whose handle doesn't resolve are silently
        // dropped (the host doesn't surface a runtime warn from
        // primitive load yet).
        use manifold_core::effect_graph_def::BindingTarget;
        let handle_map: std::collections::HashMap<&str, NodeInstanceId> =
            graph.handles().map(|(h, id)| (h, id)).collect();
        let bindings: Vec<BindingResolution> = binding_specs
            .iter()
            .filter_map(|b| match &b.target {
                BindingTarget::HandleNode { handle, param } => {
                    let node_id = *handle_map.get(handle.as_str())?;
                    let leaked_param: &'static str =
                        Box::leak(param.clone().into_boxed_str());
                    Some(BindingResolution {
                        target_node: node_id,
                        target_param: leaked_param,
                        default: b.default_value,
                        convert: b.convert,
                    })
                }
                BindingTarget::Composite { .. } => None,
            })
            .collect();

        Ok(Self {
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
        })
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
        let mut g = Self::from_json_str(json, registry)?;
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
        g.executor = Executor::new(Box::new(backend));
        Ok(g)
    }

    /// Stable identity for the GeneratorRegistry.
    pub fn type_id(&self) -> &GeneratorTypeId {
        &self.type_id
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
    /// setting its five float params (`time`, `beat`, `aspect`,
    /// `trigger_count`, `anim_progress`). Called by the host once per
    /// frame before [`Self::execute_frame`]. Uses the standard
    /// `Graph::set_param` path — no downcasting, no special-case
    /// plumbing.
    pub fn set_frame_context(
        &mut self,
        time: f32,
        beat: f32,
        aspect: f32,
        trigger_count: f32,
        anim_progress: f32,
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
    }

    /// Push the host's slider values through the preset's bindings to
    /// the matching inner-node params. Each `values[i]` lands on the
    /// `i`-th binding declared in `presetMetadata.bindings`. Missing
    /// entries fall back to the binding's `default_value`. No-op if the
    /// preset declared no bindings.
    pub fn apply_param_values(&mut self, values: &[f32]) {
        use manifold_core::effects::ParamConvert;
        for (i, binding) in self.bindings.iter().enumerate() {
            let v = values.get(i).copied().unwrap_or(binding.default);
            let pv = match binding.convert {
                ParamConvert::Float => ParamValue::Float(v),
                ParamConvert::IntRound => ParamValue::Int(v.round() as i32),
                ParamConvert::BoolThreshold => ParamValue::Bool(v > 0.5),
                ParamConvert::EnumRound => ParamValue::Enum(v.round().max(0.0) as u32),
            };
            let _ = self
                .graph
                .set_param(binding.target_node, binding.target_param, pv);
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

        // 3. Run the graph. We dispatch through execute_frame_with_gpu;
        // graphs that hold state (Feedback, mip chains) would need the
        // _with_state path — JSON generators don't compose stateful
        // primitives yet, so this is sufficient.
        let frame_time = FrameTime {
            beats: Beats(ctx.beat),
            seconds: Seconds(ctx.time),
            delta: Seconds(ctx.dt as f64),
            frame_count: 0,
        };
        self.executor.execute_frame_with_gpu(
            &mut self.graph,
            &self.plan,
            frame_time,
            gpu,
        );

        // No anim_progress tracking inside the JSON graph (yet) — pass
        // the host's value through. Future iteration: surface a node
        // that emits anim_progress and pipe its output to the
        // generator's return value.
        ctx.anim_progress
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
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::PrimitiveRegistry;

    use manifold_core::Beats;
    use manifold_core::Seconds;

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
        // declares them: pattern, complexity, contrast, speed, scale, snap.
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

        preset.set_frame_context(1.5, 0.5, 1.78, 4.0, 0.25);
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
    /// `node.mux_texture` (variadic ports), and an `outputFormats`
    /// override (per-slot format) on a `node.wgsl_compute_0in_1tex`
    /// branch — D1 + D2 + D5 wired together in one preset.
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
                {
                    "id": 1,
                    "typeId": "node.wgsl_compute_0in_1tex",
                    "handle": "branch_a",
                    "outputFormats": { "out": "rgba32float" }
                },
                { "id": 2, "typeId": "node.wgsl_compute_0in_1tex", "handle": "branch_b" },
                { "id": 3, "typeId": "node.mux_texture", "handle": "mux" },
                { "id": 4, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "trigger_count", "toNode": 3, "toPort": "selector" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in_0" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in_1" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;

        let mut preset = JsonGraphGenerator::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("infra smoke preset must load");
        assert_eq!(preset.type_id().as_str(), "InfraSmoke");

        // Verify the per-format propagation reached the compiled plan.
        // Resource ordering is topological-by-node, so we search for
        // a resource declaring rgba32float rather than hardcoding the
        // index (which would break if anyone reorders the JSON).
        let mut found_rgba32 = false;
        for i in 0..preset.plan.resource_count() {
            if preset.plan.resource_format(ResourceId(i as u32))
                == Some(manifold_gpu::GpuTextureFormat::Rgba32Float)
            {
                found_rgba32 = true;
                break;
            }
        }
        assert!(
            found_rgba32,
            "branch_a's rgba32float output_format override must reach the plan",
        );

        preset.set_frame_context(0.0, 0.0, 1.78, 1.0, 0.0);
        preset.execute_frame(frame_time());
    }

    /// Load the bundled `Plasma.json` preset from disk and execute it.
    /// This is the JSON Plasma that supersedes the legacy Rust factory
    /// — a non-trivial graph (~75 nodes) exercising the procedural-math
    /// vocabulary + port-shadows-param + system.generator_input. The
    /// `Plasma` type_id binding is load-bearing: it's what makes the
    /// editor cog populate for existing Plasma layers and what causes
    /// the registry to pick this JSON over the Rust factory at runtime.
    #[test]
    fn bundled_plasma_loads_and_executes() {
        let json = include_str!(
            "../../assets/generator-presets/Plasma.json"
        );
        let mut preset = JsonGraphGenerator::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("bundled Plasma must load");
        assert_eq!(preset.type_id().as_str(), "Plasma");
        preset.set_frame_context(0.0, 0.0, 1.78, 0.0, 0.0);
        preset.execute_frame(frame_time());
        // Advance time and execute again — phase should propagate
        // through the wired Math chains without the graph panicking.
        preset.set_frame_context(0.5, 0.25, 1.78, 0.0, 0.5);
        preset.execute_frame(frame_time());
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
        preset.set_frame_context(0.0, 0.0, 1.78, 0.0, 0.0);
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
