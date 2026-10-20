//! The Swarm template through the preset runtime, pushed by a vortex, in a
//! debug build so the executor's in-place output check runs on every frame
//! without ticks (the first frame, a paused one, the in-between frame at
//! 120 fps). The swarm's particle state is read straight from its loop.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{NodeId, Seconds};
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::exec::effect_node::SimStep;
use manifold_node_engine::freeze::install::fuse_generator_view;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_node_engine::particles::Particle;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;
use serde_json::json;

const JSON: &str = include_str!("../../assets/generator-presets/Swarm.json");
const SIZE: u32 = 96;
const TICK: f64 = 1.0 / 60.0;

/// The swarm's group body in the preset document: the Add menu's swarm,
/// inserted whole.
fn swarm_body(doc: &mut serde_json::Value) -> &mut serde_json::Value {
    let nodes = doc["nodes"].as_array_mut().unwrap();
    &mut nodes.iter_mut().find(|node| node["typeId"] == "group").expect("the swarm is one group")["group"]
}

/// The document id and stable id of the swarm's one node of `type_id`.
fn swarm_node(type_id: &str) -> (u64, String) {
    let mut doc: serde_json::Value = serde_json::from_str(JSON).unwrap();
    let body = swarm_body(&mut doc);
    let node = body["nodes"].as_array().unwrap().iter().find(|node| node["typeId"] == type_id).expect("swarm node");
    (node["id"].as_u64().unwrap(), node["nodeId"].as_str().unwrap().to_owned())
}

/// The preset document with World gravity `g` along y (m/s², negative is
/// down): the World Gravity Y source and its card control's default, so a
/// manifest built from the card holds it too. The proofs of scene forces
/// run weightless, so only the force under test moves the swarm.
fn with_world_gravity(g: f32) -> serde_json::Value {
    let mut doc: serde_json::Value = serde_json::from_str(JSON).unwrap();
    let world = doc["nodes"].as_array().unwrap().iter().find(|node| node["typeId"] == "node.physics_world").expect("a World")["id"].clone();
    let source = doc["wires"].as_array().unwrap().iter()
        .find(|wire| wire["toNode"] == world && wire["toPort"] == "gravity_y").expect("World gravity has a source")["fromNode"].clone();
    let node = doc["nodes"].as_array_mut().unwrap().iter_mut().find(|node| node["id"] == source).unwrap();
    node["params"]["value"]["value"] = json!(g);
    let source_id = node["nodeId"].clone();
    let meta = &mut doc["presetMetadata"];
    let ids: Vec<serde_json::Value> = meta["bindings"].as_array_mut().unwrap().iter_mut()
        .filter(|binding| binding["target"]["nodeId"] == source_id)
        .map(|binding| binding["id"].clone())
        .collect();
    for binding in meta["bindings"].as_array_mut().unwrap().iter_mut().filter(|binding| ids.contains(&binding["id"])) {
        binding["defaultValue"] = json!(g);
    }
    for param in meta["params"].as_array_mut().unwrap().iter_mut().filter(|param| ids.contains(&param["id"])) {
        param["defaultValue"] = json!(g);
    }
    doc
}

/// The weightless preset with `nodes` beside the swarm's forces node, the
/// last one wired into its `acceleration_field`, as a scene force list would
/// wire it.
fn swarm_with_field(nodes: Vec<serde_json::Value>, mut wires: Vec<serde_json::Value>) -> EffectGraphDef {
    let mut doc = with_world_gravity(0.0);
    let (forces, _) = swarm_node("node.swarm_forces");
    let field = nodes.last().expect("a field node")["id"].clone();
    wires.push(json!({"fromNode": field, "fromPort": "out", "toNode": forces, "toPort": "acceleration_field"}));
    let body = swarm_body(&mut doc);
    body["nodes"].as_array_mut().unwrap().extend(nodes);
    body["wires"].as_array_mut().unwrap().extend(wires);
    serde_json::from_value(doc).unwrap()
}

/// The template with a vortex about its vertical axis wired into the swarm's
/// forces.
fn pushed_swarm() -> EffectGraphDef {
    swarm_with_field(
        vec![json!({
            "id": 90, "nodeId": "swarm_vortex", "typeId": "node.vortex_vector_field", "handle": "Swarm Vortex",
            "params": {"radius": {"type": "Float", "value": 3.0}}
        })],
        Vec::new(),
    )
}

struct Swarm {
    runtime: PresetRuntime,
    target: RenderTarget,
    manifest: manifold_core::params::ParamManifest,
}

