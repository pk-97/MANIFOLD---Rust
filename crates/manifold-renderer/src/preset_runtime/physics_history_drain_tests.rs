//! Native FLIP history catch-up through the ordinary CPU graph path.
use super::*;
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::node_graph::{EffectNode, EffectNodeContext, EffectNodeType, ParamDef};
use std::{borrow::Cow, cell::Cell};

thread_local! {
    static FLUID_TIME: Cell<Option<f32>> = const { Cell::new(None) };
}

struct FluidTimeObserver(EffectNodeType);

impl EffectNode for FluidTimeObserver {
    fn is_liveness_root(&self) -> bool {
        true
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 1] = [NodePort {
            name: Cow::Borrowed("time"),
            ty: PortType::Scalar(ScalarType::F32),
            kind: PortKind::Input,
            required: true,
        }];
        &INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        assert!(
            !crate::node_graph::physics::authored_sample_only(),
            "downstream consumers must only run in the full frame"
        );
        FLUID_TIME.set(
            ctx.inputs
                .scalar("time")
                .and_then(|value| value.as_scalar()),
        );
    }
}

fn runtime() -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("test.fluid_time", || {
        Box::new(FluidTimeObserver(EffectNodeType::new("test.fluid_time")))
    });
    let def = serde_json::json!({
        "version": 2, "name": "Fluid offline history",
        "nodes": [
            {"id":0,"nodeId":"fluid","typeId":"node.fluid_surface","params":{
                "resolution":{"type":"Int","value":8},
                "fill_height":{"type":"Float","value":0.0},
                "emission":{"type":"Float","value":0.0}
            }},
            {"id":1,"nodeId":"observe","typeId":"test.fluid_time"},
            {"id":2,"nodeId":"source","typeId":"system.source"},
            {"id":3,"nodeId":"output","typeId":"system.final_output"},
            {"id":4,"nodeId":"input","typeId":"system.generator_input"}
        ],
        "wires": [
            {"fromNode":0,"fromPort":"simulation_time","toNode":1,"toPort":"time"},
            {"fromNode":2,"fromPort":"out","toNode":3,"toPort":"in"}
        ]
    });
    PresetRuntime::from_json_str(&def.to_string(), &registry).unwrap()
}

#[test]
fn offline_history_drain_crosses_fluid_history_capacity_without_reset() {
    let mut runtime = runtime();
    // 36 seconds generates over 8192 authored endpoints. An empty domain keeps
    // this a bounded scheduling proof; liquid motion is covered separately.
    let time = |seconds: f64| FrameTime {
        seconds: Seconds(seconds),
        beats: Beats(seconds * 2.0),
        delta: Seconds(seconds),
        frame_count: (seconds * 60.0) as i64,
    };
    runtime.execute_frame(time(0.0));
    assert_eq!(FLUID_TIME.get(), Some(0.0));
    FLUID_TIME.set(None);
    runtime.sample_physics_history(time(36.0));
    assert_eq!(
        FLUID_TIME.get(),
        None,
        "history must not run the output consumer"
    );
    let fluid = runtime
        .graph
        .instance_by_node_id(&NodeId::new("fluid"))
        .unwrap();
    let resource = runtime
        .plan
        .steps()
        .iter()
        .find(|step| step.node == fluid)
        .unwrap()
        .outputs
        .iter()
        .find(|(name, _)| *name == "simulation_time")
        .unwrap()
        .1;
    let backend = runtime.executor.backend();
    let published = backend
        .slot_for(resource)
        .and_then(|slot| backend.scalar(slot))
        .and_then(|value| value.as_scalar());
    assert!(
        published.is_none_or(|value| value == 0.0),
        "intermediate native progress must not publish graph outputs: {published:?}"
    );
    runtime
        .executor
        .execute_frame(&mut runtime.graph, &runtime.plan, time(36.0));
    assert_eq!(FLUID_TIME.get(), Some(36.0));
}

