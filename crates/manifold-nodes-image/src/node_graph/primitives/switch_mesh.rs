//! `node.switch_mesh` — two-way `Array<MeshVertex>` selector that passes the
//! chosen input through. `node.switch_indices` (`Array<u32>`) and `node.switch_instances`
//! (`Array<InstanceTransform>`) are its twins and share [`ArraySwitch`].
//!
//! A selector param (not a wire) names the chosen input before the frame
//! runs, so the executor skips everything that feeds only the other input and
//! aliases the chosen one onto `out`: readers of `out` read the input's own
//! buffer and live extent, with no copy and no GPU work, so a scene object
//! draws exactly what the direct wire would have drawn. The planner keeps the
//! input's storage alive and unshared while `out` is read, and refuses the
//! alias when an in-place step would write through it.
//!
//! [`ArraySwitch`] runs only when there is nothing to alias: an unwired choice
//! publishes a live extent of zero (clearing `out`'s own buffer once, when it
//! is new), and a wired selector or a refused alias blits the chosen live
//! bytes on the frame's encoder.

use std::borrow::Cow;

use manifold_gpu::GpuBuffer;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::live_extent::LiveExtent;

pub(super) const SWITCH_INPUTS: [&str; 2] = ["in_0", "in_1"];

manifold_node_engine::primitive! {
    name: SwitchMesh,
    type_id: "node.switch_mesh",
    purpose: "Two-way Array<MeshVertex> selector. Passes in_0 or in_1 through to out with the chosen input's live extent, chosen by the selector (rounded, clamped). With the selector set as a param, out is the chosen input's own buffer for the frame (no copy, no GPU work) and everything feeding only the other input is skipped; a wired selector blits the chosen live vertices on the GPU. Output capacity = the larger wired input.",
    inputs: {
        selector: ScalarF32 required,
        in_0: Array(MeshVertex) optional,
        in_1: Array(MeshVertex) optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("selector"),
            label: "Selector",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Use it to swap what a node.scene_object draws without paying for the branch it is not drawing: wire the expensive mesh into one input and the cheap one into the other, and set the selector as a param (port-shadowed: a wired selector keeps both branches running). An indexed mesh needs node.switch_indices on its indices wire with the same selector. The output carries the chosen input's live extent, so the object draws exactly the triangles the direct wire would. A chosen input that is unwired draws nothing (live extent zero).",
    examples: [],
    picker: { label: "Switch (mesh)", category: Atom },
    summary: "Picks one of two incoming meshes and passes it through, chosen by a selector number. The mesh it does not pick is not computed.",
    category: Routing,
    role: Filter,
    aliases: ["switch", "mux", "mesh switch", "selector"],
    boundary_reason: NonGpu,
    extra_fields: {
        switch: ArraySwitch = ArraySwitch::default(),
    },
}

manifold_node_engine::param_tooltips!("node.switch_mesh", {
    "selector" => "Picks which mesh to pass through: 0 or 1.",
});

impl Primitive for SwitchMesh {
    fn selected_input_branch(
        &self,
        params: &ParamValues,
        wired_inputs: &[&str],
    ) -> Option<&'static str> {
        selected_switch_input(params, wired_inputs)
    }

    fn skip_passthrough(
        &self,
        params: &ParamValues,
        wired_inputs: &[&str],
    ) -> Option<(&'static str, &'static str)> {
        switch_passthrough(params, wired_inputs)
    }

    fn variadic_skip_passthrough_out(&self) -> Option<&'static str> {
        Some("out")
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        switch_output_capacity(port_name, input_capacities)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.switch.run(
            ctx,
            std::mem::size_of::<MeshVertex>() as u64,
            "node.switch_mesh",
        );
    }
}

fn switch_port(selector: f32) -> &'static str {
    SWITCH_INPUTS[selector
        .round()
        .clamp(0.0, (SWITCH_INPUTS.len() - 1) as f32) as usize]
}

/// The chosen input when the selector is a param. A wired selector can
/// change mid-frame, so every input stays live.
pub(super) fn selected_switch_input(
    params: &ParamValues,
    wired_inputs: &[&str],
) -> Option<&'static str> {
    if wired_inputs.contains(&"selector") {
        return None;
    }
    let selector = params
        .get("selector")
        .and_then(|v| v.as_scalar())
        .unwrap_or(0.0);
    Some(switch_port(selector))
}

/// With a param selector, a wired choice passes through by alias: the executor
/// points `out` at the chosen input's buffer and live extent for the frame
/// and `run` does not execute. An unwired choice runs, to publish extent zero.
pub(super) fn switch_passthrough(
    params: &ParamValues,
    wired_inputs: &[&str],
) -> Option<(&'static str, &'static str)> {
    let port = selected_switch_input(params, wired_inputs)?;
    wired_inputs.contains(&port).then_some((port, "out"))
}

