//! `node.transform_components` — the nine scalars of a `Transform` wire, the
//! inverse of `node.transform_3d`. Lets GPU atoms that take scalar params
//! (lattice bounds, a collider's position) read a Transform-typed wire:
//! codegen buffer kernels bind only params and arrays, and a Transform wire
//! into a GPU atom is a fusion cut. CPU-only.

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::primitive::Primitive;

manifold_node_engine::primitive! {
    name: TransformComponents,
    type_id: "node.transform_components",
    purpose: "Split a Transform into its nine scalars: position X/Y/Z, rotation X/Y/Z (radians, XYZ Euler) and scale X/Y/Z. The inverse of node.transform_3d, with the same port names. An unwired input publishes the identity transform.",
    inputs: {
        transform: Transform optional,
    },
    outputs: {
        pos_x: ScalarF32, pos_y: ScalarF32, pos_z: ScalarF32,
        rot_x: ScalarF32, rot_y: ScalarF32, rot_z: ScalarF32,
        scale_x: ScalarF32, scale_y: ScalarF32, scale_z: ScalarF32,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Use it where a GPU atom needs a box or pose as scalar params: a fluid lattice's grid_bounds (position = centre, scale = full size) into the surface atoms' center_x/y/z and size_x/y/z, or an object's position into a field. Wire only the components you need.",
    examples: [],
    picker: { label: "Transform Components", category: Driver },
    summary: "Breaks a transform into its separate position, rotation and scale numbers so each can drive something else.",
    category: MathAndConvert,
    role: Map,
    aliases: ["split transform", "transform components", "decompose transform", "position of"],
    boundary_reason: NonGpu,
}

impl Primitive for TransformComponents {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let transform = ctx.inputs.transform("transform").unwrap_or_default();
        let names = [
            ["pos_x", "pos_y", "pos_z"],
            ["rot_x", "rot_y", "rot_z"],
            ["scale_x", "scale_y", "scale_z"],
        ];
        for (ports, values) in names.iter().zip([transform.pos, transform.rot_euler, transform.scale]) {
            for (port, value) in ports.iter().zip(values) {
                ctx.outputs.set_scalar(port, ParamValue::Float(value));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::exec::backend::Backend;
    use manifold_node_engine::bindings::{NodeInputs, NodeOutputs};
    use manifold_node_engine::exec::effect_node::{FrameTime, ParamValues};
    use manifold_node_engine::scene::transform::Transform;
    use manifold_node_engine::{exec::backend::MockBackend, ports::PortType, exec::execution_plan::ResourceId, ports::ScalarType};
    use manifold_core::{Beats, Seconds};

    fn run(input: Option<Transform>) -> Vec<(&'static str, f32)> {
        let mut backend = MockBackend::new();
        let mut input_bindings = Vec::new();
        if let Some(value) = input {
            let slot = backend.acquire(ResourceId(0), PortType::Transform, None, (0, 0));
            backend.set_transform(slot, value);
            input_bindings.push(("transform", slot));
        }
        let names = [
            "pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "scale_x", "scale_y", "scale_z",
        ];
        let output_bindings: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(index, &name)| {
                let slot = backend.acquire(
                    ResourceId(1 + index as u32),
                    PortType::Scalar(ScalarType::F32),
                    None,
                    (0, 0),
                );
                (name, slot)
            })
            .collect();
        let inputs = NodeInputs::new(&input_bindings, &backend, &[]);
        let (mut scalar, mut camera, mut light, mut material) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut transform, mut atmosphere, mut render_mode, mut object) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let outputs = NodeOutputs::new(
            &output_bindings,
            &backend,
            &mut scalar,
            &mut camera,
            &mut light,
            &mut material,
            &mut transform,
            &mut atmosphere,
            &mut render_mode,
            &mut object,
        );
        let params = ParamValues::default();
        let time = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        {
            let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
            Primitive::run(&mut TransformComponents::new(), &mut ctx);
        }
        scalar
            .iter()
            .map(|&(slot, ref value)| {
                let name = output_bindings.iter().find(|(_, s)| *s == slot).unwrap().0;
                let ParamValue::Float(value) = value else { panic!("scalar outputs are floats") };
                (name, *value)
            })
            .collect()
    }

    #[test]
    fn transform_components_split_every_axis_and_default_to_identity() {
        let values = run(Some(Transform {
            pos: [1.0, -2.0, 3.5],
            rot_euler: [0.25, 0.5, -0.75],
            scale: [4.0, 5.0, 6.0],
            billboard: false,
        }));
        let expected = [
            ("pos_x", 1.0), ("pos_y", -2.0), ("pos_z", 3.5),
            ("rot_x", 0.25), ("rot_y", 0.5), ("rot_z", -0.75),
            ("scale_x", 4.0), ("scale_y", 5.0), ("scale_z", 6.0),
        ];
        assert_eq!(values, expected);
        let identity = run(None);
        assert_eq!(identity[6..], [("scale_x", 1.0), ("scale_y", 1.0), ("scale_z", 1.0)]);
        assert!(identity[..6].iter().all(|(_, value)| *value == 0.0));
    }
}
