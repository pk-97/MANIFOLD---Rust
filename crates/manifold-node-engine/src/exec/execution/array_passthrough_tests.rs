//! The executor's array passthrough: a switch whose chosen input is wired
//! aliases that input onto its output instead of running, and runs when the
//! plan refuses the alias.
use std::sync::{Arc, Mutex};

use super::*;
use crate::exec::backend::MockBackend;
use crate::exec::effect_node::{EffectNode, EffectNodeType, ParamValues};
use crate::exec::execution_plan::compile;
use crate::parameters::ParamDef;
use crate::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType};

fn array() -> PortType {
    PortType::Array(ArrayType::of::<u32>())
}

fn port(name: &'static str, kind: PortKind) -> NodePort {
    NodePort {
        name: std::borrow::Cow::Borrowed(name),
        ty: array(),
        kind,
        required: false,
    }
}

/// A two-way array switch that passes `in_0` through, counting the frames it
/// actually runs.
struct Switch {
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
    runs: Arc<Mutex<u32>>,
}

impl EffectNode for Switch {
    fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule {
        crate::scene::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &EffectNodeType {
        static ID: std::sync::OnceLock<EffectNodeType> = std::sync::OnceLock::new();
        ID.get_or_init(|| EffectNodeType::new("test.array_switch"))
    }
    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }
    fn outputs(&self) -> &[NodeOutput] {
        &self.outputs
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {
        *self.runs.lock().unwrap() += 1;
    }
    fn skip_passthrough(
        &self,
        _: &ParamValues,
        wired: &[&str],
    ) -> Option<(&'static str, &'static str)> {
        wired.contains(&"in_0").then_some(("in_0", "out"))
    }
    fn variadic_skip_passthrough_out(&self) -> Option<&'static str> {
        Some("out")
    }
}

/// A producer, a reader that may write its input in place, or a root sink.
struct Stage {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
    in_place: bool,
    root: bool,
}

impl Stage {
    fn new(name: &str, input: bool, output: bool) -> Self {
        Self {
            type_id: EffectNodeType::new(name),
            inputs: if input {
                vec![port("in", PortKind::Input)]
            } else {
                Vec::new()
            },
            outputs: if output {
                vec![port("out", PortKind::Output)]
            } else {
                Vec::new()
            },
            in_place: false,
            root: !output,
        }
    }
}

impl EffectNode for Stage {
    fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule {
        crate::scene::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }
    fn outputs(&self) -> &[NodeOutput] {
        &self.outputs
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
    fn aliased_array_io(&self) -> &[(&str, &str)] {
        if self.in_place { &[("in", "out")] } else { &[] }
    }
    fn is_liveness_root(&self) -> bool {
        self.root
    }
}

/// producer -> switch -> reader -> sink, three frames. Returns how often the
/// switch ran and the array aliases the backend held after the last frame.
fn run(in_place_reader: bool) -> (u32, Vec<(Slot, Slot)>) {
    let runs = Arc::new(Mutex::new(0));
    let mut graph = Graph::new();
    let producer = graph.add_node(Box::new(Stage::new("test.producer", false, true)));
    let switch = graph.add_node(Box::new(Switch {
        inputs: vec![port("in_0", PortKind::Input), port("in_1", PortKind::Input)],
        outputs: vec![port("out", PortKind::Output)],
        runs: runs.clone(),
    }));
    let mut reader = Stage::new("test.reader", true, true);
    reader.in_place = in_place_reader;
    let reader = graph.add_node(Box::new(reader));
    let sink = graph.add_node(Box::new(Stage::new("test.sink", true, false)));
    graph.connect((producer, "out"), (switch, "in_0")).unwrap();
    graph.connect((switch, "out"), (reader, "in")).unwrap();
    graph.connect((reader, "out"), (sink, "in")).unwrap();
    let plan = compile(&graph).unwrap();
    let mut executor = Executor::with_mock();
    let time = FrameTime {
        beats: manifold_core::Beats(0.0),
        seconds: manifold_core::Seconds(0.0),
        delta: manifold_core::Seconds(1.0 / 60.0),
        frame_count: 0,
    };
    for _ in 0..3 {
        executor.execute_frame(&mut graph, &plan, time);
    }
    let mock = executor
        .backend()
        .as_any()
        .and_then(|any| any.downcast_ref::<MockBackend>())
        .expect("mock backend");
    (*runs.lock().unwrap(), mock.array_aliases().to_vec())
}

#[test]
fn chosen_wired_input_is_aliased_onto_the_output_and_the_switch_never_runs() {
    let (runs, aliases) = run(false);
    assert_eq!(runs, 0, "every frame passes through by alias");
    assert_eq!(aliases.len(), 1, "one alias this frame: {aliases:?}");
    assert_ne!(aliases[0].0, aliases[0].1, "the output reads another slot");
}

#[test]
fn an_in_place_reader_of_the_output_refuses_the_alias_and_the_switch_runs() {
    let (runs, aliases) = run(true);
    assert_eq!(runs, 3);
    assert!(aliases.is_empty());
}
