//! Native Metal presentation primitive for the Math View sparse mesh diagram.
//!
//! This node only presents buffers produced by the authored graph. It does
//! not contain a copy of any modifier or deformation equation. Geometry,
//! arrows, axes, the infinite world grid, and temporal trails share one bounded
//! instanced pass. The grid uses the scene camera without the object transform.

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::{Primitive, PrimitiveSpec};
use crate::node_graph::transform::Transform;
use manifold_gpu::{
    GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuLoadAction, GpuSamplerDesc,
    GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use std::borrow::Cow;

const MSAA_SAMPLE_COUNT: u32 = 4;
const HISTORY_SAMPLES: u32 = 64;
const TRAIL_RENDER_SAMPLES: u32 = 32;
const MAX_VERTICES: u32 = 1536;
const DIAGRAM_BLEND: GpuBlendState = GpuBlendState {
    src_factor: GpuBlendFactor::SrcAlpha,
    dst_factor: GpuBlendFactor::OneMinusSrcAlpha,
    operation: GpuBlendOp::Add,
    src_alpha_factor: GpuBlendFactor::One,
    dst_alpha_factor: GpuBlendFactor::OneMinusSrcAlpha,
    alpha_operation: GpuBlendOp::Add,
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DiagramUniforms {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    viewport: [f32; 4],
    radius: f32,
    line_width: f32,
    geometry_hue: f32,
    path_hue: f32,
    grid: u32,
    fragments: u32,
    ghosts: u32,
    vectors: u32,
    trails: u32,
    tri_count: u32,
    vertex_count: u32,
    history_head: u32,
    history_len: u32,
    history_capacity: u32,
    history_stride: u32,
    axes: u32,
    depth_pass: u32,
    occlusion: u32,
    mode: u32,
    _depth_pad: u32,
    inv_view_proj: [[f32; 4]; 4],
    camera_pos_far: [f32; 4],
    brightness: [f32;4],
    event_values: [f32;4],
    scan_values: [f32;4],
    event_targets: [u32;4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HistoryCaptureUniforms {
    vertex_count: u32,
    history_slot: u32,
    history_stride: u32,
    _pad: u32,
}

crate::primitive! {
    name: RenderMeshDiagram,
    type_id: "node.render_mesh_diagram",
    purpose: "Render sparse evaluated MeshVertex samples as a diagram or current-surface depth through the authored Camera. Optional shared surface and scene depth occlude diagram marks; this presentation node contains no modifier math.",
    inputs: {
        current: Array(MeshVertex) required,
        reference: Array(MeshVertex) required,
        incoming: Array(MeshVertex) required,
        camera: Camera required,
        transform: Transform optional,
        grid: ScalarF32 optional,
        axes: ScalarF32 optional,
        fragments: ScalarF32 optional,
        ghosts: ScalarF32 optional,
        vectors: ScalarF32 optional,
        trails: ScalarF32 optional,
        density: ScalarF32 optional,
        line_width: ScalarF32 optional,
        geometry_hue: ScalarF32 optional,
        path_hue: ScalarF32 optional,
        radius: ScalarF32 optional,
        mesh_weights: Array(f32) optional,
        scan_weights: Array(f32) optional,
        grid_brightness: ScalarF32 optional,
        fragments_brightness: ScalarF32 optional,
        ghosts_brightness: ScalarF32 optional,
        vectors_brightness: ScalarF32 optional,
        trails_brightness: ScalarF32 optional,
        pulse_gain: ScalarF32 optional,
        pulse_target: ScalarF32 optional,
        scan_target: ScalarF32 optional,
        connect_mesh: ScalarF32 optional,
        scan_amount: ScalarF32 optional,
        scan_progress: ScalarF32 optional,
        scan_width: ScalarF32 optional,
        scan_direction: ScalarF32 optional,
        scan_mode: ScalarF32 optional,
        surface_depth: Texture2D optional,
        scene_depth: Texture2D optional,
        occlusion: ScalarF32 optional,
        mode: ScalarF32 optional,
    },
    outputs: { color: Texture2D, depth: Texture2D },
    params: [
        ParamDef { name: Cow::Borrowed("grid_brightness"), label: "grid brightness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fragments_brightness"), label: "fragments brightness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("ghosts_brightness"), label: "ghosts brightness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("vectors_brightness"), label: "vectors brightness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("trails_brightness"), label: "trails brightness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pulse_gain"), label: "pulse gain", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pulse_target"), label: "pulse target", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scan_target"), label: "scan target", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("connect_mesh"), label: "connect mesh", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scan_amount"), label: "scan amount", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scan_progress"), label: "scan progress", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scan_width"), label: "scan width", ty: ParamType::Float, default: ParamValue::Float(0.2), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scan_direction"), label: "scan direction", ty: ParamType::Float, default: ParamValue::Float(2.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scan_mode"), label: "scan mode", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("occlusion"), label: "Occlusion", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("mode"), label: "Mode", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 2.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("grid"), label: "Grid", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("axes"), label: "Axes", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fragments"), label: "Fragments", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("ghosts"), label: "Ghosts", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("vectors"), label: "Vectors", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("trails"), label: "Motion Trails", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("density"), label: "Density", ty: ParamType::Int, default: ParamValue::Float(4.0), range: Some((2.0, 8.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("line_width"), label: "Line Width", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.5, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("geometry_hue"), label: "Geometry Hue", ty: ParamType::Float, default: ParamValue::Float(0.52), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("path_hue"), label: "Path Hue", ty: ParamType::Float, default: ParamValue::Float(0.13), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("radius"), label: "Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.001, 100.0)), enum_values: &[] },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Current, reference, and incoming arrays are authored graph outputs. The transparent diagram uses the Camera and optional object Transform; Motion trails are temporal history, never parameter-sweep trajectories. Wire depth-only instances in a surface_depth chain (matching canvas-sized R32Float), then feed its final depth to colour instances. Occlusion=1 tests shared surfaces; Mode=2 also tests scene_depth at its own resolution. Ghosts/trails remain translucent, and the grid never writes depth. Unwired depth inputs mean no external occluder.",
    examples: [], picker: { label: "Mesh Diagram", category: Atom },
    summary: "Draws sparse evaluated mesh samples as a transparent native diagram.",
    category: Geometry3D, role: Filter, aliases: ["mesh diagram", "math view"], boundary_reason: DrawCall,
    extra_fields: {
        render_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        depth_color_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        depth_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        depth_sampler: Option<manifold_gpu::GpuSampler> = None,
        dummy_depth: Option<manifold_gpu::GpuTexture> = None,
        capture_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        msaa: Option<manifold_gpu::GpuTexture> = None,
        width: u32 = 0,
        height: u32 = 0,
        history: Option<manifold_gpu::GpuBuffer> = None,
        history_head: u32 = 0,
        history_len: u32 = 0,
        last_seconds: Option<f64> = None,
        last_frame_count: Option<i64> = None,
        history_reset: bool = false,
        last_vertex_count: u32 = 0,
    },
}

