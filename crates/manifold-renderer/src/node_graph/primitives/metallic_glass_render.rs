//! `node.metallic_glass_render` — fused MetallicGlass render pass.
//! Bit-exact wrap of `generators/shaders/metallic_glass_render.wgsl`
//! via include_str.
//!
//! Procedurally generates an NxN grid (no Array<MeshVertex> input),
//! displaces Y per-vertex from a height texture, computes per-pixel
//! normals via finite differences on the height texture in the
//! fragment shader, and renders with Cook-Torrance BRDF + HDR IBL
//! from an envmap. Depth-tested.
//!
//! Fused for MetallicGlass parity. The decomposition into grid
//! producer + displacer + PBR renderer would not be bit-exact
//! because the per-pixel-normal-from-height trick requires the
//! height texture at fragment time, which a generic mesh renderer
//! doesn't have access to.

use manifold_gpu::{GpuBinding, GpuLoadAction, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RenderUniforms {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    light_pos: [f32; 4],
    light_color: [f32; 4],
    material: [f32; 4],   // x=metallic, y=roughness, z=displacement, w=unused
    grid_info: [f32; 4],  // x=grid_size, y=texel_size, z=aspect, w=unused
}

crate::primitive! {
    name: MetallicGlassRender,
    type_id: "node.metallic_glass_render",
    purpose: "Fused MetallicGlass renderer: procedural NxN grid, height-texture displacement in vertex shader, per-pixel normals from height-texture finite differences, Cook-Torrance BRDF, HDR environment IBL. Depth-tested, no inputs from the buffer-port primitives (grid is procedural). Inputs: height texture + envmap (both Texture2D). Fused for parity — the per-pixel-from-height normal trick can't be cleanly factored into generic primitives.",
    inputs: {
        height: Texture2D required,
        envmap: Texture2D required,
    },
    outputs: {
        color: Texture2D,
    },
    params: [
        ParamDef {
            name: "grid_size",
            label: "Grid Size",
            ty: ParamType::Int,
            default: ParamValue::Int(300),
            range: Some((4.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "metallic",
            label: "Metallic",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "roughness",
            label: "Roughness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.05),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "displacement",
            label: "Displacement",
            ty: ParamType::Float,
            default: ParamValue::Float(0.2),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "camera_distance",
            label: "Camera Distance",
            ty: ParamType::Float,
            default: ParamValue::Float(2.5),
            range: Some((0.1, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "camera_orbit",
            label: "Camera Orbit",
            ty: ParamType::Float,
            default: ParamValue::Float(0.7),
            range: Some((-6.28318, 6.28318)),
            enum_values: &[],
        },
        ParamDef {
            name: "camera_tilt",
            label: "Camera Tilt",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((-1.5, 1.5)),
            enum_values: &[],
        },
        ParamDef {
            name: "camera_fov",
            label: "Camera FOV",
            ty: ParamType::Float,
            default: ParamValue::Float(0.95),
            range: Some((0.1, 2.5)),
            enum_values: &[],
        },
        ParamDef {
            name: "look_y",
            label: "Look Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "light_intensity",
            label: "Light Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(3.5),
            range: Some((0.0, 20.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "grid_size = 300 produces (299×299×6) ≈ 538k vertices, the legacy MetallicGlass default. height texture's R channel drives Y displacement; the same height texture also drives per-pixel normals via finite differences (this is what gives MetallicGlass its high-quality reflections at low geometry density). envmap should be a 512×256 equirectangular HDR map — pair with node.metallic_glass_envmap upstream.",
    examples: [],
    picker: { label: "Metallic Glass Render", category: Atom },
    extra_fields: {
        render_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        depth_stencil: Option<manifold_gpu::GpuDepthStencilState> = None,
        depth_texture: Option<manifold_gpu::GpuTexture> = None,
        depth_width: u32 = 0,
        depth_height: u32 = 0,
        env_sampler: Option<manifold_gpu::GpuSampler> = None,
    },
}

impl MetallicGlassRender {
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
            label: "node.metallic_glass_render depth",
            mip_levels: 1,
        }));
        self.depth_width = width;
        self.depth_height = height;
    }
}

impl Primitive for MetallicGlassRender {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let grid_size = match ctx.params.get("grid_size") {
            Some(ParamValue::Int(n)) => (*n).max(4) as u32,
            _ => 300,
        };
        let metallic = match ctx.params.get("metallic") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let roughness = match ctx.params.get("roughness") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.05,
        };
        let displacement = match ctx.params.get("displacement") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.2,
        };
        let camera_distance = match ctx.params.get("camera_distance") {
            Some(ParamValue::Float(f)) => *f,
            _ => 2.5,
        };
        let camera_orbit = match ctx.params.get("camera_orbit") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.7,
        };
        let camera_tilt = match ctx.params.get("camera_tilt") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.3,
        };
        let camera_fov = match ctx.params.get("camera_fov") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.95,
        };
        let look_y = match ctx.params.get("look_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let light_intensity = match ctx.params.get("light_intensity") {
            Some(ParamValue::Float(f)) => *f,
            _ => 3.5,
        };

        let Some(height) = ctx.inputs.texture_2d("height") else {
            return;
        };
        let Some(envmap) = ctx.inputs.texture_2d("envmap") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("color") else {
            return;
        };
        let width = target.width;
        let render_height = target.height;
        if width == 0 || render_height == 0 {
            return;
        }

        let aspect = width as f32 / render_height as f32;

        let proj = crate::generators::mesh_pipeline::perspective_rh(camera_fov, aspect, 0.05, 200.0);
        let eye = [
            camera_distance * camera_orbit.cos() * camera_tilt.cos(),
            camera_distance * camera_tilt.sin() + look_y,
            camera_distance * camera_orbit.sin() * camera_tilt.cos(),
        ];
        let view = crate::generators::mesh_pipeline::look_at_rh(
            eye,
            [0.0, look_y, 0.0],
            [0.0, 1.0, 0.0],
        );
        let view_proj = crate::generators::mesh_pipeline::mat4_mul(proj, view);

        let texel_size = 1.0 / height.width as f32;
        let uniforms = RenderUniforms {
            view_proj,
            camera_pos: [eye[0], eye[1], eye[2], 1.0],
            light_pos: [-2.0, 2.0, 5.0, 1.0],
            light_color: [1.0, 1.0, 1.0, light_intensity],
            material: [metallic, roughness, displacement, 0.0],
            grid_info: [grid_size as f32, texel_size, aspect, 0.0],
        };

        let quads = (grid_size - 1) as u32;
        let vertex_count = quads * quads * 6;

        let gpu = ctx.gpu_encoder();
        if self.render_pipeline.is_none() {
            self.render_pipeline = Some(gpu.device.create_render_pipeline_depth(
                include_str!("../../generators/shaders/metallic_glass_render.wgsl"),
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                manifold_gpu::GpuTextureFormat::Depth32Float,
                None,
                1,
                "node.metallic_glass_render",
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
        if self.sampler.is_none() {
            self.sampler = Some(gpu.device.create_sampler(&GpuSamplerDesc::default()));
        }
        if self.env_sampler.is_none() {
            self.env_sampler = Some(gpu.device.create_sampler(&GpuSamplerDesc::default()));
        }
        self.ensure_depth_texture(gpu.device, width, render_height);

        let pipeline = self.render_pipeline.as_ref().expect("just inserted");
        let depth_stencil = self.depth_stencil.as_ref().expect("just inserted");
        let depth_tex = self.depth_texture.as_ref().expect("just inserted");
        let height_sampler = self.sampler.as_ref().expect("just inserted");
        let env_sampler = self.env_sampler.as_ref().expect("just inserted");

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
                GpuBinding::Texture {
                    binding: 1,
                    texture: height,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler: height_sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: envmap,
                },
                GpuBinding::Sampler {
                    binding: 4,
                    sampler: env_sampler,
                },
            ],
            vertex_count,
            1,
            GpuLoadAction::Clear,
            "node.metallic_glass_render",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn metallic_glass_render_declares_height_envmap_inputs_and_color_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(MetallicGlassRender::TYPE_ID, "node.metallic_glass_render");
        assert_eq!(MetallicGlassRender::INPUTS.len(), 2);
        assert_eq!(MetallicGlassRender::INPUTS[0].name, "height");
        assert_eq!(MetallicGlassRender::INPUTS[0].ty, PortType::Texture2D);
        assert!(MetallicGlassRender::INPUTS[0].required);
        assert_eq!(MetallicGlassRender::INPUTS[1].name, "envmap");
        assert_eq!(MetallicGlassRender::INPUTS[1].ty, PortType::Texture2D);
        assert_eq!(MetallicGlassRender::OUTPUTS.len(), 1);
        assert_eq!(MetallicGlassRender::OUTPUTS[0].name, "color");
        assert_eq!(MetallicGlassRender::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn metallic_glass_render_has_material_and_camera_params() {
        let names: Vec<&str> = MetallicGlassRender::PARAMS.iter().map(|p| p.name).collect();
        assert!(names.contains(&"grid_size"));
        assert!(names.contains(&"metallic"));
        assert!(names.contains(&"roughness"));
        assert!(names.contains(&"camera_distance"));
        assert!(names.contains(&"camera_fov"));
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = MetallicGlassRender::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.metallic_glass_render");
    }
}
