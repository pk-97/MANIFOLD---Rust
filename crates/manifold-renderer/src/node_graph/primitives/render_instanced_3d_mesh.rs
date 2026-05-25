//! `node.render_instanced_3d_mesh` — instanced triangle-list
//! mesh rendering. Each instance applies a per-instance position
//! + scale + Euler rotation to the same base mesh vertices.
//!
//! Phase B of `BUFFER_PORT_PLAN`. Second render-pass primitive,
//! pairs naturally with `node.generate_instance_transforms` and
//! whatever procedural mesh sits upstream. Unlocks the
//! NestedCubes / DigitalPlants family.

use manifold_gpu::{GpuBinding, GpuLoadAction};

use crate::generators::mesh_common::{InstanceTransform, MeshVertex};
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InstancedRenderUniforms {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    light_dir: [f32; 4],
    light_color: [f32; 4],
    base_color: [f32; 4],
}

crate::primitive! {
    name: RenderInstanced3DMesh,
    type_id: "node.render_instanced_3d_mesh",
    purpose: "Render N copies of an Array<MeshVertex> base mesh, each transformed by an Array<InstanceTransform> entry. Triangle list topology (every 3 verts = 1 triangle). One directional light + ambient. Takes a `camera: Camera` input. Pair with node.generate_instance_transforms to drive NestedCubes / DigitalPlants-shaped graphs.",
    inputs: {
        vertices: Array(MeshVertex) required,
        instances: Array(InstanceTransform) required,
        camera: Camera required,
    },
    outputs: {
        color: Texture2D,
    },
    params: [
        ParamDef {
            name: "instance_count",
            label: "Instance Count",
            ty: ParamType::Int,
            default: ParamValue::Float(64.0),
            range: Some((1.0, 1_000_000.0)),
            enum_values: &[],
        },
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
    composition_notes: "Vertex count must be a multiple of 3 (trailing partial triangle truncated). instance_count is clamped to the buffer's capacity. The instance Array dictates how many copies are drawn; an active_count semantics there is upstream's responsibility.",
    examples: [],
    picker: { label: "Render Instanced 3D Mesh", category: Atom },
    extra_fields: {
        render_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        depth_stencil: Option<manifold_gpu::GpuDepthStencilState> = None,
        depth_texture: Option<manifold_gpu::GpuTexture> = None,
        depth_width: u32 = 0,
        depth_height: u32 = 0,
    },
}

impl RenderInstanced3DMesh {
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
            label: "node.render_instanced_3d_mesh depth",
            mip_levels: 1,
        }));
        self.depth_width = width;
        self.depth_height = height;
    }
}

impl Primitive for RenderInstanced3DMesh {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let instance_count = match ctx.params.get("instance_count") {
            Some(ParamValue::Float(n)) => n.round().max(0_f32) as u32,
            _ => 64,
        };
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
        let Some(instances) = ctx.inputs.array("instances") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("color") else {
            return;
        };
        let width = target.width;
        let height = target.height;
        if width == 0 || height == 0 {
            return;
        }

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let vertex_capacity = (vertices.size / vertex_size) as u32;
        let vertex_count = (vertex_capacity / 3) * 3;
        let instance_size = std::mem::size_of::<InstanceTransform>() as u64;
        let instance_capacity = (instances.size / instance_size) as u32;
        let instance_count = instance_count.min(instance_capacity);
        if vertex_count == 0 || instance_count == 0 {
            let gpu = ctx.gpu_encoder();
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            return;
        }

        let aspect = width as f32 / height as f32;
        let view_proj = cam.view_proj(aspect);

        let uniforms = InstancedRenderUniforms {
            view_proj,
            camera_pos: [cam.pos[0], cam.pos[1], cam.pos[2], 1.0],
            light_dir: [0.3, 0.7, 0.6, light_intensity],
            light_color: [1.0, 1.0, 1.0, ambient],
            base_color: [color_r, color_g, color_b, 1.0],
        };

        let gpu = ctx.gpu_encoder();
        if self.render_pipeline.is_none() {
            self.render_pipeline = Some(gpu.device.create_render_pipeline_depth(
                include_str!("shaders/render_instanced_3d_mesh.wgsl"),
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                manifold_gpu::GpuTextureFormat::Depth32Float,
                None,
                1,
                "node.render_instanced_3d_mesh",
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
        let depth_stencil = self.depth_stencil.as_ref().expect("just inserted");
        let depth_tex = self.depth_texture.as_ref().expect("just inserted");

        gpu.native_enc.draw_instanced_depth(
            pipeline,
            target,
            depth_tex,
            depth_stencil,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: vertices,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: instances,
                    offset: 0,
                },
            ],
            vertex_count,
            instance_count,
            GpuLoadAction::Clear,
            "node.render_instanced_3d_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn render_instanced_declares_mesh_instance_and_camera_inputs() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let instance_layout = ArrayType::of_known::<InstanceTransform>();

        assert_eq!(
            RenderInstanced3DMesh::TYPE_ID,
            "node.render_instanced_3d_mesh"
        );
        assert_eq!(RenderInstanced3DMesh::INPUTS.len(), 3);
        assert_eq!(RenderInstanced3DMesh::INPUTS[0].name, "vertices");
        assert_eq!(
            RenderInstanced3DMesh::INPUTS[0].ty,
            PortType::Array(mesh_layout)
        );
        assert_eq!(RenderInstanced3DMesh::INPUTS[1].name, "instances");
        assert_eq!(
            RenderInstanced3DMesh::INPUTS[1].ty,
            PortType::Array(instance_layout)
        );
        assert_eq!(RenderInstanced3DMesh::INPUTS[2].name, "camera");
        assert!(RenderInstanced3DMesh::INPUTS[2].required);
        assert_eq!(RenderInstanced3DMesh::INPUTS[2].ty, PortType::Camera);
        assert_eq!(RenderInstanced3DMesh::OUTPUTS.len(), 1);
        assert_eq!(RenderInstanced3DMesh::OUTPUTS[0].name, "color");
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = RenderInstanced3DMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.render_instanced_3d_mesh");
    }
}
