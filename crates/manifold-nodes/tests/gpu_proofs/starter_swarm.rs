//! Swarm Cloud's Burst through the real renderer: the preset's own Explosion
//! fired through the scene impulse route kicks the live swarm once, and the
//! swarm ends up displaced from where the same run goes without it.
//! `STARTER_SWARM_PNG=path` saves the frame well after the hit.

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::params::{Param, ParamManifest};
use manifold_core::scene_impulse::ImpulseFire;
use manifold_core::{NodeId, Seconds};
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::exec::effect_node::{FrameTime, SimStep};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::gpu::headless_readback::readback_to_srgb_png;
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_node_engine::particles::Particle;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;

const JSON: &str = include_str!("../../assets/generator-presets/SwarmCloud.json");
const TICK: f64 = 1.0 / 60.0;
const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;

struct Run {
    runtime: PresetRuntime,
    target: RenderTarget,
    manifest: ParamManifest,
    fire: String,
    frame: u32,
}

impl Run {
    fn new() -> Self {
        let def: EffectGraphDef = serde_json::from_str(JSON).unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        let manifest = ParamManifest::from_params(meta.params.iter().cloned().map(Param::bundled).collect());
        let fire = meta
            .bindings
            .iter()
            .find(|b| matches!(&b.target, BindingTarget::SceneModifier { modifier_id, param_id } if modifier_id.as_str() == "swarm_burst" && param_id == "fire"))
            .expect("the card has the Burst")
            .id
            .clone();
        let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
        let mut runtime = PresetRuntime::from_json_str_with_device(JSON, &PrimitiveRegistry::with_builtin(), std::sync::Arc::clone(device), WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, Some(&manifest))
            .expect("Swarm Cloud builds");
        let group: serde_json::Value = serde_json::from_str::<serde_json::Value>(JSON).unwrap();
        let mover = group["nodes"].as_array().unwrap().iter().filter_map(|n| n["group"]["nodes"].as_array()).flatten().find(|n| n["typeId"] == "node.move_swarm").expect("move_swarm")["nodeId"].as_str().unwrap().to_owned();
        runtime.set_dump_arrays(None, &[NodeId::new(mover)]);
        let target = RenderTarget::new(device, WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, "starter-swarm");
        Self { runtime, target, manifest, fire, frame: 0 }
    }

    fn render(&mut self) {
        let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
        let seconds = f64::from(self.frame) / 60.0;
        let context = PresetContext {
            time: seconds,
            beat: seconds * 2.0,
            dt: TICK as f32,
            width: WIDTH,
            height: HEIGHT,
            output_width: WIDTH,
            output_height: HEIGHT,
            aspect: WIDTH as f32 / HEIGHT as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: i64::from(self.frame),
            anim_progress: 0.0,
            trigger_count: 0,
        };
        self.runtime.set_sim_step(SimStep::live(Seconds(TICK)));
        let mut encoder = device.create_encoder("starter-swarm");
        self.runtime.render(&mut GpuEncoder::new(&mut encoder, device), &self.target.texture, &context, &self.manifest);
        encoder.commit_and_wait_completed();
        self.frame += 1;
    }

    fn advance(&mut self, frames: u32) {
        for _ in 0..frames {
            self.render();
        }
    }

    fn hit(&mut self) {
        let seconds = f64::from(self.frame) / 60.0;
        let time = FrameTime { beats: manifold_core::Beats(seconds * 2.0), seconds: Seconds(seconds), delta: Seconds(TICK), frame_count: i64::from(self.frame) };
        let mut sequence = 1;
        assert_eq!(self.runtime.fire_scene_impulse(&self.fire, time, &mut sequence), Ok(ImpulseFire::Admitted));
    }

    fn particles(&self) -> Vec<Particle> {
        let dumps = self.runtime.dump_arrays_all();
        let dump = dumps.iter().find(|d| d.port == "out").expect("the swarm's particles are held");
        let count = (dump.buffer.size / std::mem::size_of::<Particle>() as u64) as usize;
        manifold_node_engine::testkit::array_harness::read(dump.buffer, count)
    }
}

fn speed(p: &Particle) -> f32 {
    p.velocity.iter().map(|v| v * v).sum::<f32>().sqrt()
}

/// The Burst lifts every live particle's speed within a few ticks, and 45 frames
/// on the swarm sits well away from where the same run goes without it.
#[test]
fn burst_kicks_the_cloud_and_displaces_it() {
    const SETTLE: u32 = 90;
    const AFTER: u32 = 45;
    let mut quiet = Run::new();
    quiet.advance(SETTLE + AFTER);
    let control = quiet.particles();

    let mut run = Run::new();
    run.advance(SETTLE);
    let before = run.particles();
    run.hit();
    run.advance(3);
    let kicked = run.particles();
    run.advance(AFTER - 3);
    let after = run.particles();
    if let Ok(path) = std::env::var("STARTER_SWARM_PNG") {
        std::fs::write(path, readback_to_srgb_png(&manifold_node_engine::testkit::gpu_harness::shared().device, &run.target.texture, WIDTH, HEIGHT)).unwrap();
    }

    let live: Vec<usize> = (0..before.len()).filter(|&i| before[i].life > 0.0 && after[i].life > 0.0 && control[i].life > 0.0).collect();
    let mean = |f: &dyn Fn(usize) -> f32| live.iter().map(|&i| f(i)).sum::<f32>() / live.len() as f32;
    let speed_before = mean(&|i| speed(&before[i]));
    let speed_kicked = mean(&|i| speed(&kicked[i]));
    let mut gap: Vec<f32> = live.iter().map(|&i| (0..3).map(|a| (after[i].position[a] - control[i].position[a]).powi(2)).sum::<f32>().sqrt()).collect();
    gap.sort_by(|a, b| a.total_cmp(b));
    let median = gap[gap.len() / 2];
    let far = gap.iter().filter(|g| **g > 0.5).count();
    println!("STARTER_SWARM live={} speed_before={speed_before:.2} speed_3_ticks_on={speed_kicked:.2} median_gap={median:.2}m over_half_m={far}/{}", live.len(), live.len());
    assert!(live.len() > 10_000, "most of the swarm is alive: {}", live.len());
    assert!(speed_kicked > speed_before * 1.5, "the hit kicks the swarm: {speed_before} -> {speed_kicked}");
    assert!(median > 0.5, "the swarm ends up away from the quiet run: median gap {median}");
}
