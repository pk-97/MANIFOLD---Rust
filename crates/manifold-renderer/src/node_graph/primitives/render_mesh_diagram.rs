//! Native Metal presentation primitive for the Math View sparse mesh diagram.
//!
//! This node only presents buffers produced by the authored graph. It does
//! not contain a copy of any modifier or deformation equation. Geometry,
//! arrows, axes, the ground grid, and temporal trails share one bounded
//! instanced line pass.

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::{Primitive, PrimitiveSpec};
use crate::node_graph::transform::Transform;
use manifold_gpu::{GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuLoadAction};
use std::borrow::Cow;

const MSAA_SAMPLE_COUNT: u32 = 4;
const HISTORY_SAMPLES: u32 = 64;
const TRAIL_RENDER_SAMPLES: u32 = 32;
const MAX_VERTICES: u32 = 1536;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DiagramUniforms {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    viewport: [f32; 4],
    source_offset: [f32; 4],
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
    _pad: u32,
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
    purpose: "Render sparse evaluated MeshVertex samples through the authored Camera for Math View. This presentation node contains no modifier math.",
    inputs: {
        current: Array(MeshVertex) required,
        reference: Array(MeshVertex) required,
        incoming: Array(MeshVertex) required,
        camera: Camera required,
        transform: Transform optional,
        grid: ScalarF32 optional,
        fragments: ScalarF32 optional,
        ghosts: ScalarF32 optional,
        vectors: ScalarF32 optional,
        trails: ScalarF32 optional,
        density: ScalarF32 optional,
        line_width: ScalarF32 optional,
        geometry_hue: ScalarF32 optional,
        path_hue: ScalarF32 optional,
        radius: ScalarF32 optional,
        source_offset_x: ScalarF32 optional,
        source_offset_y: ScalarF32 optional,
        source_offset_z: ScalarF32 optional,
    },
    outputs: { color: Texture2D },
    params: [
        ParamDef { name: Cow::Borrowed("grid"), label: "Grid", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fragments"), label: "Fragments", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("ghosts"), label: "Ghosts", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("vectors"), label: "Vectors", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("trails"), label: "Motion Trails", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("density"), label: "Density", ty: ParamType::Int, default: ParamValue::Float(4.0), range: Some((2.0, 8.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("line_width"), label: "Line Width", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.5, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("geometry_hue"), label: "Geometry Hue", ty: ParamType::Float, default: ParamValue::Float(0.52), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("path_hue"), label: "Path Hue", ty: ParamType::Float, default: ParamValue::Float(0.13), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("radius"), label: "Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.001, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Current, reference, and incoming arrays are authored graph outputs. The transparent overlay uses the real Camera and optional object Transform; Motion trails are temporal history, never parameter-sweep trajectories.",
    examples: [], picker: { label: "Mesh Diagram", category: Atom },
    summary: "Draws sparse evaluated mesh samples as a transparent native diagram.",
    category: Geometry3D, role: Filter, aliases: ["mesh diagram", "math view"], boundary_reason: DrawCall,
    extra_fields: {
        render_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
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

const SHADER: &str = include_str!("shaders/render_mesh_diagram.wgsl");
const CAPTURE_SHADER: &str = include_str!("shaders/history_capture.wgsl");

impl RenderMeshDiagram {
    /// Startup prewarm hook used by the native generator registry.
    pub fn prewarm_pipelines(device: &manifold_gpu::GpuDevice) {
        let blend = GpuBlendState {
            src_factor: GpuBlendFactor::SrcAlpha,
            dst_factor: GpuBlendFactor::OneMinusSrcAlpha,
            operation: GpuBlendOp::Add,
            src_alpha_factor: GpuBlendFactor::One,
            dst_alpha_factor: GpuBlendFactor::OneMinusSrcAlpha,
            alpha_operation: GpuBlendOp::Add,
        };
        device.create_render_pipeline_msaa(
            SHADER,
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            Some(blend),
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
        let Some(out) = ctx.outputs.texture_2d("color") else {
            return;
        };
        if out.width == 0 || out.height == 0 {
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
        let tri_count = (density * density * density).min(capacity / 3);
        let vertex_count = tri_count * 3;
        if vertex_count != self.last_vertex_count {
            self.history_head = 0;
            self.history_len = 0;
            self.last_vertex_count = vertex_count;
        }
        let toggled = |name: &str| u32::from(ctx.scalar_or_param(name, 1.0) > 0.5);
        let uniforms = DiagramUniforms {
            view_proj: camera.view_proj(out.width as f32 / out.height as f32),
            model: Self::model_matrix(
                ctx.inputs.transform("transform").unwrap_or_default(),
                &camera,
            ),
            viewport: [out.width as f32, out.height as f32, 0.0, 0.0],
            source_offset: [
                ctx.scalar_or_param("source_offset_x", 0.0),
                ctx.scalar_or_param("source_offset_y", 0.0),
                ctx.scalar_or_param("source_offset_z", 0.0),
                0.0,
            ],
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
            _pad: 0,
        };
        let gpu = ctx.gpu_encoder();
        self.ensure_history(gpu.device);
        if self.render_pipeline.is_none() {
            let blend = GpuBlendState {
                src_factor: GpuBlendFactor::SrcAlpha,
                dst_factor: GpuBlendFactor::OneMinusSrcAlpha,
                operation: GpuBlendOp::Add,
                src_alpha_factor: GpuBlendFactor::One,
                dst_alpha_factor: GpuBlendFactor::OneMinusSrcAlpha,
                alpha_operation: GpuBlendOp::Add,
            };
            self.render_pipeline = Some(gpu.device.create_render_pipeline_msaa(
                SHADER,
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                Some(blend),
                MSAA_SAMPLE_COUNT,
                Self::TYPE_ID,
            ));
        }
        if self.capture_pipeline.is_none() {
            self.capture_pipeline = Some(gpu.device.create_compute_pipeline(
                CAPTURE_SHADER,
                "cs_main",
                "math-view-history-capture",
            ));
        }
        if self.width != out.width || self.height != out.height || self.msaa.is_none() {
            self.msaa = Some(gpu.device.create_texture_msaa_memoryless(
                out.width,
                out.height,
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                MSAA_SAMPLE_COUNT,
                "node.render_mesh_diagram MSAA",
            ));
            self.width = out.width;
            self.height = out.height;
        }
        let history = self.history.as_ref().expect("history allocated");
        let arrow_count = tri_count;
        let grid_count = 22;
        let axes_count = 6;
        let trail_count = if uniforms.trails != 0 {
            TRAIL_RENDER_SAMPLES * vertex_count
        } else {
            0
        };
        let instance_count = tri_count * 3 + arrow_count + grid_count + axes_count + trail_count;
        gpu.native_enc.draw_instanced_msaa(
            self.render_pipeline.as_ref().expect("pipeline initialized"),
            self.msaa.as_ref().expect("MSAA initialized"),
            out,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: current,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: reference,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: incoming,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: history,
                    offset: 0,
                },
            ],
            18,
            instance_count.max(1),
            GpuLoadAction::Clear,
            Self::TYPE_ID,
        );
        // The capture follows the diagram pass in the same command stream,
        // after the authored graph has produced `current`. This is a GPU
        // storage-buffer copy; no CPU readback or CPU-authored trajectory is
        // involved.
        if vertex_count != 0 {
            if self.history_reset {
                self.history_head = 0;
                self.history_len = 0;
            }
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
        (port == "color").then_some((1, 1))
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
            "fragments",
            "ghosts",
            "vectors",
            "trails",
            "density",
            "line_width",
            "geometry_hue",
            "path_hue",
            "radius",
            "source_offset_x",
            "source_offset_y",
            "source_offset_z",
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
