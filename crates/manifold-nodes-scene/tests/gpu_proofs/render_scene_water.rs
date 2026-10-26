//! `node.render_scene` water proofs: a level set wired into a
//! `node.scene_object`'s `field` port is marched and shaded through the real
//! render path.
//!
//! Scene shape: a top-down `node.orbit_camera` over an unlit floor of one
//! constant colour, a black environment and no lights, so the water shows
//! only the floor seen through it. The water is a box level set whose top
//! steps from 0.5 m to 2.5 m deep at x = 0, resting on the floor, with
//! Pope & Fry 1997 absorption (sigma = 0.34, 0.057, 0.009 per metre).

use half::f16;
use manifold_gpu::{GpuBuffer, GpuTexture, GpuTextureFormat};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::scene::camera::Camera;
use manifold_node_engine::scene::depth_rule::DepthRule;
use manifold_node_engine::scene::pick::{PickHit, PickRequest};
use manifold_node_engine::{
    exec::effect_node::EffectNode, exec::effect_node::EffectNodeContext,
    exec::effect_node::EffectNodeType, exec::effect_node::ParamValues, parameters::ParamDef,
    persistence::PrimitiveRegistry, ports::ArrayType, ports::NodeInput, ports::NodeOutput,
    ports::NodePort, ports::PortKind, ports::PortType,
};

/// Water's absorption per metre for red, green, blue.
const SIGMA: [f32; 3] = [0.34, 0.057, 0.009];
const FLOOR_COLOR: [f32; 3] = [0.8, 0.7, 0.6];
const FLOOR_Y: f32 = -2.5;
const SHALLOW_TOP: f32 = -2.0;
const DEEP_TOP: f32 = 0.0;
/// Lattice centre and size; the bottom face sits on the floor.
const LATTICE_CENTER: [f32; 3] = [0.0, -1.0, 0.0];
const LATTICE_SIZE: [f32; 3] = [10.0, 3.0, 10.0];
const LATTICE_NODES: [usize; 3] = [40, 13, 40];
const CAMERA: (f32, f32, f32, f32) = (0.0, 1.5, 8.0, 0.6);
/// Half the width of the pool the water fills in `pool_levelset`, along z.
const POOL_HALF_WIDTH: f32 = 1.2;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;

const LEVELSET_TYPE: &str = "test.water_levelset";
const SINK_TYPE: &str = "test.texture_sink";

/// Test-only source of a hand-built level set `Array(f32)`.
struct LevelsetSource {
    type_id: EffectNodeType,
    outputs: Vec<NodeOutput>,
    values: Vec<f32>,
    staging: Option<GpuBuffer>,
}

impl LevelsetSource {
    fn new(values: Vec<f32>) -> Self {
        Self {
            type_id: EffectNodeType::new(LEVELSET_TYPE),
            outputs: vec![NodePort {
                name: std::borrow::Cow::Borrowed("field"),
                ty: PortType::Array(ArrayType::of_known::<f32>()),
                kind: PortKind::Output,
                required: false,
            }],
            values,
            staging: None,
        }
    }
}

impl EffectNode for LevelsetSource {
    fn depth_rule(&self) -> DepthRule {
        DepthRule::Terminal
    }

    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }

    fn inputs(&self) -> &[NodeInput] {
        &[]
    }

    fn outputs(&self) -> &[NodeOutput] {
        &self.outputs
    }

    fn parameters(&self) -> &[ParamDef] {
        &[]
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(dst) = ctx.outputs.array("field") else {
            return;
        };
        let bytes = bytemuck::cast_slice(self.values.as_slice());
        let staging = self.staging.get_or_insert_with(|| {
            ctx.gpu_encoder()
                .device
                .create_buffer_shared(bytes.len() as u64)
        });
        unsafe { staging.write(0, bytes) };
        ctx.gpu_encoder()
            .native_enc
            .copy_buffer_to_buffer(staging, dst, bytes.len() as u64);
    }

    fn array_output_capacity(
        &self,
        _port_name: &str,
        _params: &ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        Some(self.values.len() as u32)
    }
}

