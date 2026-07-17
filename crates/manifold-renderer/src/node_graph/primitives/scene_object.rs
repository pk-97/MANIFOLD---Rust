//! `node.scene_object` — binds one scene object's mesh, transform, material,
//! maps, and instances into a single [`SceneObject`] wire.
//!
//! Per `docs/SCENE_OBJECT_AND_PANEL_V2_DESIGN.md` D1/D3: an object today is
//! "whatever named group happens to wrap the wires feeding `mesh_k`" — a
//! convention `SceneVm` reverse-engineers. This primitive makes the object a
//! typed graph fact instead: it consumes the same wires `render_scene`'s
//! per-object port family consumed (mesh vertices, transform, material, five
//! maps, instances) and emits ONE [`Object`](crate::node_graph::ports::PortType::Object)
//! wire carrying a [`SceneObject`]. `render_scene` takes `object_k: Object`
//! (P2 of the design) instead of nine parallel per-object wires that had to
//! stay index-coherent by luck.
//!
//! `visible` is port-shadowed exactly like every `node.light` scalar —
//! "mute the statue on the drop" is a MIDI binding, not a feature request.
//! `false` means no draw AND no shadow cast.
//!
//! CPU-only — no GPU dispatch, no `wgsl_body`/`fusion_kind` (same codegen-
//! mandate exemption class as `node.light` / `node.transform_3d`: an IO/CPU
//! bridge, not a per-element GPU atom). Resources referenced through the
//! emitted struct (mesh/map/instance `Slot`s) escape the planner's normal
//! wire-based lifetime view — `carries_resources()` on the `EffectNode`
//! trait is the seam that fixes this (`effect_node.rs`, planner extension in
//! `execution_plan.rs`), the same rule `variadic_skip_passthrough_out`
//! already implements for muxes.

use std::borrow::Cow;

