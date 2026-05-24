//! `node.render_3d_mesh_pbr_ibl` — render an `Array<MeshVertex>` as a
//! depth-tested triangle list shaded with Cook-Torrance microfacet BRDF
//! plus image-based lighting from an equirectangular environment map.
//!
//! Parallel render-primitive entry to `node.render_3d_mesh` (Lambert +
//! ambient) and `node.render_instanced_3d_mesh` (instanced Lambert) —
//! same shape, PBR shading model. Takes a packed material texture
//! (R = height, G = metallic variation, B = edge / roughness) used for
//! per-pixel normal computation (finite differences on R) and material
//! modulation (G and B).
//!
//! For MetallicGlass-shaped graphs:
//!   generate_grid_mesh → displace_mesh(height=material.r * displacement)
//!     → triangulate_grid → render_3d_mesh_pbr_ibl(material, env_map, ...)
//!
//! Math (shared with the `node.cook_torrance_specular` and
//! `node.equirect_envmap_sample` atoms via `shaders/pbr_brdf.wgsl`).

use manifold_gpu::{GpuBinding, GpuLoadAction};

use crate::generators::mesh_common::MeshVertex;
use crate::generators::mesh_pipeline::{look_at_rh, mat4_mul, perspective_rh};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PbrRenderUniforms {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    light_pos: [f32; 4],     // xyz: world pos, w: intensity
    light_color: [f32; 4],   // rgb: light tint, a: unused (intensity packed in light_pos.w)
    material: [f32; 4],      // metallic, roughness, displacement, edge_roughness_mul
    base_color: [f32; 4],
    grid_info: [f32; 4],     // grid_size, texel_inv, aspect, unused
}

const PBR_BRDF: &str = include_str!("shaders/pbr_brdf.wgsl");

