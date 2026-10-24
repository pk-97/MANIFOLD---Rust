//! The renderer's instance space (scene groups design D10).
//!
//! A two-part group copied three times in a Ring: both parts read the same
//! copies buffer in Group space, so every copy draws every part at
//! parent · instance · part. A turned echo object beside them keeps Object
//! space, world = local · instance, the rule every echo, scatter and swarm
//! uses today. The expected pixels come from `Camera::project_to_pixel` on
//! positions computed with plain trigonometry here, not from the matrix
//! code under test.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::scene::camera::Camera;

const CAMERA_POS: [f32; 3] = [0.0, 11.0, 7.5];
const FOV_Y: f32 = 1.0;
/// The copies: a Ring of three, radius 3, each copy facing along the ring and
/// turned a further `COPY_YAW`.
const COPIES: usize = 3;
const RING_RADIUS: f32 = 3.0;
const COPY_YAW: f32 = 2.6;
/// The group's parent transform and the two parts' places inside it.
const PARENT: [f32; 3] = [0.0, 0.0, 0.5];
const PART_RED: [f32; 3] = [1.5, 0.0, 0.0];
const PART_GREEN: [f32; 3] = [-1.5, 0.0, 0.0];
/// The echo: three copies on a radius-1 ring inside an object turned by `ECHO_YAW`.
const ECHO_RADIUS: f32 = 1.0;
const ECHO_YAW: f32 = 0.8;

/// A Ring of [`COPIES`] at `radius`, each copy turned by `turn_y` beyond the
/// ring's own facing. With Columns = COPIES and a 1 m copy, neighbours sit
/// Spacing apart on the chord, so Spacing = radius · 2 sin(π / COPIES).
fn copies_node(id: u32, node_id: &str, radius: f32, turn_y: f32) -> String {
    let spacing = radius * 2.0 * (std::f32::consts::PI / COPIES as f32).sin();
    format!(
        r#"{{"id":{id},"typeId":"node.arrange_copies","nodeId":"{node_id}","params":{{
            "max_capacity":{{"type":"Int","value":{COPIES}}},
            "active_count":{{"type":"Int","value":{COPIES}}},
            "layout":{{"type":"Enum","value":3}},
            "columns":{{"type":"Int","value":{COPIES}}},
            "spacing":{{"type":"Float","value":{spacing}}},
            "box_z":{{"type":"Float","value":0.5}},
            "turn_y":{{"type":"Float","value":{turn_y}}}}}}}"#
    )
}

fn quad_nodes(id: u32, name: &str, size: f32) -> String {
    format!(
        r#"{{"id":{id},"typeId":"node.grid_mesh","nodeId":"{name}_grid","params":{{
            "max_capacity":{{"type":"Int","value":64}},
            "resolution_x":{{"type":"Int","value":2}},
            "resolution_y":{{"type":"Int","value":2}},
            "size_x":{{"type":"Float","value":{size}}},
            "size_y":{{"type":"Float","value":{size}}}}}}},
        {{"id":{tris},"typeId":"node.make_triangles","nodeId":"{name}_tris","params":{{
            "src_cols":{{"type":"Int","value":2}},
            "src_rows":{{"type":"Int","value":2}}}}}}"#,
        tris = id + 1,
    )
}

fn material_node(id: u32, name: &str, rgb: [f32; 3]) -> String {
    format!(
        r#"{{"id":{id},"typeId":"node.unlit_material","nodeId":"{name}","params":{{
            "color_r":{{"type":"Float","value":{}}},
            "color_g":{{"type":"Float","value":{}}},
            "color_b":{{"type":"Float","value":{}}}}}}}"#,
        rgb[0], rgb[1], rgb[2]
    )
}

fn transform_node(id: u32, name: &str, pos: [f32; 3], rot_y: f32) -> String {
    format!(
        r#"{{"id":{id},"typeId":"node.transform_3d","nodeId":"{name}","params":{{
            "pos_x":{{"type":"Float","value":{}}},
            "pos_y":{{"type":"Float","value":{}}},
            "pos_z":{{"type":"Float","value":{}}},
            "rot_y":{{"type":"Float","value":{rot_y}}}}}}}"#,
        pos[0], pos[1], pos[2]
    )
}