const SHADER: &str = concat!(include_str!("shaders/sample_face_common.wgsl"), "\n", include_str!("shaders/render_mesh_diagram.wgsl"));
const CAPTURE_SHADER: &str = include_str!("shaders/history_capture.wgsl");

impl RenderMeshDiagram {
    /// Startup prewarm hook used by the native generator registry.
    pub fn prewarm_pipelines(device: &manifold_gpu::GpuDevice) {
        device.create_render_pipeline_msaa(
            SHADER,
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            Some(DIAGRAM_BLEND),
            MSAA_SAMPLE_COUNT,
            Self::TYPE_ID,
        );
        device.create_render_pipeline_msaa(
            SHADER,
            "vs_main",
            "fs_depth_color",
            GpuTextureFormat::Rgba16Float,
            Some(DIAGRAM_BLEND),
            MSAA_SAMPLE_COUNT,
            Self::TYPE_ID,
        );
        device.create_render_pipeline(
            SHADER,
            "vs_main",
            "fs_depth",
            GpuTextureFormat::R32Float,
            Some(GpuBlendState {
                src_factor: GpuBlendFactor::One,
                dst_factor: GpuBlendFactor::One,
                operation: GpuBlendOp::Min,
                src_alpha_factor: GpuBlendFactor::One,
                dst_alpha_factor: GpuBlendFactor::One,
                alpha_operation: GpuBlendOp::Min,
            }),
            Self::TYPE_ID,
        );
        device.create_compute_pipeline(CAPTURE_SHADER, "cs_main", "math-view-history-capture");
    }

