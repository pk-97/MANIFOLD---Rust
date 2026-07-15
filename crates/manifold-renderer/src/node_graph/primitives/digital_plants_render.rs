//! `node.digital_plants_render` — fused two-pass DigitalPlants
//! renderer: shadow pass (depth-only) into an internal shadow map,
//! then main pass (cel-shaded cubes with PCF shadow sampling).
//!
//! Bit-exact wraps of `generators/shaders/digital_plants_shadow.wgsl`
//! and `digital_plants_render.wgsl` via include_str. Cube vertex
//! data is hardcoded in both shaders (36 verts).
//!
//! Inputs: `Array<InstanceTransform>` driving instanced rendering.
//! Output: `Texture2D` color (Rgba16Float, depth-tested + cel-shaded
//! + PCF shadow).
//!
//! Internal state: the shadow map (Depth32Float), the dummy color
//! attachment for the shadow pass (Rgba16Float, never read), the
//! main pass's depth buffer, and a comparison sampler for PCF.
//!
//! Fused because the shadow output is intrinsically a depth texture
//! and the node_graph runtime today only allocates Rgba16Float
//! Texture2D outputs through the resource pool. A future
//! `Texture2DDepth` port type would let the shadow pass become its
//! own primitive — this fused version ships first.

use std::borrow::Cow;

use manifold_gpu::{
    GpuAddressMode, GpuBinding, GpuFilterMode, GpuLoadAction, GpuSamplerDesc, GpuTextureFormat,
};