fn object_node(id: u32, name: &str, space: u32) -> String {
    format!(
        r#"{{"id":{id},"typeId":"node.scene_object","nodeId":"{name}","params":{{
            "instances_space":{{"type":"Enum","value":{space}}}}}}}"#
    )
}

fn scene_json() -> String {
    let nodes = [
        r#"{"id":0,"typeId":"system.generator_input","nodeId":"input"}"#.to_string(),
        format!(
            r#"{{"id":1,"typeId":"node.look_at_camera","nodeId":"cam","params":{{
            "pos_x":{{"type":"Float","value":{}}},
            "pos_y":{{"type":"Float","value":{}}},
            "pos_z":{{"type":"Float","value":{}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}}}}}}"#,
            CAMERA_POS[0], CAMERA_POS[1], CAMERA_POS[2]
        ),
        copies_node(2, "copies", RING_RADIUS, COPY_YAW),
        transform_node(3, "group", PARENT, 0.0),
        quad_nodes(4, "red", 0.8),
        material_node(6, "red_mat", [1.0, 0.0, 0.0]),
        transform_node(7, "red_part", PART_RED, 0.0),
        object_node(8, "red_obj", 1),
        quad_nodes(10, "green", 0.8),
        material_node(12, "green_mat", [0.0, 1.0, 0.0]),
        transform_node(13, "green_part", PART_GREEN, 0.0),
        object_node(14, "green_obj", 1),
        copies_node(16, "echoes", ECHO_RADIUS, 0.0),
        quad_nodes(17, "blue", 0.5),
        material_node(19, "blue_mat", [0.0, 0.0, 1.0]),
        transform_node(20, "echo_turn", [0.0; 3], ECHO_YAW),
        object_node(21, "blue_obj", 0),
        r#"{"id":30,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":3},
            "lights":{"type":"Int","value":0}}}"#
            .to_string(),
        r#"{"id":99,"typeId":"system.final_output","nodeId":"out"}"#.to_string(),
    ]
    .join(",");
    let wire = |from: u32, from_port: &str, to: u32, to_port: &str| {
        format!(
            r#"{{"fromNode":{from},"fromPort":"{from_port}","toNode":{to},"toPort":"{to_port}"}}"#
        )
    };
    let mut wires = vec![
        wire(1, "out", 30, "camera"),
        wire(30, "color", 99, "in"),
        wire(16, "instances", 21, "instances"),
        wire(17, "vertices", 18, "in"),
        wire(18, "out", 21, "vertices"),
        wire(19, "out", 21, "material"),
        wire(20, "transform", 21, "transform"),
        wire(21, "object", 30, "object_2"),
    ];
    for (mesh, material, part, object, slot) in [(4, 6, 7, 8, 0), (10, 12, 13, 14, 1)] {
        wires.extend([
            wire(mesh, "vertices", mesh + 1, "in"),
            wire(mesh + 1, "out", object, "vertices"),
            wire(material, "out", object, "material"),
            wire(part, "transform", object, "transform"),
            wire(3, "transform", object, "parent_transform"),
            wire(2, "instances", object, "instances"),
            wire(object, "object", 30, &format!("object_{slot}")),
        ]);
    }
    format!(
        r#"{{"version":2,"name":"RenderSceneInstanceSpace","nodes":[{nodes}],"wires":[{}]}}"#,
        wires.join(",")
    )
}

