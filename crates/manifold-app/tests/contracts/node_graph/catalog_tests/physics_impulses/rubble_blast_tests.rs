//! Rubble Pile's Blast, end to end: the real Explosion fired through the scene
//! impulse route captures its field, and that field scatters a settled pile.
use super::*;
use manifold_core::effect_graph_def::{BindingTarget, SerializedParamValue};
use manifold_core::params::{Param, ParamManifest};
use manifold_core::scene_impulse::{ImpulseFire, RigidImpulseTargets};
use manifold_node_engine::scene::transform::Transform;
use manifold_water_rigid::physics::{MAX_BODIES, ResolvedRigidImpulse, RigidBody, RigidSimulation};

fn rubble() -> EffectGraphDef {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../manifold-nodes/assets/generator-presets/RubblePile.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The Blast's captured field, from firing the preset's own Explosion.
fn blast_field(def: &EffectGraphDef) -> FieldValue {
    let manifest = ParamManifest::from_params(def.preset_metadata.as_ref().unwrap().params.iter().cloned().map(Param::bundled).collect());
    let fire = def
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .find(|b| matches!(&b.target, BindingTarget::SceneModifier { param_id, .. } if param_id == "fire"))
        .unwrap()
        .id
        .clone();
    let mut runtime = PresetRuntime::from_def_for_render(def.clone(), &registry(), Some(&manifest), false).unwrap();
    runtime.execute_frame(time(0.0));
    let mut sequence = 0;
    assert_eq!(runtime.fire_scene_impulse(&fire, time(0.0), &mut sequence), Ok(ImpulseFire::Admitted));
    runtime.execute_frame(time(DT));
    let mut fields = Vec::new();
    runtime.water().drain_scene_impulses(|_, event| {
        assert!(matches!(event.value.target, ImpulseTarget::Rigid(t) if t.copies), "the Blast reaches the copies");
        fields.push(event.value.field);
    });
    assert_eq!(fields.len(), 1, "one press, one hit");
    fields.remove(0)
}

fn bodies_of(def: &EffectGraphDef) -> ([Option<RigidBody>; MAX_BODIES], RigidBody, [f32; 4]) {
    let scalar = |id: u32, name: &str| {
        let node = def.nodes.iter().find(|n| n.id == id).unwrap();
        match node.params.get(name).unwrap_or_else(|| panic!("node {id} has no {name}")) {
            SerializedParamValue::Float { value } => *value,
            SerializedParamValue::Enum { value } => *value as f32,
            other => panic!("unexpected {name}: {other:?}"),
        }
    };
    let body = |id: u32| RigidBody {
        transform: Transform {
            pos: ["pos_x", "pos_y", "pos_z"].map(|p| scalar(id - 1, p)),
            rot_euler: ["rot_x", "rot_y", "rot_z"].map(|p| scalar(id - 1, p)),
            scale: ["scale_x", "scale_y", "scale_z"].map(|p| scalar(id - 1, p)),
            billboard: false,
        },
        density: scalar(id, "density"),
        shape: scalar(id, "shape") as u32,
        kind: scalar(id, "motion") as u32,
        friction: scalar(id, "friction"),
        bounce: scalar(id, "bounce"),
        enabled: true,
        ..RigidBody::default()
    };
    let mut bodies: [Option<RigidBody>; MAX_BODIES] = std::array::from_fn(|_| None);
    for (slot, id) in [101, 141, 161].into_iter().enumerate() {
        bodies[slot] = Some(body(id));
    }
    let layout = [scalar(121, "copy_count"), scalar(121, "copy_spacing"), scalar(121, "copy_columns"), scalar(121, "copy_layout")];
    (bodies, body(121), layout)
}

#[test]
fn rubble_blast_scatters_a_settled_pile_and_keeps_it_in_the_world() {
    let def = rubble();
    let field = blast_field(&def);
    let (bodies, prototype, layout) = bodies_of(&def);
    let mut sim = RigidSimulation::default();
    let frame = std::cell::Cell::new(0_u32);
    let advance = |sim: &mut RigidSimulation, frames: u32| {
        for _ in 0..frames {
            sim.advance_with_copy_layout(bodies.clone(), Some(prototype.clone()), layout[0], layout[1], layout[2], layout[3], [0.0, -9.81, 0.0], Seconds(f64::from(frame.get()) / 60.0), 1.0, 0.0).unwrap();
            frame.set(frame.get() + 1);
        }
    };
    advance(&mut sim, 600);
    let settled = sim.copy_poses[..sim.active_copy_count].to_vec();
    advance(&mut sim, 60);
    let still = sim.copy_poses[..sim.active_copy_count].to_vec();
    let drift = settled.iter().zip(&still).map(|(a, b)| dist(a.pos, b.pos)).fold(0.0_f32, f32::max);
    let epoch = sim.impulse_epoch().unwrap();
    sim.enqueue_impulse(
        EventStamp { epoch, time: Seconds(f64::from(frame.get()) / 60.0), sequence: 0 },
        ResolvedRigidImpulse { field, targets: RigidImpulseTargets { bodies: 0, copies: true } },
    )
    .unwrap();
    advance(&mut sim, 90);
    let after = sim.copy_poses[..sim.active_copy_count].to_vec();
    let mut moved: Vec<f32> = still.iter().zip(&after).map(|(a, b)| dist(a.pos, b.pos)).collect();
    moved.sort_by(|a, b| a.total_cmp(b));
    let median = moved[moved.len() / 2];
    let over_half = moved.iter().filter(|d| **d > 0.5).count();
    let lost = after.iter().filter(|p| !p.pos.iter().all(|v| v.is_finite()) || p.pos[1] < -1.0 || p.pos.iter().any(|v| v.abs() > 40.0)).count();
    println!("RUBBLE_BLAST settled_drift={drift:.3} count={} moved>0.5m={over_half} median={median:.2}m max={:.2}m lost={lost}", after.len(), moved.last().unwrap());
    assert!(drift < 0.1, "the pile has settled before the blast: {drift}");
    assert!(over_half * 10 >= after.len() * 8, "most copies move far: {over_half}/{}", after.len());
    assert!(median > 1.0, "the pile does not just twitch: median {median}");
    assert_eq!(lost, 0, "no copy leaves the world");
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// The same Blast through the real renderer: the frame after the press differs
/// from the settled pile. `RUBBLE_BLAST_PNG=path` saves the after-frame.
#[test]
fn rubble_blast_renders_a_scattered_pile() {
    use manifold_gpu::GpuTextureFormat;
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
    use manifold_node_engine::gpu::headless_readback::{mean_abs_half_diff, readback_raw_halves, readback_to_srgb_png};
    use manifold_node_engine::gpu::render_target::RenderTarget;
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_water_rigid::physics::SimStep;

    let def = rubble();
    let manifest = ParamManifest::from_params(def.preset_metadata.as_ref().unwrap().params.iter().cloned().map(Param::bundled).collect());
    let fire = def
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .find(|b| matches!(&b.target, BindingTarget::SceneModifier { param_id, .. } if param_id == "fire"))
        .unwrap()
        .id
        .clone();
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let device = &harness.device;
    let (width, height) = (960, 540);
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &serde_json::to_string(&def).unwrap(),
        &PrimitiveRegistry::with_builtin(),
        std::sync::Arc::clone(device),
        width,
        height,
        GpuTextureFormat::Rgba16Float,
        Some(&manifest),
    )
    .unwrap();
    let target = RenderTarget::new(device, width, height, GpuTextureFormat::Rgba16Float, "rubble-blast");
    let render = |runtime: &mut PresetRuntime, frame: u32| {
        let seconds = f64::from(frame) / 60.0;
        let context = PresetContext {
            time: seconds,
            beat: seconds,
            dt: 1.0 / 60.0,
            width,
            height,
            output_width: width,
            output_height: height,
            aspect: width as f32 / height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: i64::from(frame),
            anim_progress: 0.0,
            trigger_count: 0,
        };
        runtime.set_sim_step(SimStep::live(Seconds(1.0 / 60.0)));
        let mut encoder = device.create_encoder("rubble-blast");
        runtime.render(&mut GpuEncoder::new(&mut encoder, device), &target.texture, &context, &manifest);
        encoder.commit_and_wait_completed();
        seconds
    };
    let mut seconds = 0.0;
    for frame in 0..420 {
        seconds = render(&mut runtime, frame);
    }
    let settled = readback_raw_halves(device, &target.texture, width, height);
    let mut sequence = 0;
    assert_eq!(runtime.fire_scene_impulse(&fire, time(seconds), &mut sequence), Ok(ImpulseFire::Admitted));
    for frame in 420..470 {
        render(&mut runtime, frame);
    }
    let after = readback_raw_halves(device, &target.texture, width, height);
    let diff = mean_abs_half_diff(&settled, &after);
    println!("RUBBLE_BLAST_FRAME mean_abs_diff={diff:.5}");
    if let Ok(path) = std::env::var("RUBBLE_BLAST_PNG") {
        std::fs::write(path, readback_to_srgb_png(device, &target.texture, width, height)).unwrap();
    }
    assert!(diff > 0.002, "the blast changes the picture: {diff}");
}