/// Test-only dead end for a texture output, so the renderer produces it for
/// `dump_textures_all` without a second `system.final_output`.
struct TextureSink {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
}

impl TextureSink {
    fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(SINK_TYPE),
            inputs: vec![NodePort {
                name: std::borrow::Cow::Borrowed("in"),
                ty: PortType::Texture2D,
                kind: PortKind::Input,
                required: false,
            }],
        }
    }
}

impl EffectNode for TextureSink {
    fn depth_rule(&self) -> DepthRule {
        DepthRule::Terminal
    }

    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }

    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }

    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }

    fn parameters(&self) -> &[ParamDef] {
        &[]
    }

    fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

/// Lattice spacing per axis.
fn spacing() -> [f32; 3] {
    std::array::from_fn(|a| LATTICE_SIZE[a] / (LATTICE_NODES[a] - 1) as f32)
}

/// Signed height above the water top at every node, negative inside, x
/// fastest. The top is `SHALLOW_TOP` for x < 0 and `DEEP_TOP` for x > 0, and
/// the water fills the lattice down to its bottom face.
fn step_levelset() -> Vec<f32> {
    let spacing = spacing();
    let min: [f32; 3] = std::array::from_fn(|a| LATTICE_CENTER[a] - LATTICE_SIZE[a] * 0.5);
    let [nx, ny, nz] = LATTICE_NODES;
    let mut values = Vec::with_capacity(nx * ny * nz);
    for _k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let x = min[0] + i as f32 * spacing[0];
                let y = min[1] + j as f32 * spacing[1];
                let top = if x < 0.0 { SHALLOW_TOP } else { DEEP_TOP };
                values.push(y - top);
            }
        }
    }
    values
}

/// `step_levelset` limited to a band |z| < `POOL_HALF_WIDTH`, so the floor
/// shows on both sides of the water across the frame.
fn pool_levelset() -> Vec<f32> {
    let spacing = spacing();
    let min: [f32; 3] = std::array::from_fn(|a| LATTICE_CENTER[a] - LATTICE_SIZE[a] * 0.5);
    let [nx, ny, nz] = LATTICE_NODES;
    let mut values = Vec::with_capacity(nx * ny * nz);
    for k in 0..nz {
        let z = min[2] + k as f32 * spacing[2];
        for j in 0..ny {
            for i in 0..nx {
                let x = min[0] + i as f32 * spacing[0];
                let y = min[1] + j as f32 * spacing[1];
                let top = if x < 0.0 { SHALLOW_TOP } else { DEEP_TOP };
                values.push((y - top).max(z.abs() - POOL_HALF_WIDTH));
            }
        }
    }
    values
}

fn value_node(id: u32, name: &str, value: f32) -> String {
    format!(
        "{{\"id\":{id},\"typeId\":\"node.value\",\"nodeId\":\"{name}\",\"params\":{{\
            \"value\":{{\"type\":\"Float\",\"value\":{value}}}}}}},"
    )
}

