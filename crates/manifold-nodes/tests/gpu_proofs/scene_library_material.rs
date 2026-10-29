//! A library material through the real renderer: the base scene's cube, a
//! long box and a sphere wear a real ambientCG brick material read from its
//! MaterialX file (`tests/fixtures/materials/Bricks090`, pictures shrunk to
//! 512 px), and the brick picture reaches the cube's face.
//! `SCENE_MATERIAL_PNG_DIR=dir` saves the stills, box-mapped ones among them
//! and one over a ray-traced mirror floor.

use std::path::Path;

use manifold_core::effects::PresetInstance;
use manifold_core::layer::Layer;
use manifold_core::scene_doc::{
    BASE_CUBE_MATERIAL, Item, ItemFields, ItemId, ItemKind, SceneDoc, Shape, TextureSource,
};
use manifold_core::{PresetTypeId, Seconds};
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::exec::effect_node::SimStep;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::gpu::headless_readback::{encode_rgba8_png, readback_tonemapped_rgba8};
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;

const TICK: f64 = 1.0 / 60.0;
const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;

fn brick_file() -> String {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/materials/Bricks090/Bricks090_2K-JPG.mtlx"
    )
    .to_owned()
}

/// The base scene with a long box and a sphere beside its cube, on a wide
/// floor, every one drawing with the cube's Material, whose texture is
/// `texture`. A `mirror` floor draws with its own Material, `mirror`.
fn scene(texture: TextureSource, mirror: bool) -> SceneDoc {
    let mut doc = SceneDoc::base();
    if let TextureSource::Model { file, .. } = &texture {
        let (facts, warnings) =
            manifold_nodes_scene::node_graph::gltf_import::read_file_facts(Path::new(file))
                .expect("the brick material reads");
        assert!(
            warnings.is_empty(),
            "nothing in the file is dropped: {warnings:?}"
        );
        doc.models.insert(file.clone(), facts);
    }
    let Some(Item {
        fields: ItemFields::Material(material),
        ..
    }) = doc.find_mut(&ItemId::new(BASE_CUBE_MATERIAL))
    else {
        panic!("the base scene has the cube's Material")
    };
    material.texture = texture;
    if mirror {
        let at = doc.items.len();
        doc.insert(
            None,
            at,
            Item::new(
                ItemId::new("mirror"),
                "mirror",
                ItemFields::default_for(ItemKind::Material),
            ),
        )
        .unwrap();
    }
    for (id, shape) in [
        ("long", Shape::Cube),
        ("ball", Shape::Sphere),
        ("floor", Shape::Cube),
    ] {
        let ItemFields::Object(mut fields) = ItemFields::default_for(ItemKind::Object) else {
            unreachable!()
        };
        fields.shape = shape;
        fields.material = Some(ItemId::new(if mirror && id == "floor" {
            "mirror"
        } else {
            BASE_CUBE_MATERIAL
        }));
        let at = doc.items.len();
        doc.insert(
            None,
            at,
            Item::new(ItemId::new(id), id, ItemFields::Object(fields)),
        )
        .unwrap();
    }
    doc
}

/// Render `doc` with card `rows` written until every picture has loaded, as
/// the load pre-roll does, and for at least `frames` frames, and read the
/// frame back as RGBA8.
fn render(doc: SceneDoc, rows: &[(&str, f32)], still: &str, frames: u32) -> Vec<u8> {
    let mut layer =
        Layer::new_generator("Scene".into(), PresetTypeId::from_string("Scene".into()), 0);
    layer
        .gen_params_mut()
        .unwrap()
        .install_scene_document(doc)
        .unwrap();
    let mut fx: PresetInstance = layer.gen_params().unwrap().clone();
    for (id, value) in [
        ("cube.pos_x", -1.6),
        ("long.pos_x", 0.9),
        ("long.scale_x", 2.2),
        ("long.scale_y", 0.6),
        ("long.pos_y", -0.2),
        ("ball.pos_x", 3.4),
        ("floor.pos_y", -0.55),
        ("floor.scale_x", 16.0),
        ("floor.scale_y", 0.1),
        ("floor.scale_z", 16.0),
        ("camera.distance", 9.0),
    ]
    .iter()
    .chain(rows)
    {
        assert!(fx.set_base_param(id, *value), "the scene has a {id} row");
    }
    let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_def_for_render(
        fx.graph.clone().unwrap(),
        &registry,
        Some(&fx.params),
        false,
    )
    .expect("the scene builds")
    .with_generator_device(
        std::sync::Arc::clone(device),
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
    )
    .expect("the scene installs on the device");
    runtime.apply_param_values(&fx.params);
    let target = RenderTarget::new(
        device,
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "scene-library-material",
    );
    let mut frame = 0u32;
    loop {
        let seconds = f64::from(frame) * TICK;
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
            frame_count: i64::from(frame),
            anim_progress: 0.0,
            trigger_count: 0,
        };
        runtime.set_sim_step(SimStep::live(Seconds(TICK)));
        let mut encoder = device.create_encoder("scene-library-material");
        runtime.render(
            &mut GpuEncoder::new(&mut encoder, device),
            &target.texture,
            &context,
            &fx.params,
        );
        encoder.commit_and_wait().expect("GPU work completes");
        frame += 1;
        if frame > frames && !runtime.warmup_pending() {
            break;
        }
        assert!(frame < 2000, "the pictures load");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let rgba = readback_tonemapped_rgba8(device, &target.texture, WIDTH, HEIGHT)
        .expect("GPU readback completes");
    if let Ok(dir) = std::env::var("SCENE_MATERIAL_PNG_DIR") {
        std::fs::write(
            Path::new(&dir).join(format!("{still}.png")),
            encode_rgba8_png(&rgba, WIDTH, HEIGHT),
        )
        .unwrap();
    }
    rgba
}