fn render() -> (Vec<u8>, u32, u32) {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &manifold_nodes_scene::testkit::scene_objects::bundle_scene_objects(&scene_json()),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("instance-space scene graph must build");
    let target = h.make_target("render-scene-instance-space");
    for frame in 0..2 {
        let ctx = PresetContext {
            time: 0.1,
            beat: 0.2,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("render-scene-instance-space-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    }
    (h.readback(&target.texture), h.width, h.height)
}

fn write_png(bytes: &[u8], w: u32, h: u32, path: &str) {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for px in bytes.chunks_exact(8) {
        for c in 0..4 {
            let v = f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32();
            out.push(((v / (1.0 + v)).clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8);
        }
    }
    image::save_buffer(path, &out, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("write {path}: {e}"));
}

/// The colour channel (0 red, 1 green, 2 blue) dominating the pixel under
/// `world`, or `None` for background.
fn colour_at(bytes: &[u8], w: u32, h: u32, camera: &Camera, world: [f32; 3]) -> Option<usize> {
    let p = camera
        .project_to_pixel(world, w, h)
        .expect("in front of the camera");
    let (x, y) = (p.px.floor() as u32, p.py.floor() as u32);
    assert!(
        x < w && y < h,
        "{world:?} projects outside the frame ({x}, {y})"
    );
    let i = ((y * w + x) * 8) as usize;
    let rgb: Vec<f32> = (0..3)
        .map(|c| f16::from_le_bytes([bytes[i + c * 2], bytes[i + c * 2 + 1]]).to_f32())
        .collect();
    (0..3).find(|&c| rgb[c] > 0.5 && (0..3).all(|o| o == c || rgb[o] < 0.2))
}

/// y-axis turn, the euler_xyz convention with only `rot_y` set.
fn yaw(angle: f32, v: [f32; 3]) -> [f32; 3] {
    let (s, c) = angle.sin_cos();
    [c * v[0] + s * v[2], v[1], -s * v[0] + c * v[2]]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// `node.arrange_copies` Ring slot `i` of [`COPIES`] at `radius`.
fn ring(i: usize, radius: f32) -> [f32; 3] {
    let theta = i as f32 / COPIES as f32 * std::f32::consts::TAU;
    [theta.cos() * radius, 0.0, theta.sin() * radius]
}

/// Ring copy `i`'s turn about Y: its X runs along the ring, then `turn_y`.
fn facing(i: usize, turn_y: f32) -> f32 {
    let theta = i as f32 / COPIES as f32 * std::f32::consts::TAU;
    turn_y - (theta + std::f32::consts::FRAC_PI_2)
}

#[test]
fn group_space_copies_draw_every_part_and_object_space_echoes_stay_put() {
    let (bytes, w, h) = render();
    write_png(&bytes, w, h, "/tmp/render_scene_instance_space.png");
    let camera = Camera::look_at(CAMERA_POS, [0.0; 3], [0.0, 1.0, 0.0], FOV_Y, 0.05, 200.0);
    for i in 0..COPIES {
        for (part, colour, name) in [(PART_RED, 0, "red"), (PART_GREEN, 1, "green")] {
            // Group space: parent · instance · part.
            let right = add(
                PARENT,
                add(ring(i, RING_RADIUS), yaw(facing(i, COPY_YAW), part)),
            );
            assert_eq!(
                colour_at(&bytes, w, h, &camera, right),
                Some(colour),
                "copy {i} draws its {name} part at {right:?}"
            );
            // Object space would have put it at parent · part · instance.
            let wrong = add(PARENT, add(part, ring(i, RING_RADIUS)));
            assert_ne!(
                colour_at(&bytes, w, h, &camera, wrong),
                Some(colour),
                "copy {i} {name} part is not at the Object-space place {wrong:?}"
            );
        }
        // Object space: local · instance, the existing echo rule.
        let echo = yaw(ECHO_YAW, ring(i, ECHO_RADIUS));
        assert_eq!(
            colour_at(&bytes, w, h, &camera, echo),
            Some(2),
            "echo {i} stays at local · instance {echo:?}"
        );
        let group_rule = ring(i, ECHO_RADIUS);
        assert_ne!(
            colour_at(&bytes, w, h, &camera, group_rule),
            Some(2),
            "echo {i} is not at instance · local {group_rule:?}"
        );
    }
}