impl Swarm {
    fn new(def: EffectGraphDef) -> Self {
        let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
        let mut runtime = PresetRuntime::from_def_with_device(
            def,
            &PrimitiveRegistry::with_builtin(),
            std::sync::Arc::clone(device),
            SIZE,
            SIZE,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("the Swarm template builds");
        runtime.set_dump_arrays(None, &[NodeId::new(swarm_node("node.move_swarm").1)]);
        let target = RenderTarget::new(device, SIZE, SIZE, GpuTextureFormat::Rgba16Float, "swarm-proof");
        Self { runtime, target, manifest: manifold_core::params::ParamManifest::default() }
    }

    fn frame(&mut self, seconds: f64, step: SimStep) {
        let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
        let context = PresetContext {
            time: seconds,
            beat: seconds * 2.0,
            dt: TICK as f32,
            width: SIZE,
            height: SIZE,
            output_width: SIZE,
            output_height: SIZE,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        self.runtime.set_sim_step(step);
        let mut encoder = device.create_encoder("swarm-proof");
        self.runtime.render(
            &mut GpuEncoder::new(&mut encoder, device),
            &self.target.texture,
            &context,
            &self.manifest,
        );
        encoder.commit_and_wait_completed();
    }

    /// Frames at `fps` over `(from, until]`.
    fn play(&mut self, fps: f64, from: f64, until: f64, step: SimStep) {
        let frames = ((until - from) * fps).round() as u32;
        for index in 1..=frames {
            self.frame(from + f64::from(index) / fps, step);
        }
    }

    /// The swarm's particles, from the loop's in-place buffer.
    fn particles(&self) -> Vec<Particle> {
        let dumps = self.runtime.dump_arrays_all();
        let dump = dumps.iter().find(|dump| dump.port == "out").expect("the swarm's particles are held");
        let count = (dump.buffer.size / std::mem::size_of::<Particle>() as u64) as usize;
        manifold_node_engine::testkit::array_harness::read(dump.buffer, count)
    }
}

fn live() -> SimStep {
    SimStep::live(Seconds(TICK))
}

fn bits(particles: &[Particle]) -> Vec<u32> {
    bytemuck::cast_slice::<Particle, u32>(particles).to_vec()
}

fn alive(particles: &[Particle]) -> usize {
    particles.iter().filter(|particle| particle.life > 0.0).count()
}

/// 60 fps and 120 fps with a pause land on the same particles at the same
/// time, bit for bit, and the vortex moved them.
#[test]
fn swarm_moves_the_same_at_60_and_120_fps_through_tickless_and_paused_frames() {
    let mut steady = Swarm::new(pushed_swarm());
    steady.frame(0.0, live());
    let seeded = steady.particles();
    assert_eq!(alive(&seeded), 20_000, "the template seeds its Count");
    steady.play(60.0, 0.0, 1.0, live());

    let mut fast = Swarm::new(pushed_swarm());
    fast.frame(0.0, live());
    fast.play(120.0, 0.0, 0.5, live());
    for _ in 0..10 {
        fast.frame(0.5, live());
    }
    fast.play(120.0, 0.5, 1.0, live());

    let (a, b) = (steady.particles(), fast.particles());
    assert_eq!(bits(&a), bits(&b), "a swarm moves the same at any display rate");
    let moved = a.iter().zip(&seeded).filter(|(now, then)| now.life > 0.0 && now.position != then.position).count();
    assert!(moved > 19_000, "the vortex moves the swarm: {moved} of 20000 moved");
}

/// After a loop back the swarm reseeds and replays exactly what an export
/// from the loop point simulates.
#[test]
fn swarm_live_after_a_loop_matches_an_export() {
    let mut looped = Swarm::new(pushed_swarm());
    looped.frame(0.0, live());
    looped.play(60.0, 0.0, 1.0, live());
    looped.frame(0.25, live());
    looped.play(60.0, 0.25, 0.75, live());

    let export = SimStep::export(Seconds(TICK));
    let mut fresh = Swarm::new(pushed_swarm());
    fresh.frame(0.25, export);
    fresh.play(60.0, 0.25, 0.75, export);
    assert_eq!(bits(&looped.particles()), bits(&fresh.particles()), "live after a loop matches the export");
}

/// The graph compiler keeps the swarm's particle loop: whatever it fuses,
/// the fused template moves the swarm exactly as the unfused one.
#[test]
fn swarm_fused_template_matches_unfused() {
    let canonical = pushed_swarm();
    let registry = PrimitiveRegistry::with_builtin();
    let fused = fuse_generator_view(&canonical, &registry).map_or_else(|| canonical.clone(), |view| (*view.def).clone());
    let flat = manifold_core::flatten::flatten_groups(&fused).expect("the fused template flattens");
    assert!(
        flat.nodes.iter().any(|node| node.type_id == "node.move_swarm"),
        "move_swarm writes the loop in place, so it stays its own step"
    );
    let mut runs = [Swarm::new(canonical), Swarm::new(fused)];
    for run in &mut runs {
        run.frame(0.0, live());
        run.play(60.0, 0.0, 0.5, live());
    }
    let [unfused, fused] = runs.map(|run| run.particles());
    assert_eq!(bits(&unfused), bits(&fused));
    assert!(unfused.iter().zip(&pushed_seed()).any(|(now, then)| now.position != then.position), "the swarm moved");
}

/// The seeded particles, one frame in.
fn pushed_seed() -> Vec<Particle> {
    let mut swarm = Swarm::new(pushed_swarm());
    swarm.frame(0.0, live());
    swarm.particles()
}

/// The template in a wind of 2 m/s along z with drag rate 2/s: every particle
/// started at rest, so after n ticks of 1/60 s each one's velocity is
/// exactly w (1 − e^(−k·n/60)), the wind's relaxation the CPU twin integrates.
#[test]
fn swarm_takes_on_the_wind_at_its_drag_rate() {
    let mut swarm = Swarm::new(swarm_with_field(
        vec![
            json!({
                "id": 90, "nodeId": "wind_velocity", "typeId": "node.uniform_vector_field", "handle": "Wind Velocity",
                "params": {"x": {"type": "Float", "value": 0.0}, "y": {"type": "Float", "value": 0.0}, "z": {"type": "Float", "value": 2.0}}
            }),
            json!({
                "id": 91, "nodeId": "wind", "typeId": "node.wind_field", "handle": "Wind",
                "params": {"drag_rate": {"type": "Float", "value": 2.0}}
            }),
        ],
        vec![json!({"fromNode": 90, "fromPort": "out", "toNode": 91, "toPort": "velocity"})],
    ));
    swarm.frame(0.0, live());
    swarm.play(60.0, 0.0, 1.0, live());
    let particles = swarm.particles();
    // The ticks that ran: the one whole count whose relaxation the swarm shows.
    let relax = |ticks: u32| 2.0 * (1.0 - (-2.0 * ticks as f32 * TICK as f32).exp());
    let first = particles.iter().find(|particle| particle.life > 0.0).expect("a live particle").velocity[2];
    let ticks = (55..=61).min_by(|&a, &b| (relax(a) - first).abs().total_cmp(&(relax(b) - first).abs())).unwrap();
    assert!((58..=60).contains(&ticks), "about a second of ticks ran: {ticks}");
    let want = relax(ticks);
    let mut checked = 0;
    for particle in particles.iter().filter(|particle| particle.life > 0.0) {
        assert!((particle.velocity[2] - want).abs() < 1e-4, "velocity {:?}, want z {want} after {ticks} ticks", particle.velocity);
        assert!(particle.velocity[0].abs() < 1e-6 && particle.velocity[1].abs() < 1e-6, "velocity {:?}", particle.velocity);
        checked += 1;
    }
    assert_eq!(checked, 20_000);
}

/// BUG-g5rw2: the Swarm template under a Radial force builds, and a Fire
/// kicks every live particle once, on the first tick after it, by the hit's
/// field at the particle (read through the impulse lattice, as the CPU twin
/// reads it). With the force's steady push at 0 and no air, nothing else
/// moves the swarm, so its velocities hold after the kick.
#[test]
fn a_radial_force_hit_kicks_the_swarm_once() {
    use manifold_core::effect_graph_def::BindingTarget;
    use manifold_core::params::{Param, ParamManifest};
    use manifold_core::scene_impulse::{ImpulseFire, ImpulseTarget};
    use manifold_core::scene_modifier_edit::insert_scene_modifier;
    use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
    use manifold_node_engine::exec::effect_node::FrameTime;
    use manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier;
    use manifold_nodes_water::runtime::WaterRuntimeExt;
    use manifold_physics::force_lattice::ForceLattice;
    use manifold_physics::VectorField;

    let swarm: EffectGraphDef = serde_json::from_value(with_world_gravity(0.0)).unwrap();
    let recipe: EffectGraphDef = serde_json::from_str(include_str!("../../assets/scene-modifier-presets/RadialForce.json")).unwrap();
    let scene = SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") };
    let instance = prepare_new_scene_modifier(&swarm, &recipe, NodeId::new("blast"), scene, SceneTargetSelection::AllObjects).unwrap();
    let def = insert_scene_modifier(&swarm, 0, instance).unwrap().graph;
    let metadata = def.preset_metadata.clone().unwrap();
    let alias = |local: &str| {
        metadata.bindings.iter().find(|binding| matches!(&binding.target, BindingTarget::SceneModifier { param_id, .. } if param_id == local)).unwrap().id.clone()
    };
    let mut manifest = ParamManifest::from_params(metadata.params.iter().cloned().map(Param::bundled).collect());
    for (local, value) in [("strength", 0.0), ("radius", 3.0), ("falloff", 1.0), ("impulse_strength", 3.0)] {
        manifest.get_mut(&alias(local)).unwrap().value = value;
    }

    let mut run = Swarm::new(def);
    run.manifest = manifest;
    run.frame(0.0, live());
    run.play(60.0, 0.0, 0.25, live());
    let before = run.particles();
    assert!(before.iter().all(|particle| particle.velocity == [0.0; 3]), "no steady push, no motion");
    let source = FrameTime { beats: manifold_core::Beats(0.5), seconds: Seconds(0.25), delta: Seconds(TICK), frame_count: 0 };
    let mut sequence = 1;
    assert_eq!(run.runtime.fire_scene_impulse(&alias("fire"), source, &mut sequence), Ok(ImpulseFire::Admitted));
    run.play(60.0, 0.25, 0.5, live());
    let kicked = run.particles();

    let mut receipts = Vec::new();
    run.runtime.water().drain_scene_impulses(|id, event| receipts.push((id.clone(), event)));
    assert_eq!(receipts.len(), 1, "one fire, one landing");
    let (id, event) = &receipts[0];
    assert_eq!((id.as_str(), event.value.target), (swarm_node("node.swarm_forces").1.as_str(), ImpulseTarget::Swarm));
    // The template's bounds: a 4 m cube about the origin, 17 nodes a side.
    let lattice = ForceLattice::over_box([-2.0; 3], [4.0; 3], 17);
    let mut values = vec![[0.0f32; 4]; lattice.node_count()];
    lattice.fill(&mut values, |x| event.value.field.sample(x)).unwrap();
    let mut moved = 0;
    for (i, (now, then)) in kicked.iter().zip(&before).enumerate().filter(|(_, (now, _))| now.life > 0.0) {
        let want = lattice.sample(&values, then.position);
        for axis in 0..3 {
            assert!((now.velocity[axis] - want[axis]).abs() <= 1e-4 * (1.0 + want[axis].abs()), "particle {i}: {:?} vs {want:?}", now.velocity);
        }
        moved += usize::from(want.iter().any(|&v| v.abs() > 1e-3));
    }
    assert!(moved > 10_000, "the blast reaches the swarm: {moved} kicked");

    run.play(60.0, 0.5, 1.0, live());
    let later = run.particles();
    assert!(later.iter().zip(&kicked).all(|(a, b)| a.velocity == b.velocity), "the kick lands once");
    run.runtime.water().drain_scene_impulses(|_, _| panic!("no second landing"));
}

/// D-109: the shipped swarm falls under the Physics World's gravity, as a
/// physics object does. With no other force, after n ticks of 1/60 s every
/// live particle moves at exactly (0, g·n/60, 0), and a different World
/// gravity is the one it falls by.
#[test]
fn swarm_falls_under_world_gravity() {
    for g in [-9.81f32, -1.62] {
        let mut swarm = Swarm::new(serde_json::from_value(with_world_gravity(g)).unwrap());
        swarm.frame(0.0, live());
        swarm.play(60.0, 0.0, 0.5, live());
        let particles = swarm.particles();
        let first = particles.iter().find(|particle| particle.life > 0.0).expect("a live particle").velocity[1];
        let ticks = (first / g * 60.0).round();
        assert!((29.0..=31.0).contains(&ticks), "about half a second of ticks ran: {ticks}");
        let want = g * ticks / 60.0;
        let mut checked = 0;
        for particle in particles.iter().filter(|particle| particle.life > 0.0) {
            let [x, y, z] = particle.velocity;
            assert!((y - want).abs() < 1e-3 && x.abs() < 1e-6 && z.abs() < 1e-6, "velocity {:?}, want y {want}", particle.velocity);
            checked += 1;
        }
        assert_eq!(checked, 20_000);
    }
}
