//! `node.transform_components` — the nine scalars of a `Transform` wire, the
//! inverse of `node.transform_3d`. Lets GPU atoms that take scalar params
//! (lattice bounds, a collider's position) read a Transform-typed wire:
//! codegen buffer kernels bind only params and arrays, and a Transform wire
//! into a GPU atom is a fusion cut. CPU-only.
//!
//! With `parent` or `frame` wired it measures a placement instead: the
//! transform sat in `parent` (as a scene object's `parent_transform` places
//! it), seen from `frame`. A modifier bending a group as one piece reads each
//! part's live place in the group's frame this way.

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::pipeline::mat4_mul;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::transform::{IDENTITY_MAT4, Mat4, rotation_to_render_scene_euler};

manifold_node_engine::primitive! {
    name: TransformComponents,
    type_id: "node.transform_components",
    purpose: "Split a Transform into its nine scalars: position X/Y/Z, rotation X/Y/Z (radians, XYZ Euler) and scale X/Y/Z. The inverse of node.transform_3d, with the same port names. An unwired input publishes the identity transform. With `parent` or `frame` wired, the scalars are the whole placement (the transform's world, inside `parent` when wired) measured in `frame`'s space (the world when unwired).",
    inputs: {
        transform: Transform optional,
        parent: Transform optional,
        frame: Transform optional,
    },
    outputs: {
        pos_x: ScalarF32, pos_y: ScalarF32, pos_z: ScalarF32,
        rot_x: ScalarF32, rot_y: ScalarF32, rot_z: ScalarF32,
        scale_x: ScalarF32, scale_y: ScalarF32, scale_z: ScalarF32,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Use it where a GPU atom needs a box or pose as scalar params: a fluid lattice's grid_bounds (position = centre, scale = full size) into the surface atoms' center_x/y/z and size_x/y/z, or an object's position into a field. Wire only the components you need. To read where a scene object sits inside a group, wire the object's transform into `transform`, its parent_transform's producer into `parent` and the group's transform into `frame`.",
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
        let parent = ctx.inputs.transform("parent");
        let frame = ctx.inputs.transform("frame");
        let components = if parent.is_none() && frame.is_none() {
            [transform.pos, transform.rot_euler, transform.scale]
        } else {
            let world = parent.map_or(transform.world(), |parent| {
                mat4_mul(parent.world(), transform.world())
            });
            // A frame scaled to nothing has no space to measure in: read the world.
            let seen = frame
                .and_then(|frame| inverse_affine(&frame.world()))
                .map_or(world, |inverse| mat4_mul(inverse, world));
            decompose(&seen)
        };
        let names = [
            ["pos_x", "pos_y", "pos_z"],
            ["rot_x", "rot_y", "rot_z"],
            ["scale_x", "scale_y", "scale_z"],
        ];
        for (ports, values) in names.iter().zip(components) {
            for (port, value) in ports.iter().zip(values) {
                ctx.outputs.set_scalar(port, ParamValue::Float(value));
            }
        }
    }
}

/// The inverse of an affine matrix (no projective row); `None` when its
/// 3x3 part is singular.
fn inverse_affine(m: &Mat4) -> Option<Mat4> {
    let a = |col: usize, row: usize| m[col][row];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(2, 1) * a(1, 2))
        - a(1, 0) * (a(0, 1) * a(2, 2) - a(2, 1) * a(0, 2))
        + a(2, 0) * (a(0, 1) * a(1, 2) - a(1, 1) * a(0, 2));
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    // The 3x3 inverse is the transposed cofactor matrix over the determinant.
    let linear: [[f32; 3]; 3] = std::array::from_fn(|col| {
        std::array::from_fn(|row| {
            let (r1, r2) = ((col + 1) % 3, (col + 2) % 3);
            let (c1, c2) = ((row + 1) % 3, (row + 2) % 3);
            (a(c1, r1) * a(c2, r2) - a(c2, r1) * a(c1, r2)) * inv
        })
    });
    let t = [m[3][0], m[3][1], m[3][2]];
    let moved: [f32; 3] = std::array::from_fn(|row| {
        -(linear[0][row] * t[0] + linear[1][row] * t[1] + linear[2][row] * t[2])
    });
    let mut out = IDENTITY_MAT4;
    for (column, values) in out.iter_mut().zip(linear.iter().chain([&moved])) {
        column[..3].copy_from_slice(values);
    }
    Some(out)
}