    fn ensure_history(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.history.is_some() {
            return;
        }
        let bytes =
            HISTORY_SAMPLES as u64 * MAX_VERTICES as u64 * std::mem::size_of::<[f32; 4]>() as u64;
        let history = device.create_buffer_shared(bytes);
        history.zero_fill();
        self.history = Some(history);
    }

    fn ensure_depth_resources(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.depth_sampler.is_none() {
            self.depth_sampler = Some(device.create_sampler(&GpuSamplerDesc {
                min_filter: manifold_gpu::GpuFilterMode::Nearest,
                mag_filter: manifold_gpu::GpuFilterMode::Nearest,
                ..GpuSamplerDesc::default()
            }));
        }
        if self.dummy_depth.is_none() {
            let texture = device.create_texture(&GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                format: GpuTextureFormat::R32Float,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
                label: "node.render_mesh_diagram background depth",
                mip_levels: 1,
            });
            device.upload_texture(&texture, bytemuck::bytes_of(&0.0f32));
            self.dummy_depth = Some(texture);
        }
    }

    fn model_matrix(t: Transform, camera: &Camera) -> [[f32; 4]; 4] {
        let rot = if t.billboard {
            t.billboard_rot_euler(camera.pos)
        } else {
            t.rot_euler
        };
        super::render_scene::model_matrix(t.pos, rot, t.scale)
    }
}

