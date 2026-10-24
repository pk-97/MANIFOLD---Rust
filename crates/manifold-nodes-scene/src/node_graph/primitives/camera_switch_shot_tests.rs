//! A camera cut through `node.camera_switch` is a new shot.

use super::*;
use manifold_core::{Beats, Seconds};
use manifold_node_engine::bindings::{NodeInputs, NodeOutputs, Slot};
use manifold_node_engine::exec::backend::{Backend, MockBackend};
use manifold_node_engine::exec::effect_node::FrameTime;
use manifold_node_engine::exec::execution_plan::ResourceId;

/// The switch's output with `select` at `pick`, cameras on `a` and `b`.
fn switched(pick: u32) -> Camera {
    let mut backend = MockBackend::new();
    let a = backend.acquire(ResourceId(0), PortType::Camera, None, (0, 0));
    let b = backend.acquire(ResourceId(1), PortType::Camera, None, (0, 0));
    let out = backend.acquire(ResourceId(2), PortType::Camera, None, (0, 0));
    backend.set_camera(a, Camera::default_perspective());
    backend.set_camera(
        b,
        Camera {
            pos: [3.0, 1.0, 5.0],
            ..Camera::default_perspective()
        },
    );
    let mut params = ParamValues::default();
    params.insert(Cow::Borrowed("select"), ParamValue::Enum(pick));
    let input_bindings: &[(&'static str, Slot)] = &[("a", a), ("b", b)];
    let output_bindings: &[(&'static str, Slot)] = &[("out", out)];
    let (mut scalars, mut cameras, mut lights, mut materials) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut transforms, mut atmospheres, mut render_modes, mut objects) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let inputs = NodeInputs::new(input_bindings, &backend, &[]);
    let outputs = NodeOutputs::new(
        output_bindings,
        &backend,
        &mut scalars,
        &mut cameras,
        &mut lights,
        &mut materials,
        &mut transforms,
        &mut atmospheres,
        &mut render_modes,
        &mut objects,
    );
    let time = FrameTime {
        beats: Beats(0.0),
        seconds: Seconds(0.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    };
    {
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        CameraSwitch::new().evaluate(&mut ctx);
    }
    cameras.pop().expect("a camera every frame").1
}

/// A cut is a new shot, so the renderer resets its temporal history on
/// that frame; holding the same pick keeps the shot, so a moving camera
/// keeps its motion vectors.
#[test]
fn a_cut_changes_the_shot_and_a_held_pick_keeps_it() {
    assert_ne!(switched(0).shot, switched(1).shot);
    assert_eq!(switched(1).shot, switched(1).shot);
}