/// Luma spread over a patch inside the cube's lit side face.
fn face_spread(rgba: &[u8]) -> f64 {
    let (cx, cy, half) = (
        WIDTH as usize * 41 / 100,
        HEIGHT as usize * 44 / 100,
        20usize,
    );
    let lumas: Vec<f64> = (cy - half..cy + half)
        .flat_map(|y| (cx - half..cx + half).map(move |x| (y * WIDTH as usize + x) * 4))
        .map(|i| {
            0.2126 * f64::from(rgba[i])
                + 0.7152 * f64::from(rgba[i + 1])
                + 0.0722 * f64::from(rgba[i + 2])
        })
        .collect();
    let mean = lumas.iter().sum::<f64>() / lumas.len() as f64;
    (lumas.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / lumas.len() as f64).sqrt()
}

/// The brick picture, its bumps and its roughness reach the cube: its face
/// shows mortar lines a plain face does not.
#[test]
fn a_materialx_brick_material_draws_its_pictures() {
    let brick_doc = |mirror| {
        scene(
            TextureSource::Model {
                file: brick_file(),
                material: Some(0),
            },
            mirror,
        )
    };
    let brick = render(brick_doc(false), &[], "brick", 4);
    if std::env::var("SCENE_MATERIAL_PNG_DIR").is_ok() {
        let boxed = [("cube-material.mapping", 1.0)];
        render(brick_doc(false), &boxed, "brick_box_only", 4);
        render(
            brick_doc(false),
            &[
                ("cube-material.mapping", 1.0),
                ("cube-material.break_repeats", 0.5),
            ],
            "brick_box_break",
            4,
        );
        let mirror = [
            ("cube-material.mapping", 1.0),
            ("mirror.metallic", 1.0),
            ("mirror.roughness", 0.02),
            ("world.rt_enabled", 1.0),
            ("world.rt_reflections", 1.0),
        ];
        render(brick_doc(true), &mirror, "brick_box_rt", 90);
        let mut broken = mirror.to_vec();
        broken.push(("cube-material.break_repeats", 0.5));
        render(brick_doc(true), &broken, "brick_break_rt", 90);
        // `SCENE_MATERIAL_LIBRARY=1` adds a still of every starter material
        // at its file's size and repeats, as picking it starts them.
        if std::env::var("SCENE_MATERIAL_LIBRARY").is_ok() {
            let library = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/materials");
            for folder in std::fs::read_dir(library).unwrap() {
                let folder = folder.unwrap().path();
                let Some(Ok(file)) = std::fs::read_dir(&folder).ok().and_then(|mut entries| {
                    entries.find_map(|e| {
                        let path = e.ok()?.path();
                        (path.extension()? == "mtlx").then_some(Ok::<_, ()>(path))
                    })
                }) else {
                    continue;
                };
                let m = manifold_core::materialx::read(&file).unwrap();
                let doc = scene(
                    TextureSource::Model {
                        file: file.to_string_lossy().into_owned(),
                        material: Some(0),
                    },
                    false,
                );
                let rows = [
                    ("cube-material.mapping", 1.0),
                    ("cube-material.tile_size", m.tile_size.unwrap()),
                    (
                        "cube-material.break_repeats",
                        if m.hex_tiled { 0.5 } else { 0.0 },
                    ),
                    ("cube-material.depth", m.depth.unwrap() * 100.0),
                ];
                let name = folder.file_name().unwrap().to_string_lossy().into_owned();
                render(doc.clone(), &rows, &name, 4);
                // Close up: carved and flat, on the box and on the meshes'
                // own UVs, and the long box sunk 5 cm into a floor carved
                // 10 cm, where the floor's relief should cut it.
                let close = [("camera.distance", 3.5), ("camera.tilt", 0.2)];
                let carved: Vec<_> = rows.iter().chain(&close).copied().collect();
                for (suffix, extra) in [
                    ("close", vec![]),
                    ("close_flat", vec![("cube-material.depth", 0.0)]),
                    ("close_uv", vec![("cube-material.mapping", 0.0)]),
                    (
                        "close_uv_flat",
                        vec![("cube-material.mapping", 0.0), ("cube-material.depth", 0.0)],
                    ),
                    (
                        "sunk",
                        vec![("cube-material.depth", 10.0), ("long.pos_y", -0.25)],
                    ),
                ] {
                    let all: Vec<_> = carved.iter().copied().chain(extra).collect();
                    render(doc.clone(), &all, &format!("{name}_{suffix}"), 4);
                }
            }
        }
    }
    let plain = render(scene(TextureSource::None, false), &[], "plain", 4);
    let (brick, plain) = (face_spread(&brick), face_spread(&plain));
    assert!(
        brick > 4.0 * plain.max(0.5),
        "the brick face varies ({brick:.2}) where the plain face is flat ({plain:.2})"
    );
}
