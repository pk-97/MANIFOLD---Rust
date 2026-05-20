//! Loader shim that turns a JSON-defined generator graph into a
//! runtime-executable bundle.
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
//! and an `Executor`. At each frame the host updates the
//! `GeneratorInput`'s cached frame context (time, beat, aspect,
//! trigger_count, anim_progress), then runs the graph. The target
//! texture pre-binds to the FinalOutput's input resource so the last
//! primitive in the chain writes directly into it.
//!
//! This struct is the foundation for the production `Generator` trait
//! integration — the trait impl that calls `Generator::render` on this
//! wrapper lands when the first Tier 1 generator preset ships.

use manifold_core::{
    GeneratorTypeId,
    effect_graph_def::{EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef},
};

use crate::node_graph::{
    EffectGraphDefExt, ExecutionPlan, Executor, FINAL_OUTPUT_TYPE_ID, FrameTime,
    GENERATOR_INPUT_TYPE_ID, Graph, GraphError, LoadError, NodeInstanceId, ParamValue,
    PrimitiveRegistry, ResourceId, compile,
};

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
/// frame context, run the executor.
#[allow(dead_code)] // `executor` and other fields are exercised through public methods + tests
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

        Ok(Self {
            type_id,
            graph,
            plan,
            executor: Executor::with_mock(),
            generator_input_id,
            final_output_id,
            final_output_input_resource,
        })
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

    /// Run one frame against the configured executor. Mock-backend mode
    /// is fine for unit tests; production use installs a Metal
    /// backend via [`Self::set_executor`] before calling.
    pub fn execute_frame(&mut self, time: FrameTime) {
        self.executor
            .execute_frame(&mut self.graph, &self.plan, time);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::PrimitiveRegistry;

    use manifold_core::Beats;
    use manifold_core::Seconds;

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

    /// Load the bundled `PlasmaClassicDecomposed.json` preset from
    /// disk and execute it. The first Tier 1 generator — a
    /// non-trivial graph (~25 nodes) exercising the procedural-math
    /// vocabulary + port-shadows-param + system.generator_input. If
    /// this loads and executes cleanly, the infrastructure is sound.
    #[test]
    fn bundled_plasma_classic_decomposed_loads_and_executes() {
        let json = include_str!(
            "../../assets/generator-presets/PlasmaClassicDecomposed.json"
        );
        let mut preset = JsonGraphGenerator::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("bundled PlasmaClassicDecomposed must load");
        assert_eq!(preset.type_id().as_str(), "PlasmaClassicDecomposed");
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
