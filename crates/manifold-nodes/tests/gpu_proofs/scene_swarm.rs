//! A scene document's Swarm through the real renderer, one scene per emitter
//! promise (SCENE_DOCUMENT_DESIGN.md section 7.11 (Swarm)): Emit From Box
//! fills the box's volume; Emit From a Group seeds from every part at its own
//! place; a force blows the same way in the world however the emitter turns;
//! a Group with Copies on drawn by a swarm keeps its copies.
//! `SCENE_SWARM_PNG_DIR=dir` saves one still per scene.

use manifold_core::Seconds;
use manifold_core::effects::PresetInstance;
use manifold_core::layer::Layer;
use manifold_core::scene_doc::{DrawAs, EmitFrom, ForceType, Item, ItemFields, ItemId, ItemKind, SceneDoc};
use manifold_core::{NodeId, PresetTypeId};
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::exec::effect_node::SimStep;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::gpu::headless_readback::readback_to_srgb_png;
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::particles::Particle;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;

const TICK: f64 = 1.0 / 60.0;
const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;

struct Run {
    runtime: PresetRuntime,
    target: RenderTarget,
    fx: PresetInstance,
    frame: u32,
}

impl Run {
    /// `doc` compiled as a scene layer, with `values` written to its card
    /// rows, dumping `dumps` (node ids).
    fn new(doc: SceneDoc, values: &[(&str, f32)], dumps: &[&str]) -> Self {
        let mut layer = Layer::new_generator("Scene".into(), PresetTypeId::from_string("Scene".into()), 0);
        layer.gen_params_mut().unwrap().install_scene_document(doc).unwrap();
        let mut fx = layer.gen_params().unwrap().clone();
        for &(id, value) in values {
            assert!(fx.set_base_param(id, value), "the scene has a {id} row");
        }
        let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
        let registry = PrimitiveRegistry::with_builtin();
        let mut runtime = PresetRuntime::from_def_for_render(fx.graph.clone().unwrap(), &registry, Some(&fx.params), false)
            .expect("the scene builds")
            .with_generator_device(std::sync::Arc::clone(device), WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float)
            .expect("the scene installs on the device");
        runtime.apply_param_values(&fx.params);
        let nodes: Vec<NodeId> = dumps.iter().map(|id| NodeId::new(*id)).collect();
        runtime.set_dump_arrays(None, &nodes);
        let target = RenderTarget::new(device, WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, "scene-swarm");
        Self { runtime, target, fx, frame: 0 }
    }

    fn advance(&mut self, frames: u32) {
        let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
        for _ in 0..frames {
            let seconds = f64::from(self.frame) * TICK;
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
            let mut encoder = device.create_encoder("scene-swarm");
            self.runtime.render(&mut GpuEncoder::new(&mut encoder, device), &self.target.texture, &context, &self.fx.params);
            encoder.commit_and_wait_completed();
            self.frame += 1;
        }
    }

    fn array<T: bytemuck::Pod>(&self, node: &str, port: &str) -> Vec<T> {
        let dumps = self.runtime.dump_arrays_all();
        let dump = dumps
            .iter()
            .find(|d| d.name.contains(node) && d.port == port)
            .unwrap_or_else(|| panic!("{node}.{port} is dumped: {:?}", dumps.iter().map(|d| (&d.name, &d.port)).collect::<Vec<_>>()));
        let count = (dump.buffer.size / std::mem::size_of::<T>() as u64) as usize;
        manifold_node_engine::testkit::array_harness::read(dump.buffer, count)
    }

    fn live(&self) -> Vec<Particle> {
        self.array::<Particle>("swarm:move", "out").into_iter().filter(|p| p.life > 0.0).collect()
    }

    fn still(&self, name: &str) {
        if let Ok(dir) = std::env::var("SCENE_SWARM_PNG_DIR") {
            let png = readback_to_srgb_png(&manifold_node_engine::testkit::gpu_harness::shared().device, &self.target.texture, WIDTH, HEIGHT);
            std::fs::write(std::path::Path::new(&dir).join(format!("{name}.png")), png).unwrap();
        }
    }
}

