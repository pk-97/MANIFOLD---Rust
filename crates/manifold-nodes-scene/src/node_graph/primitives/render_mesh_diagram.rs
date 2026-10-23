//! Native Metal presentation primitive for the Math View sparse mesh diagram.
//!
//! This node only presents buffers produced by the authored graph. It does
//! not contain a copy of any modifier or deformation equation. Geometry,
//! arrows, axes, the infinite world grid, and temporal trails share one bounded
//! instanced pass. The grid uses the scene camera without the object transform.

use manifold_node_engine::mesh::{InstanceTransform, MeshVertex};
use manifold_node_engine::scene::camera::Camera;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::{Primitive, PrimitiveSpec};
use manifold_node_engine::scene::transform::Transform;
use manifold_gpu::{
    GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuLoadAction, GpuSamplerDesc,
    GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use std::borrow::Cow;

const MSAA_SAMPLE_COUNT: u32 = 4;
const HISTORY_SAMPLES: u32 = 64;
const TRAIL_RENDER_SAMPLES: u32 = 32;
const MAX_VERTICES: u32 = 1536;
// Echo/copy chains (SpatialEchoes, SceneLoop) are capped at 8 visible copies
// per object, matching analytic_echo_instances' own echo ceiling. Total marks
// stay bounded by 4 * MAX_VERTICES/3 * 8 diagram instances plus trails.
const MAX_COPY_INSTANCES: u32 = 8;
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
    topology_mismatch: u32,
    occlusion: u32,
    mode: u32,
    _depth_pad: u32,
    inv_view_proj: [[f32; 4]; 4],
    camera_pos_far: [f32; 4],
    brightness: [f32;4],
    trails_brightness: f32,
    _brightness_pad0: f32,
    _brightness_pad1: f32,
    _brightness_pad2: f32,
    copy_count: u32,
    instances_wired: u32,
    instance_history_stride: u32,
    _instances_pad: u32,
    /// A copy group's per-axis scale inside each copy (`copy_scale`); one
    /// with no copies or no such scale.
    copy_inner: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HistoryCaptureUniforms {
    vertex_count: u32,
    history_slot: u32,
    history_stride: u32,
    copy_count: u32,
    instance_stride: u32,
    instance_capture: u32,
    _pad: [u32; 2],
}

manifold_node_engine::primitive! {
    name: RenderMeshDiagram,
    type_id: "node.render_mesh_diagram",
    purpose: "Render sparse MeshVertex samples as a transparent diagram through the authored Camera. Scene depth occludes the marks in Overlay mode when Occlusion is on; an optional InstanceTransform array repeats fragments, ghosts, displacement arrows and motion trails per copy so echo chains are visible. Optional full current and reference meshes are a count-only topology guard. This presentation node contains no modifier math.",
    inputs: {
        current: Array(MeshVertex) required,
        reference: Array(MeshVertex) required,
        incoming: Array(MeshVertex) required,
        instances: Array(InstanceTransform) optional,
        copy_scale: Transform optional,
        camera: Camera required,
        transform: Transform optional,
        parent_transform: Transform optional,
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
        grid_brightness: ScalarF32 optional,
        fragments_brightness: ScalarF32 optional,
        ghosts_brightness: ScalarF32 optional,
        vectors_brightness: ScalarF32 optional,
        trails_brightness: ScalarF32 optional,
        current_mesh: Array(MeshVertex) optional,
        reference_mesh: Array(MeshVertex) optional,
        scene_depth: Texture2D optional,
        occlusion: ScalarF32 optional,
        mode: ScalarF32 optional,
    },
    outputs: { color: Texture2D },
    params: [
        ParamDef { name: Cow::Borrowed("grid_brightness"), label: "grid brightness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fragments_brightness"), label: "fragments brightness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("ghosts_brightness"), label: "ghosts brightness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("vectors_brightness"), label: "vectors brightness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("trails_brightness"), label: "trails brightness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
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
    composition_notes: "Current, reference, and incoming arrays are sampled faces of real meshes. The transparent diagram uses the Camera and optional object Transform, inside the optional parent_transform (the frame the object draws in: its groups, or its body's pose), as node.scene_object composes them; Motion trails are temporal history, never parameter-sweep trajectories. Occlusion=1 with Mode=2 tests scene_depth at its own resolution; Mode 1 never occludes. When current_mesh and reference_mesh are both wired and differ in size, reference and incoming read as current (ghosts coincide, arrows hide). Ghosts/trails remain translucent. Optional copy_scale (a copies body's copy_scale) scales each copy per axis inside its instance, as the scene draws it; it applies only with instances wired.",
    examples: [], picker: { label: "Mesh Diagram", category: Atom },
    summary: "Draws sparse evaluated mesh samples as a transparent native diagram.",
    category: Geometry3D, role: Filter, aliases: ["mesh diagram", "math view"], boundary_reason: DrawCall,
    extra_fields: {
        render_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        depth_color_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        depth_sampler: Option<manifold_gpu::GpuSampler> = None,
        dummy_depth: Option<manifold_gpu::GpuTexture> = None,
        identity_instances: Option<manifold_gpu::GpuBuffer> = None,
        capture_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        msaa: Option<manifold_gpu::GpuTexture> = None,
        width: u32 = 0,
        height: u32 = 0,
        history: Option<manifold_gpu::GpuBuffer> = None,
        // Per-copy InstanceTransform ring, recorded by the same capture pass
        // as the vertex ring. Only allocated once the instances port has been
        // wired, so vertices-only diagrams pay nothing.
        instance_history: Option<manifold_gpu::GpuBuffer> = None,
        instance_counts: Option<manifold_gpu::GpuBuffer> = None,
        history_head: u32 = 0,
        history_len: u32 = 0,
        last_seconds: Option<f64> = None,
        last_frame_count: Option<i64> = None,
        history_reset: bool = false,
        last_vertex_count: u32 = 0,
        last_copy_count: u32 = 0,
        topology_warned: bool = false,
    },
}

const SHADER: &str = include_str!("shaders/render_mesh_diagram.wgsl");
const CAPTURE_SHADER: &str = include_str!("shaders/history_capture.wgsl");

inventory::submit! {
    manifold_node_engine::runtime::plugin_prewarm::PipelinePrewarm {
        purpose: manifold_node_engine::runtime::plugin_prewarm::PipelinePrewarmPurpose::MathView,
        run: RenderMeshDiagram::prewarm_pipelines,
    }
}


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

    fn ensure_instance_history(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.instance_history.is_some() {
            return;
        }
        // 64 slots x 8 copies x 32 B = 16 KiB, plus 256 B of per-slot copy
        // counts — trivial next to the 1.5 MB vertex ring.
        let ring_bytes = HISTORY_SAMPLES as u64
            * MAX_COPY_INSTANCES as u64
            * std::mem::size_of::<InstanceTransform>() as u64;
        let ring = device.create_buffer_shared(ring_bytes);
        ring.zero_fill();
        let counts = device.create_buffer_shared(HISTORY_SAMPLES as u64 * 4);
        counts.zero_fill();
        self.instance_history = Some(ring);
        self.instance_counts = Some(counts);
    }

    /// Copy-count change invalidates recorded instance history: old slots may
    /// never have held copy c, and marks must not linger from a bypassed or
    /// reordered producer. The shared head/len reset covers the vertex ring in
    /// the same stroke.
    fn note_copy_count(&mut self, copies: u32) {
        if copies == self.last_copy_count {
            return;
        }
        self.history_head = 0;
        self.history_len = 0;
        self.last_copy_count = copies;
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
        if self.identity_instances.is_none() {
            // Bound at the instances slot when the port is unwired so the
            // shared layout stays valid; the shader never reads it in that
            // mode (instances_wired == 0 selects the identity constant).
            let buffer = device.create_buffer_shared(std::mem::size_of::<InstanceTransform>() as u64);
            let identity = [InstanceTransform {
                pos_scale: [0.0, 0.0, 0.0, 1.0],
                rot_pad: [0.0; 4],
            }];
            // SAFETY: 32 bytes into a 32-byte fresh buffer; no GPU work has
            // touched it yet.
            unsafe { buffer.write(0, bytemuck::bytes_of(&identity)) };
            self.identity_instances = Some(buffer);
        }
    }

    fn model_matrix(t: Transform, parent: Option<Transform>, camera: &Camera) -> [[f32; 4]; 4] {
        let turn = |t: &Transform| if t.billboard { t.billboard_rot_euler(camera.pos) } else { t.rot_euler };
        let own = super::render_scene::model_matrix(t.pos, turn(&t), t.scale);
        match parent {
            Some(parent) => manifold_node_engine::mesh::pipeline::mat4_mul(parent.world_with_rotation(turn(&parent)), own),
            None => own,
        }
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
        if color_out.is_none() {
            return;
        }
        let (width, height) = color_out
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
        // Copy count comes from the wired InstanceTransform buffer. Producers
        // collapse inactive slots to an all-zero transform (see
        // generate_instance_transforms_body.wgsl), so a smaller active_count
        // never draws garbage; the shader skips those sentinels explicitly.
        let input_instances = ctx.inputs.array("instances");
        let copies = input_instances.map_or(1, |buffer| {
            ((buffer.size / std::mem::size_of::<InstanceTransform>() as u64) as u32)
                .clamp(1, MAX_COPY_INSTANCES)
        });
        self.note_copy_count(copies);
        let capacity = ((current.size.min(reference.size).min(incoming.size)
            / std::mem::size_of::<MeshVertex>() as u64) as u32)
            .min(MAX_VERTICES);
        let tri_count = (density * density * density).min(capacity / 3);
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
        // Face correspondence needs the same full mesh size on both sides.
        // Counts only: the full buffers are never read here.
        let topology_mismatch = match (
            ctx.inputs.array("current_mesh"),
            ctx.inputs.array("reference_mesh"),
        ) {
            (Some(current_mesh), Some(reference_mesh)) => current_mesh.size != reference_mesh.size,
            _ => false,
        };
        if topology_mismatch && !self.topology_warned {
            log::warn!(
                "Math View: the mesh topology changed upstream; ghosts and arrows are hidden for this object"
            );
        }
        self.topology_warned = topology_mismatch;
        let mode = ctx.scalar_or_param("mode", 0.0).round().clamp(0.0, 2.0) as u32;
        // Depth occlusion applies to Overlay only; Math draws on black.
        let occlusion = ctx.scalar_or_param("occlusion", 0.0).round().clamp(0.0, 1.0) as u32;
        // Copies draw at the group's per-axis scale inside each copy, as the
        // scene draws them; without copies the scale is exactly one, so the
        // marks stay byte-identical.
        let copy_inner = match ctx.inputs.transform("copy_scale") {
            Some(copy) if input_instances.is_some() && copy.scale.iter().all(|s| s.is_finite()) => {
                [copy.scale[0], copy.scale[1], copy.scale[2], 0.0]
            }
            _ => [1.0, 1.0, 1.0, 0.0],
        };
        let uniforms = DiagramUniforms {
            view_proj,
            model: Self::model_matrix(
                ctx.inputs.transform("transform").unwrap_or_default(),
                ctx.inputs.transform("parent_transform"),
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
            topology_mismatch: u32::from(topology_mismatch),
            occlusion: u32::from(occlusion != 0 && mode == 2),
            mode,
            _depth_pad: 0,
            inv_view_proj,
            camera_pos_far: [camera.pos[0], camera.pos[1], camera.pos[2], camera.far],
            brightness: ["grid_brightness","fragments_brightness","ghosts_brightness","vectors_brightness"].map(|n|ctx.scalar_or_param(n,1.0).max(0.0)),
            trails_brightness: ctx.scalar_or_param("trails_brightness", 1.0).max(0.0),
            _brightness_pad0: 0.0,
            _brightness_pad1: 0.0,
            _brightness_pad2: 0.0,
            copy_count: copies,
            instances_wired: u32::from(input_instances.is_some()),
            instance_history_stride: MAX_COPY_INSTANCES,
            _instances_pad: 0,
            copy_inner,
        };
        let input_scene_depth = ctx.inputs.texture_2d("scene_depth");
        let gpu = ctx.gpu_encoder();
        if color_out.is_some() {
            self.ensure_history(gpu.device);
            if input_instances.is_some() {
                self.ensure_instance_history(gpu.device);
            }
        }
        self.ensure_depth_resources(gpu.device);
        let dummy_depth = self.dummy_depth.as_ref().expect("depth resources initialized");
        let scene_depth = input_scene_depth.unwrap_or(dummy_depth);
        let depth_sampler = self.depth_sampler.as_ref().expect("depth sampler initialized");
        let instances = input_instances.unwrap_or_else(|| {
            self.identity_instances
                .as_ref()
                .expect("identity instance stub initialized")
        });
        // Instance rings are only allocated once the instances port has been
        // wired; before that an existing read-only buffer keeps the shared
        // layout valid (the shader never reads them when unwired).
        let instance_ring = self.instance_history.as_ref().unwrap_or(current);
        let instance_ring_counts = self.instance_counts.as_ref().unwrap_or(current);

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
        let arrow_count = tri_count * copies;
        let grid_count = 1;
        // One world frame and at most three representative fragment frames.
        let axes_count = 3 + tri_count.min(3) * 3;
        // Fragments, reference, ghosts, arrows and trails repeat per copy
        // transform; a single identity copy keeps the instance budget
        // byte-identical to the vertices-only diagram.
        let trail_copies = if input_instances.is_some() { copies } else { 1 };
        let trail_count = if uniforms.trails != 0 {
            TRAIL_RENDER_SAMPLES * vertex_count * trail_copies
        } else {
            0
        };
        let instance_count = tri_count * 3 * copies + arrow_count + grid_count + axes_count + trail_count;
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
                        GpuBinding::Texture { binding: 8, texture: scene_depth },
                        GpuBinding::Sampler { binding: 9, sampler: depth_sampler },
                        GpuBinding::Buffer { binding: 10, buffer: instances, offset: 0 },
                        GpuBinding::Buffer { binding: 11, buffer: instance_ring, offset: 0 },
                        GpuBinding::Buffer { binding: 12, buffer: instance_ring_counts, offset: 0 },
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
                        GpuBinding::Buffer { binding: 10, buffer: instances, offset: 0 },
                        GpuBinding::Buffer { binding: 11, buffer: instance_ring, offset: 0 },
                        GpuBinding::Buffer { binding: 12, buffer: instance_ring_counts, offset: 0 },
                    ], 18, instance_count.max(1), GpuLoadAction::Clear, Self::TYPE_ID,
                );
            }
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
            // Record the per-copy transforms the wired producer published this
            // frame, next to the vertex sample of the same slot. Unwired:
            // copy_count 0 guards every instance-ring read and write, so the
            // ring (if it exists) stays untouched and vertices-only behaviour
            // is unchanged.
            let instance_capture = u32::from(input_instances.is_some());
            let capture_uniforms = HistoryCaptureUniforms {
                vertex_count,
                history_slot: slot,
                history_stride: MAX_VERTICES,
                copy_count: if instance_capture != 0 { copies } else { 0 },
                instance_stride: MAX_COPY_INSTANCES,
                instance_capture,
                _pad: [0; 2],
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
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: instances,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 4,
                        buffer: instance_ring,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 5,
                        buffer: instance_ring_counts,
                        offset: 0,
                    },
                ],
                [
                    vertex_count
                        .max(if instance_capture != 0 { copies } else { 0 })
                        .div_ceil(256),
                    1,
                    1,
                ],
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
        _p: &manifold_node_engine::exec::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        (port == "color").then_some((1, 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn copy_count_change_resets_history() {
        let mut node = RenderMeshDiagram::new();
        // First observed count only initialises the tracker: an empty ring
        // has nothing to invalidate.
        node.note_copy_count(3);
        assert_eq!((node.history_head, node.history_len), (0, 0));
        node.history_head = 40;
        node.history_len = 40;
        node.note_copy_count(3);
        assert_eq!((node.history_head, node.history_len), (40, 40), "steady count keeps history");
        // Grow, shrink, and bypass (wired -> unwired reports 1) all clear the
        // shared rings so no slot outlives its copy-count era.
        for next in [5u32, 2, 1] {
            node.note_copy_count(next);
            assert_eq!((node.history_head, node.history_len), (0, 0));
            node.history_head = 9;
            node.history_len = 9;
        }
    }
}

/// `MARK_SIZE` in the shader: a smaller face's mark grows until its farthest
/// corner is this fraction of the item's own radius from its centre.
#[cfg(test)]
const MARK_SIZE: f32 = 0.08;

#[cfg(test)]
#[test]
fn the_mark_size_oracle_matches_the_shader() {
    assert!(SHADER.contains(&format!("const MARK_SIZE: f32 = {MARK_SIZE:?};")));
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
mod depth_tests;

#[cfg(test)]
mod prewarm_source_tests {
    #[test]
    fn pipeline_prewarm_registration_uses_direct_prewarmer() {
        let entries: Vec<_> = inventory::iter::<manifold_node_engine::runtime::plugin_prewarm::PipelinePrewarm>
            .into_iter().filter(|entry| entry.purpose == manifold_node_engine::runtime::plugin_prewarm::PipelinePrewarmPurpose::MathView).collect();
        assert_eq!(entries.len(), 1);
        assert!(std::ptr::fn_addr_eq(entries[0].run,
            super::RenderMeshDiagram::prewarm_pipelines as fn(&manifold_gpu::GpuDevice)));
    }
}
