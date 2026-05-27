//! `node.render_3d_mesh` — first vertex+fragment render-pass
//! primitive in the node_graph runtime. Phase B of
//! `BUFFER_PORT_PLAN`.
//!
//! Reads an `Array<MeshVertex>` as a triangle list (3 consecutive
//! vertices = 1 triangle), depth-tests it, and writes the result
//! into a `Texture2D` output. Camera control is via scalar params
//! (orbit / tilt / distance / FOV / look_y); lighting is a single
//! directional light + ambient.
//!
//! State held by the primitive instance (via `extra_fields`):
//! - the render pipeline (lazy)
//! - a depth-stencil state (lazy)
//! - a depth texture sized to the most recent output texture
//!   (lazy, resized on dimension change)

use manifold_gpu::{GpuBinding, GpuLoadAction};

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshRenderUniforms {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    light_dir: [f32; 4],
    light_color: [f32; 4],
    base_color: [f32; 4],
}

crate::primitive! {
    name: Render3DMesh,
    type_id: "node.render_3d_mesh",
    purpose: "Render an Array<MeshVertex> as a depth-tested triangle list (every 3 consecutive vertices form one triangle). One directional light + ambient. Takes a `camera: Camera` input from `node.camera_orbit` (or any future Camera source) — the renderer reads view + projection from the camera instead of holding its own orbit/tilt/distance/FOV scalars. Emits three outputs: `color` (Lambert + ambient shaded), `world_pos` (interpolated per-pixel world position, alpha=1 where geometry covers), `world_normal` (renormalised per-pixel surface normal in world space, alpha=1). The G-buffer outputs let downstream PBR / SSAO / SSR atoms shade in screen space with per-pixel V and L derived from world coordinates — TouchDesigner / Blender / Unreal deferred-shading style.",
    inputs: {
        vertices: Array(MeshVertex) required,
        camera: Camera required,
    },
    outputs: {
        color: Texture2D,
        world_pos: Texture2D,
        world_normal: Texture2D,
    },
    params: [
        ParamDef {
            name: "light_intensity",
            label: "Light Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "ambient",
            label: "Ambient",
            ty: ParamType::Float,
            default: ParamValue::Float(0.15),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "color_r",
            label: "Color R",
            ty: ParamType::Float,
            default: ParamValue::Float(0.85),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "color_g",
            label: "Color G",
            ty: ParamType::Float,
            default: ParamValue::Float(0.88),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "color_b",
            label: "Color B",
            ty: ParamType::Float,
            default: ParamValue::Float(0.92),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Vertex count must be a multiple of 3; the trailing 0/1/2 leftover verts are silently truncated. Producer must emit triangle order — pair with a future Triangulate primitive when the upstream is a positions-only grid. Output `color` is Rgba16Float with pre-multiplied background = transparent. Outputs `world_pos` / `world_normal` are Rgba16Float G-buffer attachments emitted via two extra single-attachment render passes (the vertex shader runs three times — cheap at typical mesh sizes; lets the runtime stay on the existing single-color-attachment encoder API). Downstream PBR atoms (cook_torrance_specular, equirect_envmap_sample) consume `world_pos` plus camera/light positions to compute per-pixel V and L for perspective-correct shading.",
    examples: [],
    picker: { label: "Render 3D Mesh", category: Atom },
    extra_fields: {
        render_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        world_pos_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        world_normal_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        depth_stencil: Option<manifold_gpu::GpuDepthStencilState> = None,
        depth_texture: Option<manifold_gpu::GpuTexture> = None,
        depth_width: u32 = 0,
        depth_height: u32 = 0,
    },
}

impl Render3DMesh {
    fn ensure_depth_texture(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.depth_width == width
            && self.depth_height == height
            && self.depth_texture.is_some()
        {
            return;
        }
        self.depth_texture = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Depth32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET,
            label: "node.render_3d_mesh depth",
            mip_levels: 1,
        }));
        self.depth_width = width;
        self.depth_height = height;
    }
}