fn add(doc: &mut SceneDoc, parent: Option<&ItemId>, id: &str, fields: ItemFields) {
    let at = parent.and_then(|p| doc.find(p)).map_or(doc.items.len(), |p| p.children.len());
    doc.insert(parent, at, Item::new(ItemId::new(id), id, fields)).unwrap();
}

fn swarm(doc: &mut SceneDoc, emit_from: EmitFrom, draw_as: DrawAs) {
    let ItemFields::Swarm(mut fields) = ItemFields::default_for(ItemKind::Swarm) else { unreachable!() };
    fields.emit_from = emit_from;
    fields.draw_as = draw_as;
    add(doc, None, "swarm", ItemFields::Swarm(fields));
}

fn object(doc: &mut SceneDoc, parent: &str, id: &str) {
    add(doc, Some(&ItemId::new(parent)), id, ItemFields::default_for(ItemKind::Object));
}

fn group(doc: &mut SceneDoc, id: &str, copies: bool) {
    let ItemFields::Group(mut fields) = ItemFields::default_for(ItemKind::Group) else { unreachable!() };
    fields.copies = copies;
    add(doc, None, id, ItemFields::Group(fields));
}

/// The base scene's cube is hidden so only the swarm draws.
const NO_CUBE: (&str, f32) = ("cube.visible", 0.0);

#[test]
fn box_emitter_fills_its_volume() {
    let mut doc = SceneDoc::base();
    swarm(&mut doc, EmitFrom::Box, DrawAs::Cube);
    let mut run = Run::new(doc, &[NO_CUBE, ("swarm.size", 0.06)], &["swarm:move"]);
    run.advance(2);
    let live = run.live();
    // The seed box is four metres: a surface scatter leaves its inner
    // half-size box empty, a volume fill puts an eighth of the swarm there.
    let inner = live.iter().filter(|p| p.position.iter().all(|c| c.abs() < 1.0)).count() as f32 / live.len() as f32;
    println!("SCENE_SWARM box live={} inner_fraction={inner:.3}", live.len());
    run.still("box_volume");
    assert!(live.len() > 10_000, "the swarm is alive: {}", live.len());
    assert!((inner - 0.125).abs() < 0.03, "an eighth of the swarm sits in the inner box: {inner}");
}

#[test]
fn group_emitter_seeds_from_every_part() {
    let mut doc = SceneDoc::base();
    group(&mut doc, "pair", false);
    object(&mut doc, "pair", "left");
    object(&mut doc, "pair", "right");
    swarm(&mut doc, EmitFrom::Item(ItemId::new("pair")), DrawAs::Tetrahedron);
    let values = [NO_CUBE, ("left.pos_x", -2.5), ("right.pos_x", 2.5), ("right.pos_y", 1.0), ("pair.pos_y", 1.0), ("swarm.size", 0.05)];
    let mut run = Run::new(doc, &values, &["swarm:move"]);
    run.advance(2);
    let live = run.live();
    // Positions are in the group's frame: each part's own place in it.
    let left = live.iter().filter(|p| (p.position[0] + 2.5).abs() <= 0.55 && p.position[1].abs() <= 0.55).count();
    let right = live.iter().filter(|p| (p.position[0] - 2.5).abs() <= 0.55 && (p.position[1] - 1.0).abs() <= 0.55).count();
    println!("SCENE_SWARM group live={} left={left} right={right}", live.len());
    run.still("group_parts");
    assert!(left + right == live.len(), "every particle starts on a part: {} of {}", left + right, live.len());
    assert!(left > live.len() / 3 && right > live.len() / 3, "both parts emit: left {left}, right {right}");
}