/// The scene graph. `water` adds the water object as `object_1` with the
/// given `visible` value; without it the scene is the floor alone.
/// `depth_sink` wires the scene's `depth` output into a dead-end node so the
/// renderer produces it.
fn scene_json(water: Option<f32>, depth_sink: bool) -> String {
    let (orbit, tilt, distance, fov_y) = CAMERA;
    let objects = if water.is_some() { 2 } else { 1 };
    let mut nodes = format!(
        "{{\"id\":0,\"typeId\":\"system.generator_input\",\"nodeId\":\"input\"}},\
        {{\"id\":3,\"typeId\":\"node.orbit_camera\",\"nodeId\":\"cam\",\"params\":{{\
            \"orbit\":{{\"type\":\"Float\",\"value\":{orbit}}},\
            \"tilt\":{{\"type\":\"Float\",\"value\":{tilt}}},\
            \"distance\":{{\"type\":\"Float\",\"value\":{distance}}},\
            \"fov_y\":{{\"type\":\"Float\",\"value\":{fov_y}}},\
            \"near\":{{\"type\":\"Float\",\"value\":{NEAR}}},\
            \"far\":{{\"type\":\"Float\",\"value\":{FAR}}}}}}},\
        {{\"id\":5,\"typeId\":\"node.bake_environment\",\"nodeId\":\"env\",\"params\":{{\
            \"width\":{{\"type\":\"Int\",\"value\":64}},\
            \"height\":{{\"type\":\"Int\",\"value\":32}},\
            \"intensity\":{{\"type\":\"Float\",\"value\":0.0}}}}}},\
        {{\"id\":100,\"typeId\":\"node.grid_mesh\",\"nodeId\":\"floor_grid\",\"params\":{{\
            \"max_capacity\":{{\"type\":\"Int\",\"value\":256}},\
            \"resolution_x\":{{\"type\":\"Int\",\"value\":4}},\
            \"resolution_y\":{{\"type\":\"Int\",\"value\":4}},\
            \"size_x\":{{\"type\":\"Float\",\"value\":40.0}},\
            \"size_y\":{{\"type\":\"Float\",\"value\":40.0}}}}}},\
        {{\"id\":101,\"typeId\":\"node.make_triangles\",\"nodeId\":\"floor_tris\",\"params\":{{\
            \"src_cols\":{{\"type\":\"Int\",\"value\":4}},\
            \"src_rows\":{{\"type\":\"Int\",\"value\":4}}}}}},\
        {{\"id\":102,\"typeId\":\"node.transform_3d\",\"nodeId\":\"floor_xform\",\"params\":{{\
            \"pos_y\":{{\"type\":\"Float\",\"value\":{FLOOR_Y}}}}}}},\
        {{\"id\":103,\"typeId\":\"node.unlit_material\",\"nodeId\":\"floor_mat\",\"params\":{{\
            \"color_r\":{{\"type\":\"Float\",\"value\":{fr}}},\
            \"color_g\":{{\"type\":\"Float\",\"value\":{fg}}},\
            \"color_b\":{{\"type\":\"Float\",\"value\":{fb}}},\
            \"color_a\":{{\"type\":\"Float\",\"value\":1.0}},\
            \"alpha_mode\":{{\"type\":\"Enum\",\"value\":0}}}}}},\
        {{\"id\":104,\"typeId\":\"node.scene_object\",\"nodeId\":\"floor_object\",\"params\":{{}}}},",
        fr = FLOOR_COLOR[0],
        fg = FLOOR_COLOR[1],
        fb = FLOOR_COLOR[2],
    );
    let mut wires = String::from(
        "{\"fromNode\":3,\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"camera\"},\
        {\"fromNode\":5,\"fromPort\":\"envmap\",\"toNode\":20,\"toPort\":\"envmap\"},\
        {\"fromNode\":100,\"fromPort\":\"vertices\",\"toNode\":101,\"toPort\":\"in\"},\
        {\"fromNode\":101,\"fromPort\":\"out\",\"toNode\":104,\"toPort\":\"vertices\"},\
        {\"fromNode\":102,\"fromPort\":\"transform\",\"toNode\":104,\"toPort\":\"transform\"},\
        {\"fromNode\":103,\"fromPort\":\"out\",\"toNode\":104,\"toPort\":\"material\"},\
        {\"fromNode\":104,\"fromPort\":\"object\",\"toNode\":20,\"toPort\":\"object_0\"},",
    );
    if let Some(visible) = water {
        // Pope & Fry absorption as a glTF attenuation colour over one metre.
        let attenuation: [f32; 3] = std::array::from_fn(|c| (-SIGMA[c]).exp());
        nodes.push_str(&format!(
            "{{\"id\":200,\"typeId\":\"{LEVELSET_TYPE}\",\"nodeId\":\"levelset\"}},\
            {{\"id\":210,\"typeId\":\"node.pbr_material\",\"nodeId\":\"water_mat\",\"params\":{{\
                \"color_r\":{{\"type\":\"Float\",\"value\":1.0}},\
                \"color_g\":{{\"type\":\"Float\",\"value\":1.0}},\
                \"color_b\":{{\"type\":\"Float\",\"value\":1.0}},\
                \"ambient\":{{\"type\":\"Float\",\"value\":0.0}},\
                \"metallic\":{{\"type\":\"Float\",\"value\":0.0}},\
                \"roughness\":{{\"type\":\"Float\",\"value\":0.05}},\
                \"ior\":{{\"type\":\"Float\",\"value\":1.333}},\
                \"transmission\":{{\"type\":\"Float\",\"value\":1.0}},\
                \"volume_attenuation_distance\":{{\"type\":\"Float\",\"value\":1.0}},\
                \"volume_attenuation_color_r\":{{\"type\":\"Float\",\"value\":{ar}}},\
                \"volume_attenuation_color_g\":{{\"type\":\"Float\",\"value\":{ag}}},\
                \"volume_attenuation_color_b\":{{\"type\":\"Float\",\"value\":{ab}}}}}}},\
            {{\"id\":211,\"typeId\":\"node.scene_object\",\"nodeId\":\"water_object\",\"params\":{{\
                \"visible\":{{\"type\":\"Float\",\"value\":{visible}}}}}}},",
            ar = attenuation[0],
            ag = attenuation[1],
            ab = attenuation[2],
        ));
        let lattice = [
            ("field_center_x", LATTICE_CENTER[0]),
            ("field_center_y", LATTICE_CENTER[1]),
            ("field_center_z", LATTICE_CENTER[2]),
            ("field_size_x", LATTICE_SIZE[0]),
            ("field_size_y", LATTICE_SIZE[1]),
            ("field_size_z", LATTICE_SIZE[2]),
            ("field_nodes_x", LATTICE_NODES[0] as f32),
            ("field_nodes_y", LATTICE_NODES[1] as f32),
            ("field_nodes_z", LATTICE_NODES[2] as f32),
        ];
        for (index, (port, value)) in lattice.into_iter().enumerate() {
            let id = 220 + index as u32;
            nodes.push_str(&value_node(id, port, value));
            wires.push_str(&format!(
                "{{\"fromNode\":{id},\"fromPort\":\"out\",\"toNode\":211,\"toPort\":\"{port}\"}},"
            ));
        }
        wires.push_str(
            "{\"fromNode\":200,\"fromPort\":\"field\",\"toNode\":211,\"toPort\":\"field\"},\
            {\"fromNode\":210,\"fromPort\":\"out\",\"toNode\":211,\"toPort\":\"material\"},\
            {\"fromNode\":211,\"fromPort\":\"object\",\"toNode\":20,\"toPort\":\"object_1\"},",
        );
    }
    format!(
        "{{\"version\":2,\"name\":\"WaterProof\",\"nodes\":[\
        {nodes}\
        {{\"id\":20,\"typeId\":\"node.render_scene\",\"nodeId\":\"scene\",\"params\":{{\
            \"objects\":{{\"type\":\"Int\",\"value\":{objects}}},\
            \"lights\":{{\"type\":\"Int\",\"value\":0}}}}}},\
        {{\"id\":99,\"typeId\":\"system.final_output\",\"nodeId\":\"out\"}}{depth_node}\
        ],\"wires\":[\
        {wires}\
        {{\"fromNode\":20,\"fromPort\":\"color\",\"toNode\":99,\"toPort\":\"in\"}}{depth_wire}\
        ]}}",
        depth_node = if depth_sink {
            format!(",{{\"id\":98,\"typeId\":\"{SINK_TYPE}\",\"nodeId\":\"depth_sink\"}}")
        } else {
            String::new()
        },
        depth_wire = if depth_sink {
            ",{\"fromNode\":20,\"fromPort\":\"depth\",\"toNode\":98,\"toPort\":\"in\"}"
        } else {
            ""
        },
    )
}

