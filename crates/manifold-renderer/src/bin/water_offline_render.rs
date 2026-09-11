//! Offline APIC cache → density isosurface → production scene/Metal RT renderer.
//! Usage: water-offline-render CACHE_DIR OUTPUT_DIR FRAME_COUNT [WIDTH HEIGHT]
//! Geometry is held fixed while each frame's asynchronous RT build settles.
#[path = "water_offline/surface.rs"]
mod surface;

use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::{
    gpu_encoder::GpuEncoder,
    headless_readback::readback_to_srgb_png,
    node_graph::{
        EffectNode, EffectNodeContext, EffectNodeType, Executor, FinalOutput, FrameTime, Graph,
        MetalBackend, NodeInstanceId, ParamDef, ParamValue, ResourceId, compile,
        ports::{NodeInput, NodeOutput},
        primitive::PrimitiveSpec,
        primitives::{
            BakeEquirectEnvmap, CameraOrbit, GenerateCubeMesh, LightNode, PbrMaterial, RenderScene,
            SceneObjectNode, Transform3D,
        },
        primitives::{RT_CAPTURE_ARM, RT_CAPTURE_QUEUE},
    },
    render_target::RenderTarget,
};
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock, atomic::Ordering},
};

// IO bridge: the host owns a fully uploaded, immutable mesh for this cache frame.
// Cubes reuse the production generator, dirty-checked by destination identity.
struct OfflineMesh {
    cube: Option<GenerateCubeMesh>,
    last_destination: Option<usize>,
}
impl EffectNode for OfflineMesh {
    fn type_id(&self) -> &EffectNodeType {
        static ID: OnceLock<EffectNodeType> = OnceLock::new();
        ID.get_or_init(|| EffectNodeType::new("offline.cached_mesh"))
    }
    fn inputs(&self) -> &[NodeInput] {
        &[]
    }
    fn outputs(&self) -> &[NodeOutput] {
        GenerateCubeMesh::OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        GenerateCubeMesh::PARAMS
    }
    fn depth_rule(&self) -> manifold_renderer::node_graph::depth_rule::DepthRule {
        manifold_renderer::node_graph::depth_rule::DepthRule::Terminal
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let identity = ctx.outputs.array("vertices").map(|b| b.identity_key());
        if identity == self.last_destination {
            ctx.mark_outputs_unchanged();
        } else {
            if let Some(cube) = &mut self.cube {
                cube.evaluate(ctx);
            }
            self.last_destination = identity;
        }
    }
}

