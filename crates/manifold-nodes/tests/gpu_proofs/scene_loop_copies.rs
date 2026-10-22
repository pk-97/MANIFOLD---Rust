//! Scene Loop tiles a Copies grid and a Swarm like a plain object
//! (SCENE_DOCUMENT_DESIGN.md section 7.19 (Modifiers)): the Loop flyer sees
//! the same picture at beat 0 and at beat 8, one loop later, and an orbit
//! camera sees every tile carry the grid and the swarm, each mirrored
//! across the floor.
//! `SCENE_LOOP_COPIES_PNG_DIR=dir` saves each still.

use manifold_core::Seconds;
use manifold_core::layer::Layer;
use manifold_core::scene_doc::{CameraRig, Item, ItemFields, ItemId, ItemKind, ModifierFields, ModifierTargets, ModifierType, SceneDoc};
use manifold_core::PresetTypeId;
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::exec::effect_node::SimStep;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::gpu::headless_readback::readback_to_srgb_png;
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;

const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;
const TICK: f64 = 1.0 / 60.0;

/// A cube with a 3 x 3 Copies grid, a swarm around it, a Scene Loop and a
/// Scene Mirror across the floor.
fn scene(rig: CameraRig) -> SceneDoc {
    let mut doc = SceneDoc::base();
    if let Some(Item { fields: ItemFields::Object(cube), .. }) = doc.find_mut(&ItemId::new("cube")) {
        cube.copies = true;
    }
    if let Some(Item { fields: ItemFields::Camera(camera), .. }) = doc.find_mut(&ItemId::new("camera")) {
        camera.rig = rig;
    }
    let end = doc.items.len();
    doc.insert(None, end, Item::new(ItemId::new("swarm"), "Swarm", ItemFields::default_for(ItemKind::Swarm))).unwrap();
    for (k, (id, modifier)) in [("loop", ModifierType::SceneLoop), ("mirror", ModifierType::SceneMirror)].into_iter().enumerate() {
        let mut card = ItemFields::default_for(ItemKind::Modifier);
        if let ItemFields::Modifier(fields) = &mut card {
            *fields = ModifierFields { modifier, on: true, targets: ModifierTargets::Scene, ..fields.clone() };
        }
        doc.insert(None, end + 1 + k, Item::new(ItemId::new(id), id, card)).unwrap();
    }
    doc
}

/// The scene drawn at `beat` after a few frames there, as sRGB PNG bytes.
/// World Speed 0 holds the swarm at its seed, so only the camera moves. A
/// 14 m cell keeps the 3 x 3 grid and the 6 m swarm apart
/// from the next tile, and the far plane shows eight tiles.
fn still(doc: SceneDoc, values: &[(&str, f32)], beat: f64) -> Vec<u8> {
    let mut layer = Layer::new_generator("Scene".into(), PresetTypeId::from_string("Scene".into()), 0);
    layer.gen_params_mut().unwrap().install_scene_document(doc).unwrap();
    let mut fx = layer.gen_params().unwrap().clone();
    for &(id, value) in [("world.speed", 0.0), ("cube.count", 9.0), ("cube.columns", 3.0), ("swarm.count", 400.0), ("swarm.size", 0.05), ("loop.cell_size", 14.0), ("loop.far", 120.0)]
        .iter()
        .chain(values)
    {
        assert!(fx.set_base_param(id, value), "the scene has a {id} row");
    }
    let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_def_for_render(fx.graph.clone().unwrap(), &registry, Some(&fx.params), false)
        .expect("the scene builds")
        .with_generator_device(std::sync::Arc::clone(device), WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float)
        .expect("the scene installs on the device");
    runtime.apply_param_values(&fx.params);
    let target = RenderTarget::new(device, WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, "scene-loop-copies");
    for frame in 0..4 {
        let context = PresetContext {
            time: beat / 2.0,
            beat,
            dt: TICK as f32,
            width: WIDTH,
            height: HEIGHT,
            output_width: WIDTH,
            output_height: HEIGHT,
            aspect: WIDTH as f32 / HEIGHT as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        runtime.set_sim_step(SimStep::live(Seconds(TICK)));
        let mut encoder = device.create_encoder("scene-loop-copies");
        runtime.render(&mut GpuEncoder::new(&mut encoder, device), &target.texture, &context, &fx.params);
        encoder.commit_and_wait_completed();
    }
    readback_to_srgb_png(device, &target.texture, WIDTH, HEIGHT)
}

/// The flyer rides above the grid and the swarm.
const FLY_HEIGHT: (&str, f32) = ("camera.height", 3.0);

fn save(name: &str, png: &[u8]) {
    if let Ok(dir) = std::env::var("SCENE_LOOP_COPIES_PNG_DIR") {
        std::fs::write(std::path::Path::new(&dir).join(format!("{name}.png")), png).unwrap();
    }
}

#[test]
fn the_loop_wraps_a_copies_grid_and_a_swarm_cleanly() {
    let start = still(scene(CameraRig::Loop), &[FLY_HEIGHT], 0.0);
    let wrapped = still(scene(CameraRig::Loop), &[FLY_HEIGHT], 8.0);
    let midway = still(scene(CameraRig::Loop), &[FLY_HEIGHT], 3.0);
    let orbit = still(scene(CameraRig::Orbit), &[("camera.distance", 60.0)], 0.0);
    save("loop_beat0", &start);
    save("loop_beat8", &wrapped);
    save("loop_beat3", &midway);
    save("orbit_beat0", &orbit);
    assert!(start == wrapped, "one loop later the Loop flyer sees the same picture");
    assert!(start != midway, "the flyer moves between the wraps");
}
