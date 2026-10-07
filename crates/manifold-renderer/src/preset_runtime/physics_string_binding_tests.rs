use super::*;
use crate::node_graph::ParamType;
use crate::node_graph::fluid::FluidDomainState;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

struct StringGravity(EffectNodeType);

impl EffectNode for StringGravity {
    fn is_pure(&self) -> bool {
        true
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
    fn inputs(&self) -> &[NodeInput] {
        &[]
    }
    fn outputs(&self) -> &[NodeOutput] {
        static OUTPUTS: [NodeOutput; 1] = [NodePort {
            name: Cow::Borrowed("value"),
            ty: PortType::Scalar(ScalarType::F32),
            kind: PortKind::Output,
            required: false,
        }];
        &OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        static PARAMETERS: LazyLock<[ParamDef; 1]> = LazyLock::new(|| {
            [ParamDef {
                name: Cow::Borrowed("mode"),
                label: "Mode",
                ty: ParamType::String,
                default: ParamValue::String(Arc::new("primitive-default".into())),
                range: None,
                enum_values: &[],
            }]
        });
        &*PARAMETERS
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let gravity = match ctx.params.get("mode") {
            Some(ParamValue::String(value)) if value.as_str() == "take-a" => -1.0,
            Some(ParamValue::String(value)) if value.as_str() == "take-b" => -2.0,
            _ => -3.0,
        };
        ctx.outputs.set_scalar("value", ParamValue::Float(gravity));
    }
}

fn definition(directory: &Path, mode: u32) -> EffectGraphDef {
    let mut def = serde_json::to_value(runtime_definition()).unwrap();
    def["nodes"][0]["params"]["cache_mode"] = serde_json::json!({"type":"Enum","value":mode});
    def["nodes"][0]["params"]["cache_path"] =
        serde_json::json!({"type":"String","value":directory.to_str().unwrap()});
    for (id, name) in [(5, "gravity_mode"), (6, "appearance")] {
        def["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id":id, "nodeId":name, "typeId":"test.string_gravity",
                "params":{"mode":{"type":"String","value":"authored"}}
            }));
    }
    def["wires"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "fromNode":5,"fromPort":"value","toNode":0,"toPort":"gravity"
        }));
    def["presetMetadata"] = serde_json::json!({
        "id":"string-gravity","displayName":"String Gravity","category":"Diagnostic",
        "oscPrefix":"physics", "params":[],"bindings":[],
        "stringParams":[
            {"id":"mode","name":"Mode","defaultValue":"binding-default"},
            {"id":"look","name":"Look","defaultValue":"binding-default"}
        ],
        "stringBindings":[
            {"id":"mode","label":"Mode","defaultValue":"binding-default","target":{"kind":"node","nodeId":"gravity_mode","param":"mode"}},
            {"id":"look","label":"Look","defaultValue":"binding-default","target":{"kind":"node","nodeId":"appearance","param":"mode"}}
        ]
    });
    serde_json::from_value(def).unwrap()
}

fn build(def: EffectGraphDef) -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_cpu_flip_reference();
    registry.register("test.fluid_time", || {
        fluid_time_observer()
    });
    registry.register("test.string_gravity", || {
        Box::new(StringGravity(EffectNodeType::new("test.string_gravity")))
    });
    PresetRuntime::from_def(def, &registry, None).unwrap()
}

fn time(seconds: f64) -> FrameTime {
    FrameTime {
        seconds: Seconds(seconds),
        beats: Beats(seconds * 2.0),
        delta: Seconds::ZERO,
        frame_count: 0,
    }
}

fn state(runtime: &PresetRuntime) -> FluidDomainState {
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
}

fn strings(mode: &str, look: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("mode".into(), mode.into()), ("look".into(), look.into())])
}

fn current_mode(runtime: &PresetRuntime) -> Arc<String> {
    let node = runtime
        .graph
        .instance_by_node_id(&NodeId::new("gravity_mode"))
        .unwrap();
    let ParamValue::String(mode) = &runtime.graph.get_node(node).unwrap().params["mode"] else {
        panic!("string mode expected")
    };
    mode.clone()
}

fn directory(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "manifold-fluid-strings-{label}-{}",
        std::process::id()
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

#[test]
fn native_take_checks_applied_string_overrides_and_recovers_on_undo_and_rebuild() {
    let directory = directory("override");
    let mut recorded = build(definition(&directory, 1));
    assert_eq!(
        current_mode(&recorded).as_str(),
        "authored",
        "def value wins over binding default"
    );
    recorded.set_string_params(Some(&strings("take-a", "blue")));
    let original_arc = current_mode(&recorded);
    recorded.set_string_params(Some(&strings("take-a", "blue")));
    assert!(
        Arc::ptr_eq(&original_arc, &current_mode(&recorded)),
        "repeated values reuse storage"
    );
    recorded.execute_frame(time(0.0));
    recorded.execute_frame(time(0.1));
    assert_eq!(state(&recorded), FluidDomainState::Ready);
    drop(recorded);

    let mut playback = build(definition(&directory, 2));
    playback.execute_frame(time(0.1));
    assert_eq!(
        state(&playback),
        FluidDomainState::Failed,
        "authored default differs from recorded host override"
    );
    playback.set_string_params(Some(&strings("take-a", "red")));
    playback.execute_frame(time(0.1));
    assert_eq!(
        state(&playback),
        FluidDomainState::Ready,
        "unrelated appearance may differ"
    );
    playback.set_string_params(None);
    playback.execute_frame(time(0.1));
    assert_eq!(
        state(&playback),
        FluidDomainState::Ready,
        "absent host values retain accepted strings"
    );
    playback.set_string_params(Some(&strings("take-b", "red")));
    playback.execute_frame(time(0.1));
    assert_eq!(state(&playback), FluidDomainState::Failed);
    playback.set_string_params(Some(&strings("take-a", "green")));
    playback.execute_frame(time(0.1));
    assert_eq!(state(&playback), FluidDomainState::Ready);

    let mut rebuilt = build(definition(&directory, 2));
    rebuilt.set_string_params(Some(&strings("take-b", "green")));
    rebuilt.carry_generator_state_from(&mut playback);
    rebuilt.execute_frame(time(0.1));
    assert_eq!(
        state(&rebuilt),
        FluidDomainState::Failed,
        "carried native state cannot hide a new string selection"
    );
    rebuilt.set_string_params(Some(&strings("take-a", "green")));
    rebuilt.execute_frame(time(0.1));
    assert_eq!(state(&rebuilt), FluidDomainState::Ready);
    drop(rebuilt);
    drop(playback);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn native_take_commits_a_string_edit_at_held_project_time() {
    let directory = directory("held");
    let mut recorded = build(definition(&directory, 1));
    recorded.set_string_params(Some(&strings("take-a", "blue")));
    recorded.execute_frame(time(0.0));
    recorded.execute_frame(time(0.1));
    recorded.set_string_params(Some(&strings("take-b", "blue")));
    recorded.execute_frame(time(0.1));
    assert_eq!(state(&recorded), FluidDomainState::Ready);
    drop(recorded);

    let mut playback = build(definition(&directory, 2));
    playback.set_string_params(Some(&strings("take-b", "blue")));
    playback.execute_frame(time(0.1));
    assert_eq!(state(&playback), FluidDomainState::Ready);
    playback.set_string_params(Some(&strings("take-a", "blue")));
    playback.execute_frame(time(0.1));
    assert_eq!(
        state(&playback),
        FluidDomainState::Failed,
        "last committed string selection is authoritative"
    );
    drop(playback);
    std::fs::remove_dir_all(directory).unwrap();
}