fn floats(g: &mut Graph, node: NodeInstanceId, values: &[(&str, f32)]) {
    for &(key, value) in values {
        g.set_param(node, key, ParamValue::Float(value)).unwrap();
    }
}
fn resource(
    plan: &manifold_renderer::node_graph::ExecutionPlan,
    node: NodeInstanceId,
    port: &str,
) -> ResourceId {
    plan.steps()
        .iter()
        .find(|s| s.node == node)
        .unwrap()
        .outputs
        .iter()
        .find(|(name, _)| *name == port)
        .unwrap()
        .1
}
fn scene() -> (Graph, NodeInstanceId, Vec<NodeInstanceId>) {
    let mut g = Graph::new();
    let render = g.add_node(Box::new(RenderScene::new()));
    floats(&mut g, render, &[("objects", 7.0), ("lights", 1.0)]);
    for key in [
        "rt_enabled",
        "rt_shadows",
        "rt_ao",
        "rt_gi",
        "rt_reflections",
    ] {
        g.set_param(render, key, ParamValue::Bool(true)).unwrap();
    }
    let cam = g.add_node(Box::new(CameraOrbit::new()));
    floats(
        &mut g,
        cam,
        &[
            ("orbit", 0.65),
            ("tilt", 0.65),
            ("distance", 2.9),
            ("fov_y", 0.65),
            ("look_y", 0.16),
            ("near", 0.02),
            ("far", 30.0),
        ],
    );
    g.connect((cam, "out"), (render, "camera")).unwrap();
    let light = g.add_node(Box::new(LightNode::new()));
    floats(
        &mut g,
        light,
        &[
            ("pos_x", -2.0),
            ("pos_y", 4.0),
            ("pos_z", 2.0),
            ("intensity", 3.0),
            ("light_size", 0.12),
        ],
    );
    g.connect((light, "out"), (render, "light_0")).unwrap();
    let env = g.add_node(Box::new(BakeEquirectEnvmap::new()));
    g.connect((env, "envmap"), (render, "envmap")).unwrap();
    let sink = g.add_node(Box::new(FinalOutput::new()));
    g.connect((render, "color"), (sink, "in")).unwrap();
    // Basin inside is exactly the simulation's 1.25 × .75 metre box.
    // Floor is at world y=0 after the water's translation.
    let boxes = [
        ([0.0, -0.08, 0.0], [2.8, 0.12, 2.1]),
        ([0.0, -0.025, 0.0], [1.43, 0.05, 0.93]),
        ([-0.665, 0.25, 0.0], [0.08, 0.5, 0.91]),
        ([0.665, 0.25, 0.0], [0.08, 0.5, 0.91]),
        ([0.0, 0.25, -0.415], [1.25, 0.5, 0.08]),
        ([0.0, 0.25, 0.415], [1.25, 0.5, 0.08]),
    ];
    let mut meshes = Vec::new();
    for i in 0..7 {
        let mesh = g.add_node(Box::new(OfflineMesh {
            cube: (i > 0).then(GenerateCubeMesh::new),
            last_destination: None,
        }));
        meshes.push(mesh);
        let object = g.add_node(Box::new(SceneObjectNode::new()));
        let mat = g.add_node(Box::new(PbrMaterial::new()));
        let transform = g.add_node(Box::new(Transform3D::new()));
        if i == 0 {
            g.set_param(mat, "alpha_mode", ParamValue::Enum(2)).unwrap();
            floats(
                &mut g,
                mat,
                &[
                    ("color_r", 1.0),
                    ("color_g", 1.0),
                    ("color_b", 1.0),
                    ("metallic", 0.0),
                    ("roughness", 0.04),
                    ("ior", 1.333),
                    ("transmission", 1.0),
                    ("volume_thickness", 0.375),
                    ("volume_attenuation_distance", 2.0),
                    ("volume_attenuation_color_r", 0.70),
                    ("volume_attenuation_color_g", 0.90),
                    ("volume_attenuation_color_b", 0.95),
                ],
            );
            floats(
                &mut g,
                transform,
                &[("pos_x", -0.75), ("pos_y", -0.125), ("pos_z", -0.5)],
            );
        } else {
            let (p, s) = boxes[i - 1];
            floats(
                &mut g,
                transform,
                &[
                    ("pos_x", p[0]),
                    ("pos_y", p[1]),
                    ("pos_z", p[2]),
                    ("scale_x", s[0]),
                    ("scale_y", s[1]),
                    ("scale_z", s[2]),
                ],
            );
            let c = if i == 1 {
                [0.12, 0.15, 0.18]
            } else {
                [0.62, 0.57, 0.46]
            };
            floats(
                &mut g,
                mat,
                &[
                    ("color_r", c[0]),
                    ("color_g", c[1]),
                    ("color_b", c[2]),
                    ("roughness", 0.38),
                    ("metallic", 0.0),
                ],
            );
        }
        g.connect((mesh, "vertices"), (object, "vertices")).unwrap();
        g.connect((mat, "out"), (object, "material")).unwrap();
        g.connect((transform, "transform"), (object, "transform"))
            .unwrap();
        g.connect(
            (object, "object"),
            (
                render,
                [
                    "object_0", "object_1", "object_2", "object_3", "object_4", "object_5",
                    "object_6",
                ][i],
            ),
        )
        .unwrap();
    }
    (g, render, meshes)
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if !(4..=6).contains(&args.len()) {
        return Err(
            "usage: water-offline-render CACHE_DIR OUTPUT_DIR FRAME_COUNT [WIDTH HEIGHT]".into(),
        );
    }
    let cache = PathBuf::from(&args[1]);
    let out = PathBuf::from(&args[2]);
    let frames: u32 = args[3].parse()?;
    let width: u32 = args.get(4).map_or(Ok(1920), |s| s.parse())?;
    let height: u32 = args.get(5).map_or(Ok(1080), |s| s.parse())?;
    if frames == 0 || width == 0 || height == 0 {
        return Err("positive frame count and dimensions required".into());
    }
    std::fs::create_dir_all(&out)?;
    let device = Arc::new(GpuDevice::new());
    let (mut graph, render, meshes) = scene();
    let plan = compile(&graph)?;
    let format = GpuTextureFormat::Rgba16Float;
    let mut tick = 0;
    for frame in 0..frames {
        let bytes = std::fs::read(cache.join(format!("frame_{frame:06}.bin")))?;
        if bytes.is_empty() || bytes.len() % 96 != 0 {
            return Err("invalid WaterParticle cache length".into());
        }
        let particles: Vec<[f32; 4]> = bytes
            .chunks_exact(96)
            .map(|p| {
                std::array::from_fn(|i| f32::from_le_bytes(p[i * 4..i * 4 + 4].try_into().unwrap()))
            })
            .collect();
        let vertices = surface::reconstruct_surface(
            &particles,
            [0.0; 3],
            [97, 97, 65],
            0.015625,
            0.0625,
            0.5,
        )?;
        if vertices.is_empty() {
            return Err("density surface is empty".into());
        }
        eprintln!(
            "frame {frame}: {} particles, {} triangles",
            particles.len(),
            vertices.len() / 3
        );
        // Fresh bindings bound memory and make changed topology visible to RT.
        let mut backend = MetalBackend::new(Arc::clone(&device), width, height, format);
        let target = RenderTarget::new(&device, width, height, format, "offline-water-color");
        let color_slot = backend.pre_bind_texture_2d(resource(&plan, render, "color"), target);
        for (i, &mesh) in meshes.iter().enumerate() {
            let size = if i == 0 {
                std::mem::size_of_val(vertices.as_slice())
            } else {
                36 * 64
            };
            let buffer = device.create_buffer_shared(size as u64);
            if i == 0 {
                let ptr = buffer.mapped_ptr().ok_or("mesh buffer is not mapped")?;
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        vertices.as_ptr().cast::<u8>(),
                        ptr.cast::<u8>(),
                        size,
                    );
                }
            }
            backend.pre_bind_array(resource(&plan, mesh, "vertices"), buffer);
        }
        let mut exec = Executor::new(Box::new(backend));
        let mut traced = 0;
        // Bounded settling, with actual trace-channel evidence; never save raster fallback.
        for _ in 0..48 {
            RT_CAPTURE_ARM.store(true, Ordering::Relaxed);
            let mut encoder = device.create_encoder("offline-water-frame");
            {
                let mut gpu = GpuEncoder::new(&mut encoder, &device);
                exec.execute_frame_with_gpu(
                    &mut graph,
                    &plan,
                    FrameTime {
                        beats: Beats(frame as f64 / 15.0),
                        seconds: Seconds(frame as f64 / 30.0),
                        delta: Seconds(0.0),
                        frame_count: tick,
                    },
                    &mut gpu,
                );
            }
            encoder.commit_and_wait_completed();
            tick += 1;
            let captures = std::mem::take(&mut *RT_CAPTURE_QUEUE.lock().unwrap());
            if captures.iter().any(|c| c.label == "refl_raw")
                && captures.iter().any(|c| c.label == "mask_half")
            {
                traced += 1;
            }
            if traced >= 8 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        RT_CAPTURE_ARM.store(false, Ordering::Relaxed);
        if traced < 8 {
            return Err(format!("frame {frame}: RT did not produce eight lighting samples within 48 ticks ({traced} observed)").into());
        }
        let texture = exec
            .backend()
            .texture_2d(color_slot)
            .ok_or("missing color target")?;
        let png = readback_to_srgb_png(&device, texture, width, height);
        std::fs::write(out.join(format!("frame_{frame:06}.png")), png)?;
        eprintln!("frame {frame}: saved with {traced} confirmed RT samples");
    }
    Ok(())
}
struct StderrLogger;
impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{}: {}", record.level(), record.args());
        }
    }
    fn flush(&self) {}
}
fn main() {
    static LOGGER: StderrLogger = StderrLogger;
    log::set_logger(&LOGGER).expect("install offline logger");
    log::set_max_level(log::LevelFilter::Info);
    if let Err(e) = run() {
        eprintln!("water-offline-render: {e}");
        std::process::exit(1);
    }
}

#[test]
fn offline_scene_graph_wires_the_production_rt_renderer() {
    let (graph, render, meshes) = scene();
    let plan = compile(&graph).expect("offline scene compiles");
    assert_eq!(meshes.len(), 7);
    for mesh in meshes {
        let _ = resource(&plan, mesh, "vertices");
    }
    let _ = resource(&plan, render, "color");
}