impl Primitive for RenderMeshDiagram {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(current) = ctx.inputs.array("current") else {
            return;
        };
        let Some(reference) = ctx.inputs.array("reference") else {
            return;
        };
        let Some(incoming) = ctx.inputs.array("incoming") else {
            return;
        };
        let Some(camera) = ctx.inputs.camera("camera") else {
            return;
        };
        let color_out = ctx.outputs.texture_2d("color");
        let depth_out = ctx.outputs.texture_2d("depth");
        if color_out.is_none() && depth_out.is_none() {
            return;
        }
        let (width, height) = color_out
            .or(depth_out)
            .map(|target| (target.width, target.height))
            .unwrap_or((0, 0));
        if width == 0 || height == 0 {
            return;
        }
        let seconds = ctx.time.seconds.0;
        let discontinuous = self.last_seconds.is_some_and(|prev| {
            seconds < prev - 1.0e-6
                || ctx.time.delta.0 < -1.0e-6
                || (seconds - prev - ctx.time.delta.0).abs() > 0.25
        }) || self.last_frame_count.is_some_and(|prev| {
            ctx.time.frame_count > 0 && prev > 0 && ctx.time.frame_count != prev + 1
        });
        if discontinuous {
            self.history_head = 0;
            self.history_len = 0;
            self.history_reset = true;
        }
        self.last_seconds = Some(seconds);
        self.last_frame_count = Some(ctx.time.frame_count);
        let density = ctx.scalar_or_param("density", 4.0).round().clamp(2.0, 8.0) as u32;
        let capacity = ((current.size.min(reference.size).min(incoming.size)
            / std::mem::size_of::<MeshVertex>() as u64) as u32)
            .min(MAX_VERTICES);
        let mesh_weights=ctx.inputs.array("mesh_weights");
        let scan_weights=ctx.inputs.array("scan_weights");
        let mesh_triangles=mesh_weights.map_or(0,|b|(b.size/12) as u32);
        let tri_count = (density * density * density).min(capacity / 3).min(if mesh_triangles>0 {mesh_triangles}else{u32::MAX});
        let vertex_count = tri_count * 3;
        if vertex_count != self.last_vertex_count {
            self.history_head = 0;
            self.history_len = 0;
            self.last_vertex_count = vertex_count;
        }
        // Reset before publishing the draw uniforms: re-enabling trails must
        // not display the stale ring for one frame before capture clears it.
        if self.history_reset {
            self.history_head = 0;
            self.history_len = 0;
        }
        // Toggle ports shadow the Bool params. Resolve the connected value
        // first, then accept either the typed Bool or a modulatable Float
        // from the param table before falling back to the declared default.
        let toggled = |name: &str| {
            let as_toggle = |value: &ParamValue| {
                value
                    .as_scalar()
                    .map(|scalar| scalar > 0.5)
                    .or(match value {
                        ParamValue::Bool(enabled) => Some(*enabled),
                        _ => None,
                    })
            };
            let enabled = ctx
                .inputs
                .scalar(name)
                .as_ref()
                .and_then(as_toggle)
                .or_else(|| ctx.params.get(name).and_then(as_toggle))
                .unwrap_or(true);
            u32::from(enabled)
        };
        let view_proj = camera.view_proj(width as f32 / height as f32);
        let Some(inv_view_proj) = super::render_scene::mat4_inverse(view_proj) else {
            log::error!("Math View cannot project the world grid: singular scene camera");
            return;
        };
        let uniforms = DiagramUniforms {
            view_proj,
            model: Self::model_matrix(
                ctx.inputs.transform("transform").unwrap_or_default(),
                &camera,
            ),
            viewport: [width as f32, height as f32, 0.0, 0.0],
            radius: ctx.scalar_or_param("radius", 1.0).max(0.001),
            line_width: ctx.scalar_or_param("line_width", 1.0).clamp(0.5, 4.0),
            geometry_hue: ctx.scalar_or_param("geometry_hue", 0.52).fract().abs(),
            path_hue: ctx.scalar_or_param("path_hue", 0.13).fract().abs(),
            grid: toggled("grid"),
            fragments: toggled("fragments"),
            ghosts: toggled("ghosts"),
            vectors: toggled("vectors"),
            trails: toggled("trails"),
            tri_count,
            vertex_count,
            history_head: self.history_head,
            history_len: self.history_len,
            history_capacity: HISTORY_SAMPLES,
            history_stride: MAX_VERTICES,
            axes: toggled("axes"),
            depth_pass: 0,
            occlusion: ctx.scalar_or_param("occlusion", 0.0).round().clamp(0.0, 1.0) as u32,
            mode: ctx.scalar_or_param("mode", 0.0).round().clamp(0.0, 2.0) as u32,
            _depth_pad: 0,
            inv_view_proj,
            camera_pos_far: [camera.pos[0], camera.pos[1], camera.pos[2], camera.far],
            brightness: ["grid_brightness","fragments_brightness","ghosts_brightness","vectors_brightness"].map(|n|ctx.scalar_or_param(n,1.0).max(0.0)),
            event_values: [ctx.scalar_or_param("trails_brightness",1.0).max(0.0),ctx.scalar_or_param("pulse_gain",1.0).max(0.0),ctx.scalar_or_param("scan_amount",0.0),ctx.scalar_or_param("scan_progress",0.0)],
            scan_values: [ctx.scalar_or_param("scan_width",0.2),ctx.scalar_or_param("scan_direction",2.0),ctx.scalar_or_param("scan_mode",0.0),ctx.scalar_or_param("connect_mesh",0.0)],
            event_targets: [ctx.scalar_or_param("pulse_target",0.0).round().clamp(0.0,5.0) as u32,ctx.scalar_or_param("scan_target",0.0).round().clamp(0.0,5.0) as u32,mesh_triangles,scan_weights.map_or(0,|b|(b.size/4) as u32)],
        };
        let input_surface_depth = ctx.inputs.texture_2d("surface_depth");
        let input_scene_depth = ctx.inputs.texture_2d("scene_depth");
        let gpu = ctx.gpu_encoder();
        if color_out.is_some() {
            self.ensure_history(gpu.device);
        }
        self.ensure_depth_resources(gpu.device);
        let dummy_depth = self.dummy_depth.as_ref().expect("depth resources initialized");
        let surface_depth = input_surface_depth.unwrap_or(dummy_depth);
        let scene_depth = input_scene_depth.unwrap_or(dummy_depth);
        let depth_sampler = self.depth_sampler.as_ref().expect("depth sampler initialized");