use crate::generators::mesh_common::InstanceTransform;
use crate::generators::mesh_pipeline::{look_at_rh, mat4_mul, ortho_rh};
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const CUBE_VERTEX_COUNT: u32 = 36;
const SHADOW_MAP_SIZE: u32 = 2048;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RenderUniforms {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    light_pos: [f32; 4],
    light_color: [f32; 4],
    shadow_info: [f32; 4], // x: shadow_map_size
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowUniforms {
    light_view_proj: [[f32; 4]; 4],
}

crate::primitive! {
    name: DigitalPlantsRender,
    type_id: "node.digital_plants_render",
    purpose: "Fused two-pass DigitalPlants renderer: shadow pass (depth-only from light POV) into an internal shadow map, then main pass with instanced cel-shaded cubes + 5-tap PCF shadow sampling. Hardcoded 36-vert cube geometry (no Array<MeshVertex> input). Pair upstream with node.arrange_copies (or any procedural compute that produces InstanceTransforms — DigitalPlants's procedural compute is one such producer).",
    inputs: {
        instances: Array(InstanceTransform) required,
        camera: Camera required,
        instance_count: ScalarF32 optional,
        light_x: ScalarF32 optional,
        light_y: ScalarF32 optional,
        light_z: ScalarF32 optional,
        light_intensity: ScalarF32 optional,
    },
    outputs: {
        color: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("instance_count"),
            label: "Instance Count",
            ty: ParamType::Int,
            default: ParamValue::Float(160_000.0),
            range: Some((1.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("light_x"),
            label: "Light X",
            ty: ParamType::Float,
            default: ParamValue::Float(8.0),
            range: Some((-50.0, 50.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("light_y"),
            label: "Light Y",
            ty: ParamType::Float,
            default: ParamValue::Float(20.0),
            range: Some((-50.0, 50.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("light_z"),
            label: "Light Z",
            ty: ParamType::Float,
            default: ParamValue::Float(8.0),
            range: Some((-50.0, 50.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("light_intensity"),
            label: "Light Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Hardcoded 36-vertex cube geometry — no Array<MeshVertex> input. Pair upstream with node.arrange_copies (Grid/Ring/Spiral/Random layouts) to drive arbitrary cube fields, or use a procedural compute that emits InstanceTransform values (DigitalPlants's noise-driven plant-stalk generator is one). 5-tap PCF shadow sampling with hard-coded 0.003 depth bias matches the legacy DigitalPlants. Shadow map is 2048×2048 internal state.",
    examples: [],
    picker: { label: "Digital Plants Render", category: Atom },
    summary: "Renders a field of cubes lit with shadows, the core of the Digital Plants look. A fused renderer still to be decomposed.",
    category: Geometry3D,
    role: Filter,
    aliases: ["digital plants", "cubes", "shadows", "render"],
    boundary_reason: FusedBundle,
    extra_fields: {
        shadow_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        shadow_depth_stencil: Option<manifold_gpu::GpuDepthStencilState> = None,
        shadow_map: Option<manifold_gpu::GpuTexture> = None,
        shadow_color_dummy: Option<manifold_gpu::GpuTexture> = None,
        render_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        render_depth_stencil: Option<manifold_gpu::GpuDepthStencilState> = None,
        depth_texture: Option<manifold_gpu::GpuTexture> = None,
        depth_width: u32 = 0,
        depth_height: u32 = 0,
        shadow_sampler: Option<manifold_gpu::GpuSampler> = None,
    },
}

impl DigitalPlantsRender {
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
            label: "node.digital_plants_render depth",
            mip_levels: 1,
        }));
        self.depth_width = width;
        self.depth_height = height;
    }
}

impl Primitive for DigitalPlantsRender {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let instance_count = ctx.scalar_or_param("instance_count", 160_000.0).round().max(0.0) as u32;
        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);
        let light_x = ctx.scalar_or_param("light_x", 8.0);
        let light_y = ctx.scalar_or_param("light_y", 20.0);
        let light_z = ctx.scalar_or_param("light_z", 8.0);
        let light_intensity = ctx.scalar_or_param("light_intensity", 1.0);

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

        let item_size = std::mem::size_of::<InstanceTransform>() as u64;
        let inst_capacity = (instances.size / item_size) as u32;
        let inst_count = instance_count.min(inst_capacity);
        if inst_count == 0 {
            let gpu = ctx.gpu_encoder();
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            return;
        }

        // Camera setup
        let aspect = width as f32 / height as f32;
        let view_proj = cam.view_proj(aspect);

        // Light VP — orthographic from light POV
        let light_proj = ortho_rh(-30.0, 30.0, -30.0, 30.0, 0.1, 200.0);
        let light_view = look_at_rh([light_x, light_y, light_z], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let light_view_proj = mat4_mul(light_proj, light_view);

        let render_uniforms = RenderUniforms {
            view_proj,
            camera_pos: [cam.pos[0], cam.pos[1], cam.pos[2], 1.0],
            light_pos: [light_x, light_y, light_z, 1.0],
            light_color: [1.0, 1.0, 1.0, light_intensity],
            shadow_info: [SHADOW_MAP_SIZE as f32, 0.0, 0.0, 0.0],
        };

        let shadow_uniforms = ShadowUniforms { light_view_proj };

        let gpu = ctx.gpu_encoder();

        // Lazy-init all the pipelines and textures.
        if self.shadow_pipeline.is_none() {
            self.shadow_pipeline = Some(gpu.device.create_render_pipeline_depth(
                include_str!("../../generators/shaders/digital_plants_shadow.wgsl"),
                "vs_shadow",
                "fs_shadow",
                GpuTextureFormat::Rgba16Float,
                GpuTextureFormat::Depth32Float,
                None,
                1,
                "node.digital_plants_render shadow",
            ));
        }
        if self.shadow_depth_stencil.is_none() {
            self.shadow_depth_stencil = Some(gpu.device.create_depth_stencil_state(
                &manifold_gpu::GpuDepthStencilDesc {
                    compare: manifold_gpu::GpuCompareFunction::Less,
                    write_enabled: true,
                },
            ));
        }
        if self.shadow_map.is_none() {
            self.shadow_map = Some(gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: SHADOW_MAP_SIZE,
                height: SHADOW_MAP_SIZE,
                depth: 1,
                format: GpuTextureFormat::Depth32Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET
                    | manifold_gpu::GpuTextureUsage::SHADER_READ,
                label: "node.digital_plants_render shadow_map",
                mip_levels: 1,
            }));
        }
        if self.shadow_color_dummy.is_none() {
            self.shadow_color_dummy = Some(gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: SHADOW_MAP_SIZE,
                height: SHADOW_MAP_SIZE,
                depth: 1,
                format: GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET,
                label: "node.digital_plants_render shadow_color_dummy",
                mip_levels: 1,
            }));
        }
        if self.render_pipeline.is_none() {
            self.render_pipeline = Some(gpu.device.create_render_pipeline_depth(
                include_str!("../../generators/shaders/digital_plants_render.wgsl"),
                "vs_main",
                "fs_main",
                GpuTextureFormat::Rgba16Float,
                GpuTextureFormat::Depth32Float,
                None,
                1,
                "node.digital_plants_render main",
            ));
        }
        if self.render_depth_stencil.is_none() {
            self.render_depth_stencil = Some(gpu.device.create_depth_stencil_state(
                &manifold_gpu::GpuDepthStencilDesc {
                    compare: manifold_gpu::GpuCompareFunction::Less,
                    write_enabled: true,
                },
            ));
        }
        if self.shadow_sampler.is_none() {
            self.shadow_sampler = Some(gpu.device.create_sampler(&GpuSamplerDesc {
                min_filter: GpuFilterMode::Linear,
                mag_filter: GpuFilterMode::Linear,
                mip_filter: GpuFilterMode::Nearest,
                address_mode_u: GpuAddressMode::ClampToEdge,
                address_mode_v: GpuAddressMode::ClampToEdge,
                address_mode_w: GpuAddressMode::ClampToEdge,
                compare: Some(manifold_gpu::GpuCompareFunction::Less),
                ..Default::default()
            }));
        }
        self.ensure_depth_texture(gpu.device, width, height);

        let shadow_pipeline = self.shadow_pipeline.as_ref().expect("just inserted");
        let shadow_depth_stencil = self.shadow_depth_stencil.as_ref().expect("just inserted");
        let shadow_map = self.shadow_map.as_ref().expect("just inserted");
        let shadow_color_dummy = self.shadow_color_dummy.as_ref().expect("just inserted");
        let render_pipeline = self.render_pipeline.as_ref().expect("just inserted");
        let render_depth_stencil = self.render_depth_stencil.as_ref().expect("just inserted");
        let depth_tex = self.depth_texture.as_ref().expect("just inserted");
        let shadow_sampler = self.shadow_sampler.as_ref().expect("just inserted");

        // Shadow pass: render cubes depth-only into shadow_map.
        gpu.native_enc.draw_instanced_depth(
            shadow_pipeline,
            shadow_color_dummy,
            shadow_map,
            shadow_depth_stencil,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&shadow_uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: instances,
                    offset: 0,
                },
            ],
            CUBE_VERTEX_COUNT,
            inst_count,
            GpuLoadAction::Clear,
            "node.digital_plants_render.shadow",
        );

        // Main pass: render with PCF shadow sampling.
        gpu.native_enc.draw_instanced_depth(
            render_pipeline,
            target,
            depth_tex,
            render_depth_stencil,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&render_uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: instances,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 2,
                    data: bytemuck::bytes_of(&shadow_uniforms),
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: shadow_map,
                },
                GpuBinding::Sampler {
                    binding: 4,
                    sampler: shadow_sampler,
                },
            ],
            CUBE_VERTEX_COUNT,
            inst_count,
            GpuLoadAction::Clear,
            "node.digital_plants_render.main",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn digital_plants_render_declares_instance_camera_in_and_color_out() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let layout = ArrayType::of_known::<InstanceTransform>();
        assert_eq!(DigitalPlantsRender::TYPE_ID, "node.digital_plants_render");

        let inst_in = DigitalPlantsRender::INPUTS
            .iter()
            .find(|p| p.name == "instances")
            .expect("instances input must exist");
        assert!(inst_in.required);
        assert_eq!(inst_in.ty, PortType::Array(layout));

        let cam_in = DigitalPlantsRender::INPUTS
            .iter()
            .find(|p| p.name == "camera")
            .expect("camera input must exist");
        assert!(cam_in.required);
        assert_eq!(cam_in.ty, PortType::Camera);

        // Light params stay as port-shadow ScalarF32 — the JSON preset can drive
        // them from in-graph math if needed.
        for name in [
            "instance_count",
            "light_x",
            "light_y",
            "light_z",
            "light_intensity",
        ] {
            let port = DigitalPlantsRender::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} is port-shadow, must be optional");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert_eq!(DigitalPlantsRender::OUTPUTS.len(), 1);
        assert_eq!(DigitalPlantsRender::OUTPUTS[0].name, "color");
        assert_eq!(
            DigitalPlantsRender::OUTPUTS[0].ty,
            PortType::Texture2D
        );
    }

    #[test]
    fn digital_plants_render_has_light_params() {
        let names: Vec<&str> = DigitalPlantsRender::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        for required in &[
            "instance_count",
            "light_x",
            "light_intensity",
        ] {
            assert!(names.contains(required), "missing param {}", required);
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = DigitalPlantsRender::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.digital_plants_render");
    }
}