impl Primitive for Render3DMesh {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);
        let light_intensity = match ctx.params.get("light_intensity") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let ambient = match ctx.params.get("ambient") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.15,
        };
        let color_r = match ctx.params.get("color_r") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.85,
        };
        let color_g = match ctx.params.get("color_g") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.88,
        };
        let color_b = match ctx.params.get("color_b") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.92,
        };

        let Some(vertices) = ctx.inputs.array("vertices") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("color") else {
            return;
        };
        let world_pos_target = ctx.outputs.texture_2d("world_pos");
        let world_normal_target = ctx.outputs.texture_2d("world_normal");
        let width = target.width;
        let height = target.height;
        if width == 0 || height == 0 {
            return;
        }

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let vertex_capacity = (vertices.size / vertex_size) as u32;
        // Round down to a multiple of 3 — trailing partial triangle is skipped.
        let vertex_count = (vertex_capacity / 3) * 3;
        if vertex_count == 0 {
            // No geometry — emit a clear pass so the output isn't garbage.
            let gpu = ctx.gpu_encoder();
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            if let Some(wp) = world_pos_target {
                gpu.native_enc.clear_texture(wp, 0.0, 0.0, 0.0, 0.0);
            }
            if let Some(wn) = world_normal_target {
                gpu.native_enc.clear_texture(wn, 0.0, 0.0, 0.0, 0.0);
            }
            return;
        }

        let aspect = width as f32 / height as f32;
        let view_proj = cam.view_proj(aspect);

        let uniforms = MeshRenderUniforms {
            view_proj,
            camera_pos: [cam.pos[0], cam.pos[1], cam.pos[2], 1.0],
            light_dir: [0.3, 0.7, 0.6, light_intensity],
            light_color: [1.0, 1.0, 1.0, ambient],
            base_color: [color_r, color_g, color_b, 1.0],
        };

        let gpu = ctx.gpu_encoder();

        if self.render_pipeline.is_none() {
            self.render_pipeline = Some(gpu.device.create_render_pipeline_depth(
                include_str!("shaders/render_3d_mesh.wgsl"),
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                manifold_gpu::GpuTextureFormat::Depth32Float,
                None,
                1,
                "node.render_3d_mesh",
            ));
        }
        if self.world_pos_pipeline.is_none() {
            self.world_pos_pipeline = Some(gpu.device.create_render_pipeline_depth(
                include_str!("shaders/render_3d_mesh.wgsl"),
                "vs_main",
                "fs_world_pos",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                manifold_gpu::GpuTextureFormat::Depth32Float,
                None,
                1,
                "node.render_3d_mesh.world_pos",
            ));
        }
        if self.world_normal_pipeline.is_none() {
            self.world_normal_pipeline = Some(gpu.device.create_render_pipeline_depth(
                include_str!("shaders/render_3d_mesh.wgsl"),
                "vs_main",
                "fs_world_normal",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                manifold_gpu::GpuTextureFormat::Depth32Float,
                None,
                1,
                "node.render_3d_mesh.world_normal",
            ));
        }
        if self.depth_stencil.is_none() {
            self.depth_stencil = Some(
                gpu.device
                    .create_depth_stencil_state(&manifold_gpu::GpuDepthStencilDesc {
                        compare: manifold_gpu::GpuCompareFunction::Less,
                        write_enabled: true,
                    }),
            );
        }
        self.ensure_depth_texture(gpu.device, width, height);

        let pipeline = self.render_pipeline.as_ref().expect("just inserted");
        let world_pos_pipeline = self.world_pos_pipeline.as_ref().expect("just inserted");
        let world_normal_pipeline = self.world_normal_pipeline.as_ref().expect("just inserted");
        let depth_stencil = self.depth_stencil.as_ref().expect("just inserted");
        let depth_tex = self.depth_texture.as_ref().expect("just inserted");

        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: vertices,
                offset: 0,
            },
        ];

        gpu.native_enc.draw_instanced_depth(
            pipeline,
            target,
            depth_tex,
            depth_stencil,
            &bindings,
            vertex_count,
            1,
            GpuLoadAction::Clear,
            "node.render_3d_mesh",
        );

        // G-buffer passes. Each is a Clear of its color attachment + depth,
        // followed by a re-rasterization of the same geometry. Depth is
        // deterministic across passes — the rasterizer produces the same
        // depth values from the same vertex buffer + view_proj, so the
        // depth-Less test admits identical fragments and the outputs align
        // pixel-perfect with the color pass.
        if let Some(wp_target) = world_pos_target {
            gpu.native_enc.draw_instanced_depth(
                world_pos_pipeline,
                wp_target,
                depth_tex,
                depth_stencil,
                &bindings,
                vertex_count,
                1,
                GpuLoadAction::Clear,
                "node.render_3d_mesh.world_pos",
            );
        }
        if let Some(wn_target) = world_normal_target {
            gpu.native_enc.draw_instanced_depth(
                world_normal_pipeline,
                wn_target,
                depth_tex,
                depth_stencil,
                &bindings,
                vertex_count,
                1,
                GpuLoadAction::Clear,
                "node.render_3d_mesh.world_normal",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn render_3d_mesh_declares_array_mesh_camera_in_and_texture_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();

        assert_eq!(Render3DMesh::TYPE_ID, "node.render_3d_mesh");
        assert_eq!(Render3DMesh::INPUTS.len(), 2);
        assert_eq!(Render3DMesh::INPUTS[0].name, "vertices");
        assert!(Render3DMesh::INPUTS[0].required);
        assert_eq!(
            Render3DMesh::INPUTS[0].ty,
            PortType::Array(mesh_layout)
        );
        assert_eq!(Render3DMesh::INPUTS[1].name, "camera");
        assert!(Render3DMesh::INPUTS[1].required);
        assert_eq!(Render3DMesh::INPUTS[1].ty, PortType::Camera);
        assert_eq!(Render3DMesh::OUTPUTS.len(), 3);
        assert_eq!(Render3DMesh::OUTPUTS[0].name, "color");
        assert_eq!(Render3DMesh::OUTPUTS[0].ty, PortType::Texture2D);
        assert_eq!(Render3DMesh::OUTPUTS[1].name, "world_pos");
        assert_eq!(Render3DMesh::OUTPUTS[1].ty, PortType::Texture2D);
        assert_eq!(Render3DMesh::OUTPUTS[2].name, "world_normal");
        assert_eq!(Render3DMesh::OUTPUTS[2].ty, PortType::Texture2D);
    }

    #[test]
    fn render_3d_mesh_has_lighting_color_params() {
        let names: Vec<&str> = Render3DMesh::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "light_intensity",
                "ambient",
                "color_r",
                "color_g",
                "color_b",
            ]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Render3DMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.render_3d_mesh");
    }
}
