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

/// The template with a vortex about its vertical axis wired into the swarm's
/// forces, as a scene force list would wire it.
fn pushed_swarm() -> EffectGraphDef {
    let mut doc: serde_json::Value = serde_json::from_str(JSON).unwrap();
    doc["nodes"].as_array_mut().unwrap().push(json!({
        "id": 50, "nodeId": "swarm_vortex", "typeId": "node.vortex_vector_field", "handle": "Swarm Vortex",
        "params": {"radius": {"type": "Float", "value": 3.0}}
    }));
    doc["wires"].as_array_mut().unwrap().push(json!({"fromNode": 50, "fromPort": "out", "toNode": 11, "toPort": "acceleration_field"}));
    serde_json::from_value(doc).unwrap()
}

struct Swarm {
    runtime: PresetRuntime,
    target: RenderTarget,
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
        runtime.set_dump_arrays(None, &[NodeId::new("swarm_move")]);
        let target = RenderTarget::new(device, SIZE, SIZE, GpuTextureFormat::Rgba16Float, "swarm-proof");
        Self { runtime, target }
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
            &manifold_core::params::ParamManifest::default(),
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
    assert!(
        fused.nodes.iter().any(|node| node.type_id == "node.move_swarm"),
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