#[test]
fn a_force_blows_the_same_way_however_the_emitter_turns() {
    // The mean world velocity after half a second, the emitter turned `turn` about y.
    let world_velocity = |turn: f32, still: Option<&str>| {
        let mut doc = SceneDoc::base();
        swarm(&mut doc, EmitFrom::Box, DrawAs::Octahedron);
        let ItemFields::Force(mut force) = ItemFields::default_for(ItemKind::Force) else { unreachable!() };
        force.force = ForceType::Uniform;
        add(&mut doc, None, "push", ItemFields::Force(force));
        let values = [
            NO_CUBE,
            ("swarm.rot_y", turn),
            ("push.direction_x", 1.0),
            ("push.direction_y", 0.0),
            ("push.direction_z", 0.0),
            ("swarm.size", 0.05),
        ];
        let mut run = Run::new(doc, &values, &["swarm:move"]);
        run.advance(30);
        let live = run.live();
        let mean = |axis: usize| live.iter().map(|p| p.velocity[axis]).sum::<f32>() / live.len() as f32;
        if let Some(name) = still {
            run.still(name);
        }
        // Turned a quarter about y, the emitter's x is world -z and its z world x.
        let (sin, cos) = turn.sin_cos();
        [cos * mean(0) + sin * mean(2), mean(1), cos * mean(2) - sin * mean(0)]
    };
    let still = world_velocity(0.0, None);
    let turned = world_velocity(std::f32::consts::FRAC_PI_2, Some("force_world_space"));
    println!("SCENE_SWARM force still={still:?} turned={turned:?}");
    assert!(still[0] > 0.2, "the push carries the swarm along world +x: {still:?}");
    for axis in 0..3 {
        assert!(
            (turned[axis] - still[axis]).abs() < 0.1 * still[0],
            "the turned emitter's swarm moves as the unturned one does: {turned:?} vs {still:?}"
        );
    }
}

#[test]
fn a_drawn_group_keeps_its_copies() {
    let mut doc = SceneDoc::base();
    group(&mut doc, "row", true);
    object(&mut doc, "row", "block");
    swarm(&mut doc, EmitFrom::Box, DrawAs::Item(ItemId::new("row")));
    let values = [NO_CUBE, ("row.count", 3.0), ("swarm.count", 400.0), ("swarm.size", 0.15)];
    let mut run = Run::new(doc, &values, &["swarm:copies", "swarm:nest"]);
    run.advance(3);
    let particles: Vec<InstanceTransform> = run.array("swarm:copies", "copies");
    let nested: Vec<InstanceTransform> = run.array("swarm:nest", "copies");
    let live: Vec<usize> = (0..particles.len()).filter(|&i| particles[i].pos_scale[3] > 0.0).collect();
    println!("SCENE_SWARM copies particles={} live={} nested={}", particles.len(), live.len(), nested.len());
    run.still("group_copies");
    assert!(live.len() >= 300, "the swarm is alive: {}", live.len());
    assert!(nested.len() >= particles.len() * 3, "three copies a particle: {} for {}", nested.len(), particles.len());
    for &i in live.iter().take(50) {
        let copies = &nested[i * 3..i * 3 + 3];
        let shown: Vec<[f32; 4]> = copies.iter().map(|c| c.pos_scale).collect();
        let apart = |a: &InstanceTransform, b: &InstanceTransform| (0..3).map(|d| (a.pos_scale[d] - b.pos_scale[d]).powi(2)).sum::<f32>().sqrt();
        assert!(copies.iter().all(|c| c.pos_scale[3] > 0.0), "particle {i}'s copies are drawn: {shown:?}");
        assert!(apart(&copies[0], &copies[1]) > 0.01 && apart(&copies[1], &copies[2]) > 0.01, "particle {i} carries three separate copies: {shown:?}");
        let centre: [f32; 3] = std::array::from_fn(|d| copies.iter().map(|c| c.pos_scale[d]).sum::<f32>() / 3.0);
        assert!(apart(&InstanceTransform { pos_scale: [centre[0], centre[1], centre[2], 1.0], rot_pad: [0.0; 4] }, &particles[i]) < 5.0, "particle {i}'s copies sit around it");
    }
}