fn read_depth(
    h: &manifold_node_engine::testkit::gpu_harness::ParityHarness,
    tex: &GpuTexture,
) -> Vec<f32> {
    assert_eq!(
        tex.format,
        GpuTextureFormat::R32Float,
        "depth output is R32Float"
    );
    let bytes_per_row = tex.width * 4;
    let total = u64::from(tex.height * bytes_per_row);
    let buf = h.device.create_buffer_shared(total);
    let mut enc = h.device.create_encoder("water-depth-readback");
    enc.copy_texture_to_buffer(tex, &buf, tex.width, tex.height, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let values: &[f32] =
        unsafe { std::slice::from_raw_parts(ptr.cast::<f32>(), (tex.width * tex.height) as usize) };
    values.to_vec()
}

/// One water scene on the shared GPU harness: the runtime, its target and
/// the selection to apply before each frame.
struct WaterScene {
    h: &'static manifold_node_engine::testkit::gpu_harness::ParityHarness,
    runtime: PresetRuntime,
    target: RenderTarget,
    frame_count: i64,
    selection: Vec<u32>,
    width: u32,
    height: u32,
}

impl WaterScene {
    /// `dump_all` keeps every node output so `depth` can read the scene's.
    fn new(json: &str, levelset: Vec<f32>, dump_all: bool) -> Self {
        let h = manifold_node_engine::testkit::gpu_harness::shared();
        let mut registry = PrimitiveRegistry::with_builtin();
        registry.register(LEVELSET_TYPE, move || {
            Box::new(LevelsetSource::new(levelset.clone()))
        });
        registry.register(SINK_TYPE, || Box::new(TextureSink::new()));
        let mut runtime = PresetRuntime::from_json_str_with_device(
            json,
            &registry,
            std::sync::Arc::clone(&h.device),
            h.width,
            h.height,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .unwrap_or_else(|e| panic!("water scene graph must build: {e}\n{json}"));
        if dump_all {
            runtime.set_dump_all(true);
        }
        Self {
            h,
            runtime,
            target: h.make_target("render-scene-water"),
            frame_count: 0,
            selection: Vec::new(),
            width: h.width,
            height: h.height,
        }
    }

    fn set_selection(&mut self, slots: &[u32]) {
        self.selection = slots.to_vec();
    }

    fn request_pick(&mut self, id: u64, uv: [f32; 2]) {
        self.runtime.request_pick(PickRequest { id, uv });
    }

    fn take_pick(&mut self) -> Option<PickHit> {
        self.runtime.take_pick()
    }

    /// Render one frame, applying the selection first.
    fn frame(&mut self) {
        let h = self.h;
        self.runtime.set_selection_mask(&self.selection);
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
            frame_count: self.frame_count,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        self.frame_count += 1;
        let mut enc = h.device.create_encoder("render-scene-water-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            self.runtime.render(
                &mut gpu,
                &self.target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    }

    /// Two frames, so the scene has settled the way the first proofs do.
    fn settle(&mut self) {
        self.frame();
        self.frame();
    }

    fn color(&self) -> Vec<u8> {
        self.h.readback(&self.target.texture)
    }

    /// The scene's `depth` output; needs `dump_all`.
    fn depth(&self) -> Vec<f32> {
        let dumped = self.runtime.dump_textures_all();
        let (_, _, _, tex) = dumped
            .iter()
            .find(|(node, port, _, _)| node == "scene" && port == "depth")
            .expect("a wired depth output must be dumped");
        read_depth(self.h, tex)
    }

    /// The selection mask's bytes, if the last frame drew one.
    fn mask(&self) -> Option<Vec<u8>> {
        let mask = self.runtime.selection_mask()?;
        assert_eq!(mask.format, GpuTextureFormat::R8Unorm);
        let buf = self
            .h
            .device
            .create_buffer_shared(u64::from(mask.width * mask.height));
        let mut enc = self.h.device.create_encoder("water-mask-readback");
        enc.copy_texture_to_buffer(mask, &buf, mask.width, mask.height, mask.width);
        enc.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("shared readback buffer");
        Some(
            unsafe {
                std::slice::from_raw_parts(ptr.cast::<u8>(), (mask.width * mask.height) as usize)
            }
            .to_vec(),
        )
    }
}

fn pixel_rgb(bytes: &[u8], width: u32, x: u32, y: u32) -> [f32; 3] {
    let idx = (y * width + x) as usize * 8;
    let px = &bytes[idx..idx + 8];
    [
        f16::from_le_bytes([px[0], px[1]]).to_f32(),
        f16::from_le_bytes([px[2], px[3]]).to_f32(),
        f16::from_le_bytes([px[4], px[5]]).to_f32(),
    ]
}

fn camera() -> Camera {
    let (orbit, tilt, distance, fov_y) = CAMERA;
    Camera::orbit_perspective(orbit, tilt, distance, fov_y, 0.0, 0.0, NEAR, FAR)
}

/// The world point where pixel (`x`, `y`)'s centre ray meets the plane at
/// height `plane_y`, and that ray's unit direction.
fn ray_at_plane(
    cam: &Camera,
    x: u32,
    y: u32,
    size: (u32, u32),
    plane_y: f32,
) -> ([f32; 3], [f32; 3]) {
    let (origin, dir) = cam.pixel_ray(x as f32 + 0.5, y as f32 + 0.5, size.0, size.1);
    let t = (plane_y - origin[1]) / dir[1];
    (std::array::from_fn(|a| origin[a] + dir[a] * t), dir)
}

fn pixel_of(cam: &Camera, world: [f32; 3], size: (u32, u32)) -> (u32, u32) {
    let projection = cam
        .project_to_pixel(world, size.0, size.1)
        .expect("the point is in front of the camera");
    (projection.px.floor() as u32, projection.py.floor() as u32)
}

/// Beer-Lambert: the 2 m of extra water on the deep side darkens each
/// channel by exp(-sigma * 2 / cos(angle)), compared at pixels mirrored about
/// the frame centre so both see the surface at the same angle.
#[test]
fn water_absorbs_like_beer_lambert() {
    let mut scene = WaterScene::new(&scene_json(Some(1.0), false), step_levelset(), false);
    scene.settle();
    let color = scene.color();
    let size = (scene.width, scene.height);
    let cam = camera();
    // Deep-side surface points at several offsets from the step; the mirrored
    // pixel lands on the shallow side. Both stay clear of the frame edge,
    // where the screen-space refraction fades to the (black) environment.
    for world in [
        [1.2, DEEP_TOP, 0.0],
        [1.8, DEEP_TOP, 0.8],
        [1.5, DEEP_TOP, -0.5],
    ] {
        let (dx, dy) = pixel_of(&cam, world, size);
        let (sx, sy) = (size.0 - 1 - dx, size.1 - 1 - dy);
        let (deep_hit, deep_dir) = ray_at_plane(&cam, dx, dy, size, DEEP_TOP);
        let (shallow_hit, shallow_dir) = ray_at_plane(&cam, sx, sy, size, SHALLOW_TOP);
        assert!(
            deep_hit[0] > 0.5,
            "deep pixel ({dx},{dy}) must be on the deep side: {deep_hit:?}"
        );
        assert!(
            shallow_hit[0] < -0.5,
            "mirrored pixel ({sx},{sy}) must be on the shallow side: {shallow_hit:?}"
        );
        let deep = pixel_rgb(&color, scene.width, dx, dy);
        let shallow = pixel_rgb(&color, scene.width, sx, sy);
        // Path lengths through the water along each ray.
        let deep_path = (DEEP_TOP - FLOOR_Y) / deep_dir[1].abs();
        let shallow_path = (SHALLOW_TOP - FLOOR_Y) / shallow_dir[1].abs();
        for c in 0..3 {
            assert!(
                shallow[c] < FLOOR_COLOR[c],
                "0.5 m of water must absorb: channel {c} shallow {} floor {}",
                shallow[c],
                FLOOR_COLOR[c]
            );
            let expected = (-SIGMA[c] * (deep_path - shallow_path)).exp();
            let ratio = deep[c] / shallow[c];
            assert!(
                (ratio / expected - 1.0).abs() < 0.02,
                "channel {c} at deep ({dx},{dy}) / shallow ({sx},{sy}): deep/shallow {ratio}, expected {expected} (deep {} shallow {})",
                deep[c],
                shallow[c]
            );
        }
    }
}

/// A hidden water object is skipped before it reaches the water path, so
/// the frame matches a scene that never had it.
#[test]
fn hidden_water_leaves_the_frame_bit_identical() {
    let mut with_hidden_water =
        WaterScene::new(&scene_json(Some(0.0), false), step_levelset(), false);
    with_hidden_water.settle();
    let mut without_water = WaterScene::new(&scene_json(None, false), step_levelset(), false);
    without_water.settle();
    assert_eq!(with_hidden_water.color(), without_water.color());
    // The control must not be trivially empty: the floor is on screen.
    let center = pixel_rgb(&without_water.color(), without_water.width, 64, 64);
    for c in 0..3 {
        assert!(
            (center[c] - FLOOR_COLOR[c]).abs() < 0.01,
            "floor colour {center:?}"
        );
    }
}

/// The `depth` output holds the water surface where water covers a pixel,
/// nearer than the floor there.
#[test]
fn water_writes_depth() {
    let mut scene = WaterScene::new(&scene_json(Some(1.0), true), step_levelset(), true);
    scene.settle();
    let depth = scene.depth();
    let size = (scene.width, scene.height);
    let cam = camera();
    // One lattice cell is a tenth of the 2.5 m deep side, in reversed-Z terms.
    let cell_fraction = spacing()[1] / (DEEP_TOP - FLOOR_Y);
    for world in [
        [1.2, DEEP_TOP, 0.0],
        [2.0, DEEP_TOP, 0.6],
        [-1.5, SHALLOW_TOP, -0.4],
    ] {
        let (x, y) = pixel_of(&cam, world, size);
        let top = if world[0] > 0.0 {
            DEEP_TOP
        } else {
            SHALLOW_TOP
        };
        let (water_point, _) = ray_at_plane(&cam, x, y, size, top);
        let (floor_point, _) = ray_at_plane(&cam, x, y, size, FLOOR_Y);
        let water_depth = cam
            .project_to_pixel(water_point, size.0, size.1)
            .unwrap()
            .depth;
        let floor_depth = cam
            .project_to_pixel(floor_point, size.0, size.1)
            .unwrap()
            .depth;
        let got = depth[(y * scene.width + x) as usize];
        // Reversed Z: nearer is larger.
        assert!(
            got > floor_depth,
            "pixel ({x},{y}): water depth {got} must be nearer than the floor's {floor_depth}"
        );
        assert!(
            (got - water_depth).abs() <= (water_depth - floor_depth) * cell_fraction * 1.5,
            "pixel ({x},{y}): depth {got}, the water surface projects to {water_depth} (floor {floor_depth})"
        );
    }
}

/// Window uv of a world point, as the editor builds a click.
fn uv_of(cam: &Camera, world: [f32; 3], size: (u32, u32)) -> [f32; 2] {
    let p = cam
        .project_to_pixel(world, size.0, size.1)
        .expect("the point is in front of the camera");
    [p.px / size.0 as f32, p.py / size.1 as f32]
}

/// Click `uv`: request, two frames, take. Returns the picked render slot.
fn click(scene: &mut WaterScene, id: u64, uv: [f32; 2]) -> Option<u32> {
    scene.request_pick(id, uv);
    scene.settle();
    let hit = scene
        .take_pick()
        .expect("a pick is answered the frame after it draws");
    assert_eq!(hit.id, id);
    hit.slot
}

/// A click on field water picks the water (`object_1`, slot 1), not the
/// floor under it; a click past the pool picks the floor; hidden water is
/// never picked.
#[test]
fn a_click_on_the_water_picks_it_not_the_floor_below() {
    let cam = camera();
    let mut scene = WaterScene::new(&scene_json(Some(1.0), false), pool_levelset(), false);
    let size = (scene.width, scene.height);
    scene.settle();
    let deep = uv_of(&cam, [1.2, DEEP_TOP, 0.0], size);
    let shallow = uv_of(&cam, [-1.5, SHALLOW_TOP, 0.3], size);
    // The ray to this floor point crosses the top plane at |z| > 1.5, outside the band.
    let floor = uv_of(&cam, [1.2, FLOOR_Y, 2.0], size);
    assert_eq!(click(&mut scene, 1, deep), Some(1), "the deep water");
    assert_eq!(click(&mut scene, 2, shallow), Some(1), "the shallow water");
    assert_eq!(
        click(&mut scene, 3, floor),
        Some(0),
        "the floor beside the pool"
    );

    let mut hidden = WaterScene::new(&scene_json(Some(0.0), false), pool_levelset(), false);
    hidden.settle();
    assert_eq!(
        click(&mut hidden, 4, deep),
        Some(0),
        "hidden water leaves the floor"
    );
}

/// Selecting the water leaves the picture alone and masks its band: pixels
/// whose ray meets the water well inside the band are covered, pixels whose
/// ray stays well outside it are not.
#[test]
fn selecting_the_water_masks_its_band() {
    let json = scene_json(Some(1.0), false);
    let mut plain = WaterScene::new(&json, pool_levelset(), false);
    plain.settle();
    let mut selected = WaterScene::new(&json, pool_levelset(), false);
    selected.set_selection(&[1]);
    selected.settle();
    assert!(
        plain.color() == selected.color(),
        "D9: the output is byte-identical with a selection"
    );
    assert!(plain.mask().is_none(), "no selection, no mask");
    let mask = selected.mask().expect("a selection draws a mask");
    let (width, height) = (selected.width, selected.height);
    assert_eq!(mask.len(), (width * height) as usize);

    let cam = camera();
    let (mut inside, mut inside_covered) = (0u32, 0u32);
    let (mut outside, mut outside_covered) = (0u32, 0u32);
    for y in 0..height {
        for x in 0..width {
            let (origin, dir) = cam.pixel_ray(x as f32 + 0.5, y as f32 + 0.5, width, height);
            // The point where the ray meets `plane_y`, ahead of the camera.
            let at = |plane_y: f32| {
                let t = (plane_y - origin[1]) / dir[1];
                (t > 0.0).then(|| -> [f32; 3] { std::array::from_fn(|a| origin[a] + dir[a] * t) })
            };
            // The top plane of the side the ray lands on.
            let top = match (at(DEEP_TOP), at(SHALLOW_TOP)) {
                (Some(p), _) if p[0] > 0.0 && p[0] < 4.9 => p,
                (_, Some(p)) if p[0] < 0.0 && p[0] > -4.9 => p,
                _ => continue,
            };
            let covered = mask[(y * width + x) as usize] > 127;
            if top[2].abs() < POOL_HALF_WIDTH - 0.1 {
                inside += 1;
                inside_covered += u32::from(covered);
            } else if top[2].abs() > POOL_HALF_WIDTH + 0.1 {
                // The floor point must also be outside the band, on the same side.
                let Some(floor) = at(FLOOR_Y) else { continue };
                if floor[2].abs() > POOL_HALF_WIDTH + 0.1 && floor[2] * top[2] > 0.0 {
                    outside += 1;
                    outside_covered += u32::from(covered);
                }
            }
        }
    }
    eprintln!("water mask: inside {inside_covered}/{inside}, outside {outside_covered}/{outside}");
    assert!(inside > 500, "the band shows ({inside} px)");
    assert!(outside > 500, "the floor beside it shows ({outside} px)");
    assert!(
        inside_covered as f64 >= inside as f64 * 0.99,
        "the mask covers the band: {inside_covered} of {inside}"
    );
    assert!(
        outside_covered as f64 <= outside as f64 * 0.01,
        "and not the floor beside it: {outside_covered} of {outside}"
    );
}