crate::primitive! {
    name: Render3DMeshPbrIbl,
    type_id: "node.render_3d_mesh_pbr_ibl",
    purpose: "Render an Array<MeshVertex> triangle list with Cook-Torrance PBR + image-based lighting from an equirectangular environment map. Takes a packed material texture (R = height for per-pixel normal computation, G = metallic variation, B = edge → roughness modulation) and the env map. Camera + light + material params parallel `node.render_3d_mesh` (Lambert sibling).",
    inputs: {
        vertices: Array(MeshVertex) required,
        material: Texture2D required,
        env_map: Texture2D required,
        camera_distance: ScalarF32 optional,
        camera_orbit: ScalarF32 optional,
        camera_tilt: ScalarF32 optional,
        camera_fov: ScalarF32 optional,
        look_y: ScalarF32 optional,
        light_x: ScalarF32 optional,
        light_y: ScalarF32 optional,
        light_z: ScalarF32 optional,
        light_intensity: ScalarF32 optional,
        metallic: ScalarF32 optional,
        roughness: ScalarF32 optional,
        displacement: ScalarF32 optional,
        edge_roughness_mul: ScalarF32 optional,
    },
    outputs: {
        color: Texture2D,
    },
    params: [
        ParamDef {
            name: "grid_size",
            label: "Grid Size",
            ty: ParamType::Int,
            default: ParamValue::Float(300.0),
            range: Some((4.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "camera_distance",
            label: "Camera Distance",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.1, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "camera_orbit",
            label: "Camera Orbit",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
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
            name: "light_x",
            label: "Light X",
            ty: ParamType::Float,
            default: ParamValue::Float(-2.0),
            range: Some((-20.0, 20.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "light_y",
            label: "Light Y",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((-20.0, 20.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "light_z",
            label: "Light Z",
            ty: ParamType::Float,
            default: ParamValue::Float(5.0),
            range: Some((-20.0, 20.0)),
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
            range: Some((0.01, 1.0)),
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
            name: "edge_roughness_mul",
            label: "Edge Roughness Mul",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((1.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "light_color",
            label: "Light Color",
            ty: ParamType::Color,
            default: ParamValue::Color([1.0, 1.0, 1.0, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: "base_color",
            label: "Base Color",
            ty: ParamType::Color,
            default: ParamValue::Color([0.8, 0.8, 0.82, 1.0]),
            range: None,
            enum_values: &[],
        },
    ],
    composition_notes: "Vertex count must be a multiple of 3 (trailing partial triangle is skipped). `material.r` is sampled per-pixel for finite-difference normals (full-resolution reflections regardless of grid density) AND for per-vertex Y-displacement via the upstream `node.displace_mesh`. The `grid_size` param tells the fragment shader the source grid topology so it knows what 1-texel offset corresponds to in world space. Output is Rgba16Float, Reinhard-tone-mapped in-shader (color / (color + 1)).",
    examples: [],
    picker: { label: "Render 3D Mesh PBR-IBL", category: Atom },
    extra_fields: {
        render_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        depth_stencil: Option<manifold_gpu::GpuDepthStencilState> = None,
        material_sampler: Option<manifold_gpu::GpuSampler> = None,
        env_sampler: Option<manifold_gpu::GpuSampler> = None,
        depth_texture: Option<manifold_gpu::GpuTexture> = None,
        depth_width: u32 = 0,
        depth_height: u32 = 0,
    },
}

impl Render3DMeshPbrIbl {
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
            label: "node.render_3d_mesh_pbr_ibl depth",
            mip_levels: 1,
        }));
        self.depth_width = width;
        self.depth_height = height;
    }
}

impl Primitive for Render3DMeshPbrIbl {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read_int = |name: &str, default: f32| -> f32 {
            match ctx.params.get(name) {
                Some(ParamValue::Float(f)) => *f,
                _ => default,
            }
        };
        let read = |name: &str, default: f32| -> f32 {
            match ctx.inputs.scalar(name) {
                Some(ParamValue::Float(f)) => f,
                _ => match ctx.params.get(name) {
                    Some(ParamValue::Float(f)) => *f,
                    _ => default,
                },
            }
        };
        let grid_size = read_int("grid_size", 300.0).round().max(4.0) as u32;
        let camera_distance = read("camera_distance", 2.0).max(0.01);
        let camera_orbit = read("camera_orbit", 0.0);
        let camera_tilt = read("camera_tilt", 0.3);
        let camera_fov = read("camera_fov", 0.95).max(0.05);
        let look_y = read("look_y", 0.0);
        let light_x = read("light_x", -2.0);
        let light_y = read("light_y", 2.0);
        let light_z = read("light_z", 5.0);
        let light_intensity = read("light_intensity", 3.5);
        let metallic = read("metallic", 1.0);
        let roughness = read("roughness", 0.05);
        let displacement = read("displacement", 0.2);
        let edge_roughness_mul = read("edge_roughness_mul", 4.0);
        let light_color = match ctx.params.get("light_color") {
            Some(ParamValue::Color(c)) => *c,
            _ => [1.0, 1.0, 1.0, 1.0],
        };
        let base_color = match ctx.params.get("base_color") {
            Some(ParamValue::Color(c)) => *c,
            _ => [0.8, 0.8, 0.82, 1.0],
        };

        let Some(vertices) = ctx.inputs.array("vertices") else {
            return;
        };
        let Some(material) = ctx.inputs.texture_2d("material") else {
            return;
        };
        let Some(env_map) = ctx.inputs.texture_2d("env_map") else {
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
        if vertex_count == 0 {
            let gpu = ctx.gpu_encoder();
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            return;
        }

        let aspect = width as f32 / height as f32;
        let proj = perspective_rh(camera_fov, aspect, 0.01, 50.0);
        let target_pos = [0.0_f32, look_y, 0.0];
        let eye = [
            camera_distance * camera_tilt.cos() * camera_orbit.sin(),
            camera_distance * camera_tilt.sin() + look_y,
            camera_distance * camera_tilt.cos() * camera_orbit.cos(),
        ];
        let view = look_at_rh(eye, target_pos, [0.0, 1.0, 0.0]);
        let view_proj = mat4_mul(proj, view);

        let material_w = material.width.max(1) as f32;

        let uniforms = PbrRenderUniforms {
            view_proj,
            camera_pos: [eye[0], eye[1], eye[2], 1.0],
            light_pos: [light_x, light_y, light_z, light_intensity],
            light_color: [light_color[0], light_color[1], light_color[2], 0.0],
            material: [metallic, roughness, displacement, edge_roughness_mul],
            base_color: [base_color[0], base_color[1], base_color[2], 1.0],
            grid_info: [grid_size as f32, 1.0 / material_w, aspect, 0.0],
        };

        let gpu = ctx.gpu_encoder();

        if self.render_pipeline.is_none() {
            let source = format!(
                "{}\n{}",
                PBR_BRDF,
                include_str!("shaders/render_3d_mesh_pbr_ibl.wgsl"),
            );
            self.render_pipeline = Some(gpu.device.create_render_pipeline_depth(
                &source,
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                manifold_gpu::GpuTextureFormat::Depth32Float,
                None,
                1,
                "node.render_3d_mesh_pbr_ibl",
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
        if self.material_sampler.is_none() {
            self.material_sampler = Some(
                gpu.device
                    .create_sampler(&manifold_gpu::GpuSamplerDesc::default()),
            );
        }
        if self.env_sampler.is_none() {
            self.env_sampler = Some(
                gpu.device
                    .create_sampler(&manifold_gpu::GpuSamplerDesc::default()),
            );
        }
        self.ensure_depth_texture(gpu.device, width, height);

        let pipeline = self.render_pipeline.as_ref().expect("just inserted");
        let depth_stencil = self.depth_stencil.as_ref().expect("just inserted");
        let depth_tex = self.depth_texture.as_ref().expect("just inserted");
        let material_sampler = self.material_sampler.as_ref().expect("just inserted");
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
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: vertices,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: material,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler: material_sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: env_map,
                },
                GpuBinding::Sampler {
                    binding: 5,
                    sampler: env_sampler,
                },
            ],
            vertex_count,
            1,
            GpuLoadAction::Clear,
            "node.render_3d_mesh_pbr_ibl",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_vertices_material_envmap_inputs() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        assert_eq!(Render3DMeshPbrIbl::TYPE_ID, "node.render_3d_mesh_pbr_ibl");

        // Three required Array/Texture inputs come first.
        assert_eq!(Render3DMeshPbrIbl::INPUTS[0].name, "vertices");
        assert!(Render3DMeshPbrIbl::INPUTS[0].required);
        assert_eq!(
            Render3DMeshPbrIbl::INPUTS[0].ty,
            PortType::Array(mesh_layout)
        );
        assert_eq!(Render3DMeshPbrIbl::INPUTS[1].name, "material");
        assert!(Render3DMeshPbrIbl::INPUTS[1].required);
        assert_eq!(Render3DMeshPbrIbl::INPUTS[1].ty, PortType::Texture2D);
        assert_eq!(Render3DMeshPbrIbl::INPUTS[2].name, "env_map");
        assert!(Render3DMeshPbrIbl::INPUTS[2].required);
        assert_eq!(Render3DMeshPbrIbl::INPUTS[2].ty, PortType::Texture2D);

        // Remaining inputs are optional port-shadows over scalar params.
        for input in &Render3DMeshPbrIbl::INPUTS[3..] {
            assert!(
                !input.required,
                "scalar port-shadow `{}` should be optional",
                input.name
            );
            assert_eq!(input.ty, PortType::Scalar(crate::node_graph::ports::ScalarType::F32));
        }

        assert_eq!(Render3DMeshPbrIbl::OUTPUTS.len(), 1);
        assert_eq!(Render3DMeshPbrIbl::OUTPUTS[0].name, "color");
    }

    #[test]
    fn registers_as_atom() {
        let prim = Render3DMeshPbrIbl::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.render_3d_mesh_pbr_ibl");
    }
}