use crate::generators::mesh_common::{InstanceTransform, MeshVertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::scene_object::SceneObject;

crate::primitive! {
    name: SceneObjectNode,
    type_id: "node.scene_object",
    purpose: "Binds one scene object's mesh vertices, transform, material, five maps (base colour / normal / metallic-roughness / occlusion / emissive), and instances into a single Object wire consumed by render_scene's object_k ports. Object wires never chain — this is the sole producer, and it takes no Object input (SCENE_OBJECT_AND_PANEL_V2_DESIGN D1's single-hop invariant). `visible` is port-shadowed so muting the object is a MIDI/LFO binding, not a graph edit; false means no draw AND no shadow cast. CPU-only bridge: no GPU dispatch of its own — mesh/map/instance resources are forwarded as Slots, resolved by the consumer exactly as render_scene resolves them today.",
    inputs: {
        vertices: Array(MeshVertex) optional,
        transform: Transform optional,
        material: Material optional,
        base_color_map: Texture2D optional,
        normal_map: Texture2D optional,
        mr_map: Texture2D optional,
        occlusion_map: Texture2D optional,
        emissive_map: Texture2D optional,
        instances: Array(InstanceTransform) optional,
        visible: ScalarF32 optional,
    },
    outputs: {
        object: Object,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("visible"),
            label: "Visible",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Wire `object` into render_scene's object_k port (replacing the legacy mesh_k/material_k/…/instances_k nine-wire family). Unwired inputs read as the same unresolved/identity defaults their legacy per-object ports did: vertices unwired = no draw (consumer skip, matching render_scene.rs's existing tolerance), transform unwired = identity TRS, material unwired = the consumer's existing structured-error path, maps unwired = no map. `visible` is a [0, 1] threshold (> 0.5 = on) so it can be modulated by an LFO or MIDI, or bound to an eye-toggle in the panel.",
    examples: [],
    picker: { label: "Scene Object", category: Driver },
    summary: "Binds one object's mesh, transform, material, maps, and instances into a single wire. Wire it into a render_scene object slot.",
    category: Geometry3D,
    role: Source,
    aliases: ["scene object", "object", "object bundle"],
    boundary_reason: NonGpu,
}

impl Primitive for SceneObjectNode {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let visible = ctx.scalar_or_param("visible", 1.0) > 0.5;
        let transform = ctx.inputs.transform("transform").unwrap_or_default();
        let material = ctx.inputs.material("material");
        let mesh = ctx.inputs.slot_of("vertices");
        let base_color_map = ctx.inputs.slot_of("base_color_map");
        let normal_map = ctx.inputs.slot_of("normal_map");
        let mr_map = ctx.inputs.slot_of("mr_map");
        let occlusion_map = ctx.inputs.slot_of("occlusion_map");
        let emissive_map = ctx.inputs.slot_of("emissive_map");
        let instances = ctx.inputs.slot_of("instances");

        let object = SceneObject {
            visible,
            transform,
            material,
            mesh,
            base_color_map,
            normal_map,
            mr_map,
            occlusion_map,
            emissive_map,
            instances,
        };

        ctx.outputs.set_object("object", object);
    }

    /// SCENE_OBJECT_AND_PANEL_V2_DESIGN D2: this node forwards mesh/map/
    /// instance `Slot`s inside the emitted `SceneObject` struct — resources
    /// referenced through a struct field are invisible to the planner's
    /// normal wire-based lifetime tracking. `carries_resources` is the seam
    /// that fixes this (see `EffectNode::carries_resources` doc for the
    /// planner-side rule).
    fn carries_resources(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::MockBackend;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
    use crate::node_graph::effect_node::{FrameTime, ParamValues};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::ports::{ArrayType, PortType};
    use crate::node_graph::primitive::PrimitiveSpec;
    use manifold_core::{Beats, Seconds};

    fn frame_time() -> FrameTime {
        FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 }
    }

    #[test]
    fn scene_object_declares_object_output_and_optional_inputs() {
        assert_eq!(SceneObjectNode::TYPE_ID, "node.scene_object");
        for input in SceneObjectNode::INPUTS {
            assert!(!input.required, "{} should be optional", input.name);
        }
        assert_eq!(SceneObjectNode::OUTPUTS.len(), 1);
        assert_eq!(SceneObjectNode::OUTPUTS[0].name, "object");
        assert_eq!(SceneObjectNode::OUTPUTS[0].ty, PortType::Object);
    }

    #[test]
    fn scene_object_declares_no_object_input() {
        // Invariant: `node.scene_object` never takes an `Object` input —
        // the single-hop rule. Also proven registry-wide by
        // `object_port_single_hop` in `validate.rs`.
        assert!(SceneObjectNode::INPUTS.iter().all(|i| i.ty != PortType::Object));
    }

    #[test]
    fn scene_object_carries_resources() {
        let node = SceneObjectNode::new();
        let effect_node: &dyn EffectNode = &node;
        assert!(effect_node.carries_resources());
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SceneObjectNode::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.scene_object");
    }

    #[test]
    fn unwired_object_is_invisible_by_default_flag_but_visible_param_defaults_on() {
        // Fully unwired: visible param defaults to 1.0 (on), transform
        // defaults identity, everything else None.
        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Object, None, (0, 0));

        let mut params = ParamValues::default();
        params.insert(std::borrow::Cow::Borrowed("visible"), ParamValue::Float(1.0));

        let mut prim = SceneObjectNode::new();
        let inputs_bindings: &[(&'static str, Slot)] = &[];
        let outputs_bindings: &[(&'static str, Slot)] = &[("object", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let inputs = NodeInputs::new(inputs_bindings, &backend, &[]);
        let outputs = NodeOutputs::new(
            outputs_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut object_scratch,
        );
        let time = frame_time();
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        Primitive::run(&mut prim, &mut ctx);

        for (slot, value) in object_scratch.drain(..) {
            backend.set_object(slot, value);
        }

        let object = backend.object(out_slot).expect("object should be set");
        assert!(object.visible);
        assert_eq!(object.transform, crate::node_graph::transform::Transform::default());
        assert!(object.material.is_none());
        assert!(object.mesh.is_none());
        assert!(object.instances.is_none());
    }

    #[test]
    fn wired_mesh_and_maps_forward_their_slots_onto_the_object() {
        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Object, None, (0, 0));
        let mesh_slot = backend.acquire(
            ResourceId(1),
            PortType::Array(ArrayType::of_known::<MeshVertex>()),
            None,
            (0, 0),
        );
        let color_slot = backend.acquire(ResourceId(2), PortType::Texture2D, None, (0, 0));
        let instances_slot = backend.acquire(
            ResourceId(3),
            PortType::Array(ArrayType::of_known::<InstanceTransform>()),
            None,
            (0, 0),
        );

        let mut params = ParamValues::default();
        params.insert(std::borrow::Cow::Borrowed("visible"), ParamValue::Float(0.0));

        let mut prim = SceneObjectNode::new();
        let inputs_bindings: &[(&'static str, Slot)] = &[
            ("vertices", mesh_slot),
            ("base_color_map", color_slot),
            ("instances", instances_slot),
        ];
        let outputs_bindings: &[(&'static str, Slot)] = &[("object", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let inputs = NodeInputs::new(inputs_bindings, &backend, &[]);
        let outputs = NodeOutputs::new(
            outputs_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut object_scratch,
        );
        let time = frame_time();
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        Primitive::run(&mut prim, &mut ctx);

        for (slot, value) in object_scratch.drain(..) {
            backend.set_object(slot, value);
        }

        let object = backend.object(out_slot).expect("object should be set");
        assert!(!object.visible, "visible param was set to 0.0");
        assert_eq!(object.mesh, Some(mesh_slot));
        assert_eq!(object.base_color_map, Some(color_slot));
        assert_eq!(object.instances, Some(instances_slot));
        assert_eq!(object.normal_map, None);
    }
}

/// Real-GPU end-to-end wiring proof: `mesh_src → node.scene_object →
/// consumer` through the real [`Executor`](crate::node_graph::execution::Executor) +
/// [`MetalBackend`] path, per the P1 gate ("synthetic def wiring
/// mesh→scene_object→(test consumer via `inputs.object`), asserts the
/// struct arrives with correct slots and the mesh resolves"). No `wgsl_body`
/// exists to parity-test here (CPU-only bridge, per the module doc) — the
/// thing under test is the wiring itself: does a real `Array<MeshVertex>`
/// buffer, pre-bound to `mesh_src`'s output resource, actually arrive at the
/// consumer through the `SceneObject`'s forwarded `Slot`.
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use std::sync::{Arc, Mutex};

    use crate::node_graph::EffectNode;
    use crate::node_graph::MetalBackend;
    use crate::node_graph::compile;
    use crate::node_graph::effect_node::{EffectNodeContext, EffectNodeType, FrameTime};
    use crate::node_graph::execution::Executor;
    use crate::node_graph::graph::Graph;
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType};
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::*;

    fn frame_time() -> FrameTime {
        FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 }
    }

    fn mk_vertex(pos: [f32; 3]) -> MeshVertex {
        MeshVertex { position: pos, _pad0: 0.0, normal: [0.0, 1.0, 0.0], _pad1: 0.0, uv: [0.0, 0.0], _pad2: [0.0, 0.0] }
    }

    /// Test-only mesh producer: its `out` resource is pre-bound directly to
    /// a real GPU buffer before the frame runs (mirroring
    /// `bindings.rs::array_accessor_tests`), so `evaluate` itself is a
    /// no-op — the test is about the WIRING downstream of the pre-bound
    /// resource, not about dispatching a mesh-generation kernel.
    struct MeshSourceNode {
        type_id: EffectNodeType,
    }

    impl EffectNode for MeshSourceNode {
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &[]
        }
        fn outputs(&self) -> &[NodeOutput] {
            static OUTPUTS: [NodePort; 1] = [NodePort {
                name: Cow::Borrowed("out"),
                ty: PortType::Array(ArrayType::of_known::<MeshVertex>()),
                kind: PortKind::Output,
                required: false,
            }];
            &OUTPUTS
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
    }

    /// Test consumer: declares an `Object` input (standing in for
    /// `render_scene`'s future `object_k` port), records the resolved
    /// [`SceneObject`] plus the mesh vertices read back through its
    /// forwarded `Slot`.
    struct ObjectConsumerNode {
        type_id: EffectNodeType,
        seen: Arc<Mutex<Option<SceneObject>>>,
        resolved_mesh: Arc<Mutex<Option<Vec<[f32; 3]>>>>,
    }

    impl EffectNode for ObjectConsumerNode {
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodePort; 1] = [NodePort {
                name: Cow::Borrowed("in"),
                ty: PortType::Object,
                kind: PortKind::Input,
                required: false,
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
            let Some(object) = ctx.inputs.object("in") else { return };
            *self.seen.lock().unwrap() = Some(object);
            if let Some(mesh_slot) = object.mesh
                && let Some(buf) = ctx.inputs.array_slot(mesh_slot)
            {
                let ptr = buf.mapped_ptr().expect("shared mesh buffer") as *const MeshVertex;
                let count = buf.size as usize / std::mem::size_of::<MeshVertex>();
                let verts = unsafe { std::slice::from_raw_parts(ptr, count) };
                *self.resolved_mesh.lock().unwrap() =
                    Some(verts.iter().map(|v| v.position).collect());
            }
        }
    }

    #[test]
    fn object_wire_carries_real_mesh_slot_end_to_end() {
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let mesh_src = g.add_node(Box::new(MeshSourceNode { type_id: EffectNodeType::new("mesh_src") }));
        let scene_object = g.add_node(Box::new(SceneObjectNode::new()));
        let seen = Arc::new(Mutex::new(None));
        let resolved_mesh = Arc::new(Mutex::new(None));
        let consumer = g.add_node(Box::new(ObjectConsumerNode {
            type_id: EffectNodeType::new("object_consumer"),
            seen: seen.clone(),
            resolved_mesh: resolved_mesh.clone(),
        }));
        g.connect((mesh_src, "out"), (scene_object, "vertices")).unwrap();
        g.connect((scene_object, "object"), (consumer, "in")).unwrap();

        let plan = compile(&g).unwrap();
        let r_mesh_out = plan
            .steps()
            .iter()
            .find(|s| s.node == mesh_src)
            .and_then(|s| s.outputs.iter().find(|(n, _)| *n == "out"))
            .map(|&(_, r)| r)
            .expect("mesh_src's out resource is bound (scene_object reads it)");

        let expected = vec![
            mk_vertex([1.0, 2.0, 3.0]),
            mk_vertex([-1.0, 0.5, 4.0]),
            mk_vertex([0.0, 0.0, 0.0]),
        ];
        let buffer = device.create_buffer_shared(std::mem::size_of_val(expected.as_slice()) as u64);
        unsafe {
            buffer.write(0, bytemuck::cast_slice(&expected));
        }

        let mut backend = MetalBackend::new(device.arc(), 16, 16, format);
        backend.pre_bind_array(r_mesh_out, buffer);

        let mut exec = Executor::new(Box::new(backend));
        exec.execute_frame(&mut g, &plan, frame_time());

        let object = seen.lock().unwrap().expect("consumer should have seen a SceneObject");
        assert!(object.mesh.is_some(), "SceneObject must carry the forwarded mesh Slot");
        assert!(object.visible, "visible defaults to on (param default 1.0)");

        let resolved = resolved_mesh.lock().unwrap().clone().expect("mesh must resolve through the forwarded slot");
        let expected_positions: Vec<[f32; 3]> = expected.iter().map(|v| v.position).collect();
        assert_eq!(resolved, expected_positions, "mesh vertices must round-trip through the Object wire's forwarded Slot");
    }
}