/// `out` holds the larger wired input, so either choice fits.
pub(super) fn switch_output_capacity(
    port_name: &str,
    input_capacities: &[(&str, u32)],
) -> Option<u32> {
    if port_name != "out" {
        return None;
    }
    let max = input_capacities
        .iter()
        .filter(|(port, _)| SWITCH_INPUTS.contains(port))
        .map(|&(_, n)| n)
        .max()
        .unwrap_or(1);
    Some(max.max(1))
}

/// The run path shared by the array switches, for frames the executor does not
/// alias: the unwired choice, a wired selector, or a refused alias.
#[derive(Default)]
pub struct ArraySwitch {
    /// Identity of the output buffer `dirty` describes.
    target: usize,
    /// Bytes of `out` past which every byte is zero.
    dirty: u64,
    /// A count word of zero: the extent of an unwired choice.
    zero_counts: Option<GpuBuffer>,
    /// A count word holding the item count of a chosen input that publishes
    /// no extent of its own. Never rewritten: a new count is a new buffer, so
    /// frames still in flight keep reading theirs.
    fixed_counts: Option<(u32, GpuBuffer)>,
}

impl ArraySwitch {
    pub(super) fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>, item: u64, label: &str) {
        let Some(dst) = ctx.outputs.array("out") else {
            log::warn!("{label}: no GpuBuffer bound to output port `out`");
            return;
        };
        let selector = ctx.scalar_or_param("selector", 0.0);
        let port = switch_port(selector);
        let src = ctx.inputs.array(port);
        let input_extent = src.and(ctx.inputs.live_extent(port));
        let out_items = dst.size / item;
        if dst.identity_key() != self.target {
            // A new buffer holds unknown bytes.
            self.target = dst.identity_key();
            self.dirty = dst.size;
        }

        let gpu = ctx.gpu_encoder();
        // Live bytes to copy, and whether the source is zero past them up to
        // its own size (the live-extent contract), so a longer copy clears
        // what an earlier frame left in `out`.
        let (live, extent, zero_tail) = match (src, input_extent) {
            (None, _) => {
                let counts = self.zero_counts.get_or_insert_with(|| {
                    let buffer = gpu.device.create_buffer_shared(4);
                    buffer.zero_fill();
                    buffer
                });
                (
                    0,
                    LiveExtent {
                        counts: counts.clone(),
                        offset: 0,
                        per_item: 1,
                        bound: 0,
                    },
                    false,
                )
            }
            (Some(src), Some(extent)) => {
                let bound = u64::from(extent.bound).min(src.size / item).min(out_items) as u32;
                (
                    u64::from(bound) * item,
                    LiveExtent { bound, ..extent },
                    true,
                )
            }
            (Some(src), None) => {
                let items = (src.size / item).min(out_items) as u32;
                if self.fixed_counts.as_ref().is_none_or(|(n, _)| *n != items) {
                    let buffer = gpu.device.create_buffer_shared(4);
                    // SAFETY: fresh 4-byte shared buffer; no GPU work has seen it.
                    unsafe { buffer.write(0, bytemuck::bytes_of(&items)) };
                    self.fixed_counts = Some((items, buffer));
                }
                let counts = self.fixed_counts.as_ref().expect("set above").1.clone();
                (
                    u64::from(items) * item,
                    LiveExtent {
                        counts,
                        offset: 0,
                        per_item: 1,
                        bound: items,
                    },
                    false,
                )
            }
        };

        let mut copy = live;
        if self.dirty > live {
            match src {
                Some(src) if zero_tail && src.size >= self.dirty => copy = self.dirty,
                _ => gpu.native_enc.clear_buffer(dst),
            }
        }
        if let Some(src) = src
            && copy > 0
        {
            gpu.native_enc.copy_buffer_range(src, 0, dst, 0, copy);
        }
        self.dirty = live;
        ctx.outputs.set_live_extent("out", extent);
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(super) mod extent;

#[cfg(test)]
pub(super) mod tests {
    use std::sync::{Arc, Mutex};

    use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeType, FrameTime};
    use manifold_node_engine::exec::execution::Executor;
    use manifold_node_engine::exec::execution_plan::compile;
    use manifold_node_engine::graph::Graph;
    use manifold_node_engine::ports::{
        ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType,
    };

    use super::*;

    #[test]
    fn param_selector_picks_a_rounded_clamped_input() {
        let node = SwitchMesh::new();
        for (selector, port) in [
            (0.0, "in_0"),
            (0.4, "in_0"),
            (0.6, "in_1"),
            (1.0, "in_1"),
            (7.0, "in_1"),
            (-3.0, "in_0"),
        ] {
            let mut params = ParamValues::default();
            params.insert(Cow::Borrowed("selector"), ParamValue::Float(selector));
            assert_eq!(
                Primitive::selected_input_branch(&node, &params, &["in_0", "in_1"]),
                Some(port),
                "{selector}"
            );
        }
    }

    /// A param selector passes a wired choice through by alias; an unwired
    /// choice or a wired selector runs instead.
    #[test]
    fn param_selector_on_a_wired_choice_passes_through() {
        let node = SwitchMesh::new();
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("selector"), ParamValue::Float(1.0));
        assert_eq!(
            Primitive::skip_passthrough(&node, &params, &["in_0", "in_1"]),
            Some(("in_1", "out"))
        );
        assert_eq!(Primitive::skip_passthrough(&node, &params, &["in_0"]), None);
        assert_eq!(
            Primitive::skip_passthrough(&node, &params, &["selector", "in_0", "in_1"]),
            None
        );
        assert_eq!(Primitive::variadic_skip_passthrough_out(&node), Some("out"));
    }

    #[test]
    fn wired_selector_keeps_every_branch() {
        let node = SwitchMesh::new();
        let params = ParamValues::default();
        assert_eq!(
            Primitive::selected_input_branch(&node, &params, &["selector", "in_0", "in_1"]),
            None
        );
    }

    #[test]
    fn output_capacity_is_the_larger_wired_input() {
        let node = SwitchMesh::new();
        let params = ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&node, "out", &params, &[("in_0", 5), ("in_1", 8)]),
            Some(8)
        );
        assert_eq!(
            Primitive::array_output_capacity(&node, "out", &params, &[("in_1", 3)]),
            Some(3)
        );
        assert_eq!(
            Primitive::array_output_capacity(&node, "out", &params, &[]),
            Some(1)
        );
    }

    /// Records each evaluation by name.
    pub(in super::super) struct LoggedArrayNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        root: bool,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl LoggedArrayNode {
        fn port(name: &'static str, ty: PortType, kind: PortKind) -> NodePort {
            NodePort {
                name: Cow::Borrowed(name),
                ty,
                kind,
                required: false,
            }
        }

        pub(in super::super) fn producer(
            name: &str,
            ty: PortType,
            log: &Arc<Mutex<Vec<String>>>,
        ) -> Self {
            Self {
                type_id: EffectNodeType::new(name),
                inputs: Vec::new(),
                outputs: vec![Self::port("out", ty, PortKind::Output)],
                root: false,
                log: log.clone(),
            }
        }

        pub(in super::super) fn sink(ty: PortType, log: &Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                type_id: EffectNodeType::new("sink"),
                inputs: vec![Self::port("in", ty, PortKind::Input)],
                outputs: Vec::new(),
                root: true,
                log: log.clone(),
            }
        }
    }

    impl EffectNode for LoggedArrayNode {
        fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
            manifold_node_engine::scene::depth_rule::DepthRule::Terminal
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
        fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {
            self.log
                .lock()
                .unwrap()
                .push(self.type_id.as_str().to_string());
        }
        fn is_liveness_root(&self) -> bool {
            self.root
        }
    }

    /// Runs one frame of `a → in_0, b → in_1, switch → sink` with the
    /// selector param set, and returns which producers ran.
    pub(in super::super) fn producers_run(
        switch: Box<dyn EffectNode>,
        ty: PortType,
        selector: f32,
    ) -> Vec<String> {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut graph = Graph::new();
        let a = graph.add_node(Box::new(LoggedArrayNode::producer("a", ty, &log)));
        let b = graph.add_node(Box::new(LoggedArrayNode::producer("b", ty, &log)));
        let switch = graph.add_node(switch);
        graph
            .set_param(switch, "selector", ParamValue::Float(selector))
            .unwrap();
        let sink = graph.add_node(Box::new(LoggedArrayNode::sink(ty, &log)));
        graph.connect((a, "out"), (switch, "in_0")).unwrap();
        graph.connect((b, "out"), (switch, "in_1")).unwrap();
        graph.connect((switch, "out"), (sink, "in")).unwrap();
        let plan = compile(&graph).unwrap();
        let time = FrameTime {
            beats: manifold_core::Beats(0.0),
            seconds: manifold_core::Seconds(0.0),
            delta: manifold_core::Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        Executor::with_mock().execute_frame(&mut graph, &plan, time);
        let ran = log.lock().unwrap().clone();
        assert!(
            ran.contains(&"sink".to_string()),
            "the sink is a liveness root: {ran:?}"
        );
        ran.into_iter()
            .filter(|name| name == "a" || name == "b")
            .collect()
    }

    #[test]
    fn param_selector_skips_the_producer_of_the_other_input() {
        let mesh = PortType::Array(ArrayType::of_known::<MeshVertex>());
        assert_eq!(producers_run(Box::new(SwitchMesh::new()), mesh, 1.0), ["b"]);
        assert_eq!(producers_run(Box::new(SwitchMesh::new()), mesh, 0.0), ["a"]);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
pub(super) mod gpu_tests;