#[test]
fn fluid_graph_recording_receives_authoritative_project_tempo_and_exact_breakpoints() {
    use manifold_core::{Bpm, tempo::TempoMap, types::TempoPointSource};
    let directory =
        std::env::temp_dir().join(format!("manifold-fluid-tempo-graph-{}", std::process::id()));
    let mut runtime = runtime();
    let fluid = runtime
        .graph
        .instance_by_node_id(&NodeId::new("fluid"))
        .unwrap();
    runtime
        .graph
        .set_param(fluid, "cache_mode", ParamValue::Enum(1))
        .unwrap();
    runtime
        .graph
        .set_param(
            fluid,
            "cache_path",
            ParamValue::String(directory.to_str().unwrap().to_owned().into()),
        )
        .unwrap();
    let mut map = TempoMap::default();
    map.add_or_replace_point(Beats::ZERO, Bpm(120.0), TempoPointSource::Manual, 0.00001);
    map.add_or_replace_point(Beats(0.03), Bpm(60.0), TempoPointSource::Manual, 0.00001);
    let tempo = ProjectTempo::new(&map, Bpm(120.0));
    runtime.set_project_tempo(Some(&tempo));
    for seconds in [0.0, 0.1] {
        runtime.execute_frame(FrameTime {
            seconds: Seconds(seconds),
            beats: TempoMapConverter::seconds_to_beat_immut(
                tempo.map(),
                Seconds(seconds),
                tempo.fallback_bpm(),
            ),
            delta: Seconds(seconds),
            frame_count: 0,
        });
    }
    let take = crate::node_graph::fluid::FluidTakeReplay::open(&directory).unwrap();
    assert_eq!(take.recorded_tick(), 6);
    take.validate_project_tempo(&tempo).unwrap();
    let changed = ProjectTempo::new(&TempoMap::default(), Bpm(120.0));
    assert!(take.validate_project_tempo(&changed).is_err());
    drop(take);
    drop(runtime);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn fluid_graph_cache_ignores_appearance_but_rejects_authored_force_edits() {
    use crate::node_graph::fluid::FluidDomainState;
    let directory = std::env::temp_dir().join(format!(
        "manifold-fluid-source-graph-{}",
        std::process::id()
    ));
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("test.fluid_time", || {
        Box::new(FluidTimeObserver(EffectNodeType::new("test.fluid_time")))
    });
    let mut definition = serde_json::json!({
        "version": 2, "name": "Recorded source graph",
        "nodes": [
            {"id":0,"nodeId":"fluid","typeId":"node.fluid_surface","params":{
                "resolution":{"type":"Int","value":8}, "fill_height":{"type":"Float","value":0.0},
                "emission":{"type":"Float","value":0.0}, "cache_mode":{"type":"Enum","value":1},
                "cache_path":{"type":"String","value":directory.to_str().unwrap()}
            }},
            {"id":1,"nodeId":"observe","typeId":"test.fluid_time"},
            {"id":2,"nodeId":"source","typeId":"system.source"},
            {"id":3,"nodeId":"output","typeId":"system.final_output"},
            {"id":4,"nodeId":"input","typeId":"system.generator_input"},
            {"id":5,"nodeId":"appearance","typeId":"node.pbr_material","params":{
                "roughness":{"type":"Float","value":0.1}
            }}
        ],
        "wires": [
            {"fromNode":0,"fromPort":"simulation_time","toNode":1,"toPort":"time"},
            {"fromNode":2,"fromPort":"out","toNode":3,"toPort":"in"}
        ]
    });
    let time = |seconds| FrameTime {
        seconds: Seconds(seconds),
        beats: Beats(seconds * 2.0),
        delta: Seconds::ZERO,
        frame_count: 0,
    };
    let mut recorded = PresetRuntime::from_json_str(&definition.to_string(), &registry).unwrap();
    recorded.execute_frame(time(0.0));
    recorded.execute_frame(time(0.1));
    drop(recorded);
    definition["nodes"][0]["params"]["cache_mode"]["value"] = 2.into();
    definition["nodes"][5]["params"]["roughness"]["value"] = 0.9.into();
    let mut playback = PresetRuntime::from_json_str(&definition.to_string(), &registry).unwrap();
    let state = |runtime: &PresetRuntime| {
        let fluid = runtime
            .graph
            .instance_by_node_id(&NodeId::new("fluid"))
            .unwrap();
        runtime
            .graph
            .get_node(fluid)
            .unwrap()
            .node
            .fluid_domain_snapshot()
            .unwrap()
            .state
    };
    playback.execute_frame(time(0.1));
    assert_eq!(state(&playback), FluidDomainState::Ready);
    let original: EffectGraphDef = serde_json::from_value(definition.clone()).unwrap();
    definition["nodes"][0]["params"]["gravity"] = serde_json::json!({"type":"Float","value":-3.0});
    let changed: EffectGraphDef = serde_json::from_value(definition).unwrap();
    playback.apply_inner_param_overrides(&changed);
    playback.execute_frame(time(0.1));
    assert_eq!(state(&playback), FluidDomainState::Failed);
    playback.apply_inner_param_overrides(&original);
    playback.execute_frame(time(0.1));
    assert_eq!(state(&playback), FluidDomainState::Ready);
    let mut rebuilt = PresetRuntime::from_def(original.clone(), &registry, None).unwrap();
    rebuilt.apply_physics_source_graphs(Err("unresolved source after rebuild".into()));
    rebuilt.carry_generator_state_from(&mut playback);
    rebuilt.execute_frame(time(0.1));
    assert_eq!(state(&rebuilt), FluidDomainState::Failed);
    rebuilt.apply_inner_param_overrides(&original);
    rebuilt.execute_frame(time(0.1));
    assert_eq!(state(&rebuilt), FluidDomainState::Ready);
    drop(rebuilt);
    drop(playback);
    std::fs::remove_dir_all(directory).unwrap();
}