/// Position, XYZ Euler rotation and scale of an affine matrix built as
/// `T * R * S`.
fn decompose(m: &Mat4) -> [[f32; 3]; 3] {
    let pos = [m[3][0], m[3][1], m[3][2]];
    let scale: [f32; 3] = std::array::from_fn(|col| {
        (m[col][0] * m[col][0] + m[col][1] * m[col][1] + m[col][2] * m[col][2]).sqrt()
    });
    let axis = |col: usize, row: usize| {
        if scale[col] > 0.0 {
            m[col][row] / scale[col]
        } else {
            f32::from(u8::from(col == row))
        }
    };
    let rot = rotation_to_render_scene_euler(std::array::from_fn(|row| {
        std::array::from_fn(|col| axis(col, row))
    }));
    [pos, rot, scale]
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{Beats, Seconds};
    use manifold_node_engine::bindings::{NodeInputs, NodeOutputs};
    use manifold_node_engine::exec::backend::Backend;
    use manifold_node_engine::exec::effect_node::{FrameTime, ParamValues};
    use manifold_node_engine::scene::transform::Transform;
    use manifold_node_engine::{
        exec::backend::MockBackend, exec::execution_plan::ResourceId, ports::PortType,
        ports::ScalarType,
    };

    fn run(input: Option<Transform>) -> Vec<(&'static str, f32)> {
        run_wired(
            &input
                .map(|value| ("transform", value))
                .into_iter()
                .collect::<Vec<_>>(),
        )
    }

    fn run_wired(wired: &[(&'static str, Transform)]) -> Vec<(&'static str, f32)> {
        let mut backend = MockBackend::new();
        let mut input_bindings = Vec::new();
        for (index, (port, value)) in wired.iter().enumerate() {
            let slot = backend.acquire(
                ResourceId(100 + index as u32),
                PortType::Transform,
                None,
                (0, 0),
            );
            backend.set_transform(slot, *value);
            input_bindings.push((*port, slot));
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
        let (mut scalar, mut camera, mut light, mut material) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
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
                let ParamValue::Float(value) = value else {
                    panic!("scalar outputs are floats")
                };
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
            ..Default::default()
        }));
        let expected = [
            ("pos_x", 1.0),
            ("pos_y", -2.0),
            ("pos_z", 3.5),
            ("rot_x", 0.25),
            ("rot_y", 0.5),
            ("rot_z", -0.75),
            ("scale_x", 4.0),
            ("scale_y", 5.0),
            ("scale_z", 6.0),
        ];
        assert_eq!(values, expected);
        let identity = run(None);
        assert_eq!(
            identity[6..],
            [("scale_x", 1.0), ("scale_y", 1.0), ("scale_z", 1.0)]
        );
        assert!(identity[..6].iter().all(|(_, value)| *value == 0.0));

        // A part placed in a turned, scaled group reads its own TRS back in
        // the group's frame, and its world place with no frame.
        let part = Transform {
            pos: [1.0, -2.0, 3.5],
            rot_euler: [0.25, 0.5, -0.75],
            scale: [1.5, 0.5, 2.0],
            ..Default::default()
        };
        let group = Transform {
            pos: [4.0, 1.0, -3.0],
            rot_euler: [0.0, 0.9, 0.3],
            scale: [2.0; 3],
            ..Default::default()
        };
        let seen = run_wired(&[("transform", part), ("parent", group), ("frame", group)]);
        let own = [1.0, -2.0, 3.5, 0.25, 0.5, -0.75, 1.5, 0.5, 2.0];
        for ((name, value), expected) in seen.iter().zip(own) {
            assert!(
                (value - expected).abs() < 1e-4,
                "{name}: {value} != {expected}"
            );
        }
        let world = run_wired(&[("transform", part), ("parent", group)]);
        let placed =
            manifold_node_engine::scene::transform::transform_point(&group.world(), part.pos);
        for ((name, value), expected) in world.iter().zip(placed) {
            assert!(
                (value - expected).abs() < 1e-4,
                "{name}: {value} != {expected}"
            );
        }
    }
}