        if color_out.is_some() && self.render_pipeline.is_none() {
            self.render_pipeline = Some(gpu.device.create_render_pipeline_msaa(
                SHADER,
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                Some(DIAGRAM_BLEND),
                MSAA_SAMPLE_COUNT,
                Self::TYPE_ID,
            ));
        }
        if color_out.is_some()
            && uniforms.occlusion != 0
            && self.depth_color_pipeline.is_none()
        {
            self.depth_color_pipeline = Some(gpu.device.create_render_pipeline_msaa(
                SHADER,
                "vs_main",
                "fs_depth_color",
                GpuTextureFormat::Rgba16Float,
                Some(DIAGRAM_BLEND),
                MSAA_SAMPLE_COUNT,
                Self::TYPE_ID,
            ));
        }
        if color_out.is_some() && self.capture_pipeline.is_none() {
            self.capture_pipeline = Some(gpu.device.create_compute_pipeline(
                CAPTURE_SHADER,
                "cs_main",
                "math-view-history-capture",
            ));
        }
        if color_out.is_some() && (self.width != width || self.height != height || self.msaa.is_none()) {
            self.msaa = Some(gpu.device.create_texture_msaa_memoryless(
                width,
                height,
                GpuTextureFormat::Rgba16Float,
                MSAA_SAMPLE_COUNT,
                "node.render_mesh_diagram MSAA",
            ));
            self.width = width;
            self.height = height;
        }
        let arrow_count = tri_count;
        let grid_count = 1;
        // One world frame and at most three representative fragment frames.
        let axes_count = 3 + tri_count.min(3) * 3;
        let trail_count = if uniforms.trails != 0 {
            TRAIL_RENDER_SAMPLES * vertex_count
        } else {
            0
        };
        let instance_count = tri_count * 3 + arrow_count + grid_count + axes_count + trail_count;
        if let Some(out) = color_out {
            let history = self.history.as_ref().expect("history allocated");
            let depth_tested = uniforms.occlusion != 0;
            if depth_tested {
                gpu.native_enc.draw_instanced_msaa(
                    self.depth_color_pipeline.as_ref().expect("depth colour pipeline initialized"),
                    self.msaa.as_ref().expect("MSAA initialized"), out,
                    &[
                        GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                        GpuBinding::Buffer { binding: 1, buffer: current, offset: 0 },
                        GpuBinding::Buffer { binding: 2, buffer: reference, offset: 0 },
                        GpuBinding::Buffer { binding: 3, buffer: incoming, offset: 0 },
                        GpuBinding::Buffer { binding: 4, buffer: history, offset: 0 },
                        GpuBinding::Buffer { binding: 5, buffer: mesh_weights.unwrap_or(reference), offset: 0 },
                        GpuBinding::Buffer { binding: 6, buffer: scan_weights.unwrap_or(reference), offset: 0 },
                        GpuBinding::Texture { binding: 7, texture: surface_depth },
                        GpuBinding::Texture { binding: 8, texture: scene_depth },
                        GpuBinding::Sampler { binding: 9, sampler: depth_sampler },
                    ], 18, instance_count.max(1), GpuLoadAction::Clear, Self::TYPE_ID,
                );
            } else {
                gpu.native_enc.draw_instanced_msaa(
                    self.render_pipeline.as_ref().expect("pipeline initialized"),
                    self.msaa.as_ref().expect("MSAA initialized"), out,
                    &[
                        GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                        GpuBinding::Buffer { binding: 1, buffer: current, offset: 0 },
                        GpuBinding::Buffer { binding: 2, buffer: reference, offset: 0 },
                        GpuBinding::Buffer { binding: 3, buffer: incoming, offset: 0 },
                        GpuBinding::Buffer { binding: 4, buffer: history, offset: 0 },
                        GpuBinding::Buffer { binding: 5, buffer: mesh_weights.unwrap_or(reference), offset: 0 },
                        GpuBinding::Buffer { binding: 6, buffer: scan_weights.unwrap_or(reference), offset: 0 },
                    ], 18, instance_count.max(1), GpuLoadAction::Clear, Self::TYPE_ID,
                );
            }
        }
        if let Some(depth_out) = depth_out {
            if self.depth_pipeline.is_none() {
                self.depth_pipeline = Some(gpu.device.create_render_pipeline(
                    SHADER, "vs_main", "fs_depth", GpuTextureFormat::R32Float,
                    Some(GpuBlendState {
                        src_factor: GpuBlendFactor::One, dst_factor: GpuBlendFactor::One,
                        operation: GpuBlendOp::Max, src_alpha_factor: GpuBlendFactor::One,
                        dst_alpha_factor: GpuBlendFactor::One, alpha_operation: GpuBlendOp::Max,
                    }), Self::TYPE_ID,
                ));
            }
            if let Some(previous) = input_surface_depth {
                if (previous.width, previous.height, previous.format)
                    != (depth_out.width, depth_out.height, GpuTextureFormat::R32Float)
                {
                    log::error!("Mesh diagram surface depth accumulation requires matching canvas-sized R32Float inputs");
                    return;
                }
                gpu.copy_texture_to_texture(previous, depth_out, depth_out.width, depth_out.height);
            } else {
                gpu.native_enc.clear_texture(depth_out, 0.0, 0.0, 0.0, 0.0);
            }
            let mut depth_uniforms = uniforms;
            depth_uniforms.depth_pass = 1;
            depth_uniforms.occlusion = uniforms.occlusion;
            gpu.native_enc.draw_instanced(
                self.depth_pipeline.as_ref().expect("depth pipeline initialized"),
                depth_out,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&depth_uniforms) },
                    GpuBinding::Buffer { binding: 1, buffer: current, offset: 0 },
                    GpuBinding::Buffer { binding: 2, buffer: reference, offset: 0 },
                    GpuBinding::Buffer { binding: 3, buffer: incoming, offset: 0 },
                    // The depth entry point never reads history. Bind an
                    // existing vertex buffer to keep the shared layout valid
                    // without allocating or capturing trail state.
                    GpuBinding::Buffer { binding: 4, buffer: current, offset: 0 },
                    GpuBinding::Buffer { binding: 5, buffer: mesh_weights.unwrap_or(reference), offset: 0 },
                    GpuBinding::Buffer { binding: 6, buffer: scan_weights.unwrap_or(reference), offset: 0 },
                    GpuBinding::Texture { binding: 7, texture: surface_depth },
                    GpuBinding::Texture { binding: 8, texture: scene_depth },
                    GpuBinding::Sampler { binding: 9, sampler: depth_sampler },
                ],
                3, if uniforms.occlusion != 0 { tri_count } else { 0 }, GpuLoadAction::Load, Self::TYPE_ID,
            );
        }
        // The capture follows the diagram pass in the same command stream,
        // after the authored graph has produced `current`. This is a GPU
        // storage-buffer copy; no CPU readback or CPU-authored trajectory is
        // involved. Trails off: skip the copy entirely — the ring is only
        // read when trails render — and mark it stale so re-enabling starts
        // clean instead of replaying pre-toggle positions.
        if color_out.is_none() || uniforms.trails == 0 {
            self.history_reset = true;
        } else if vertex_count != 0 {
            let history = self.history.as_ref().expect("history allocated for colour pass");
            let slot = self.history_head;
            let capture_uniforms = HistoryCaptureUniforms {
                vertex_count,
                history_slot: slot,
                history_stride: MAX_VERTICES,
                _pad: 0,
            };
            gpu.native_enc.dispatch_compute(
                self.capture_pipeline
                    .as_ref()
                    .expect("capture pipeline initialized"),
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&capture_uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: current,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: history,
                        offset: 0,
                    },
                ],
                [vertex_count.div_ceil(256), 1, 1],
                "math-view-history-capture",
            );
            self.history_head = (slot + 1) % HISTORY_SAMPLES;
            self.history_len = (self.history_len + 1).min(HISTORY_SAMPLES);
            self.history_reset = false;
        }
    }

    fn clear_state(&mut self) {
        self.history_head = 0;
        self.history_len = 0;
        self.history_reset = true;
        self.last_seconds = None;
        self.last_frame_count = None;
    }
    fn output_canvas_scale(
        &self,
        port: &str,
        _p: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        matches!(port, "color" | "depth").then_some((1, 1))
    }

    fn output_format(&self, port: &str) -> Option<GpuTextureFormat> {
        (port == "depth").then_some(GpuTextureFormat::R32Float)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn diagram_declares_scalar_shadows() {
        assert_eq!(RenderMeshDiagram::TYPE_ID, "node.render_mesh_diagram");
        for name in [
            "grid",
            "axes",
            "fragments",
            "ghosts",
            "vectors",
            "trails",
            "density",
            "line_width",
            "geometry_hue",
            "path_hue",
            "radius",
        ] {
            assert!(
                RenderMeshDiagram::INPUTS.iter().any(|p| p.name == name),
                "missing scalar shadow {name}"
            );
        }
    }

    #[test]
    fn history_is_bounded_and_reset_is_explicit() {
        assert_eq!(HISTORY_SAMPLES, 64);
        assert_eq!(TRAIL_RENDER_SAMPLES, 32);
        let mut node = RenderMeshDiagram::new();
        node.history_head = 17;
        node.history_len = 64;
        node.last_seconds = Some(10.0);
        Primitive::clear_state(&mut node);
        assert_eq!((node.history_head, node.history_len), (0, 0));
        assert!(node.history_reset && node.last_seconds.is_none());
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "render_mesh_diagram/gpu_tests.rs"]
mod gpu_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "render_mesh_diagram_depth_tests.rs"]
mod depth_tests;
