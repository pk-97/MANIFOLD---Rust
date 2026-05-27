//! `node.render_instanced_3d_mesh` — bundled instanced 3D mesh
//! renderer. Sibling to [`render_3d_mesh`](super::render_3d_mesh):
//! same per-MaterialKind dispatch, same Material/Light/envmap input
//! shape, but applies a per-instance `pos/scale/Euler-rotation`
//! transform from an `Array<InstanceTransform>` to each instance's
//! vertices.
//!
//! Per-kind conditional requirements + magenta-fallback for missing
//! inputs match `render_3d_mesh`'s contract exactly. See the doc on
//! that primitive for the shared design rationale.

use ahash::AHashMap;
use manifold_gpu::{GpuBinding, GpuLoadAction};

use crate::generators::mesh_common::{InstanceTransform, MeshVertex};
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::{ConditionalRequirement, EffectNodeContext};
use crate::node_graph::material::{Material, MaterialKind};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InstancedMaterialUniforms {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    light_dir: [f32; 4],
    light_color: [f32; 4],
    base_color: [f32; 4],
    emission: [f32; 4],
    pbr_metallic_roughness: [f32; 4],
    specular: [f32; 4],
    cel_params: [f32; 4],
}

const CONDITIONAL_RULES: &[ConditionalRequirement] = &[
    ConditionalRequirement {
        on_material_kind: MaterialKind::Phong,
        required_inputs: &["light"],
    },
    ConditionalRequirement {
        on_material_kind: MaterialKind::Pbr,
        required_inputs: &["light", "envmap"],
    },
    ConditionalRequirement {
        on_material_kind: MaterialKind::Cel,
        required_inputs: &["light"],
    },
];

crate::primitive! {
    name: RenderInstanced3DMesh,
    type_id: "node.render_instanced_3d_mesh",
    purpose: "Bundled instanced 3D mesh renderer. Draws N copies of an Array<MeshVertex> base mesh, each transformed by an Array<InstanceTransform> entry. Takes a Camera + Material + optional Light + optional envmap, picks the matching per-MaterialKind fragment shader (Unlit / Phong / PBR / Cel), and emits a shaded `color` Texture2D. Pair with node.generate_instance_transforms to drive NestedCubes / DigitalPlants graphs. Per-kind requirements mirror render_3d_mesh: Unlit needs no light; Phong / Cel need light; PBR needs light + envmap.",
    inputs: {
        vertices: Array(MeshVertex) required,
        instances: Array(InstanceTransform) required,
        camera: Camera required,
        material: Material required,
        light: Light optional,
        envmap: Texture2D optional,
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
    ],
    composition_notes: "Vertex count must be a multiple of 3 (trailing partial triangle truncated). instance_count is clamped to the wired instance buffer's capacity. The instance Array dictates how many copies are drawn. Wire a `node.{unlit,phong,pbr,cel}_material` into `material` to pick the shading model; pair with `node.light` (required for Phong/PBR/Cel) and `node.bake_equirect_envmap` (required for PBR).",
    examples: [],
    picker: { label: "Render Instanced 3D Mesh", category: Atom },
    extra_fields: {
        pipelines: AHashMap<MaterialKind, manifold_gpu::GpuRenderPipeline> = AHashMap::new(),
        depth_stencil: Option<manifold_gpu::GpuDepthStencilState> = None,
        depth_texture: Option<manifold_gpu::GpuTexture> = None,
        depth_width: u32 = 0,
        depth_height: u32 = 0,
        dummy_envmap: Option<manifold_gpu::GpuTexture> = None,
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

    fn ensure_sampler(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.sampler.is_none() {
            self.sampler = Some(device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                mag_filter: manifold_gpu::GpuFilterMode::Linear,
                min_filter: manifold_gpu::GpuFilterMode::Linear,
                mip_filter: manifold_gpu::GpuFilterMode::Linear,
                address_mode_u: manifold_gpu::GpuAddressMode::Repeat,
                address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
                address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
                compare: None,
            }));
        }
    }

    fn ensure_dummy_envmap(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.dummy_envmap.is_none() {
            self.dummy_envmap = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ,
                label: "node.render_instanced_3d_mesh dummy envmap",
                mip_levels: 1,
            }));
        }
    }

    fn pipeline_for(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        kind: MaterialKind,
    ) -> &manifold_gpu::GpuRenderPipeline {
        let fs_entry = match kind {
            MaterialKind::Unlit => "fs_unlit",
            MaterialKind::Phong => "fs_phong",
            MaterialKind::Pbr => "fs_pbr",
            MaterialKind::Cel => "fs_cel",
        };
        self.pipelines.entry(kind).or_insert_with(|| {
            device.create_render_pipeline_depth(
                include_str!("shaders/render_instanced_3d_mesh.wgsl"),
                "vs_main",
                fs_entry,
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                manifold_gpu::GpuTextureFormat::Depth32Float,
                None,
                1,
                "node.render_instanced_3d_mesh",
            )
        })
    }
}

fn build_uniforms(
    view_proj: [[f32; 4]; 4],
    cam: &Camera,
    light_dir: [f32; 3],
    light_color: [f32; 4],
    material: &Material,
) -> InstancedMaterialUniforms {
    InstancedMaterialUniforms {
        view_proj,
        camera_pos: [cam.pos[0], cam.pos[1], cam.pos[2], 1.0],
        light_dir: [light_dir[0], light_dir[1], light_dir[2], 1.0],
        light_color,
        base_color: material.base_color,
        emission: material.emission,
        pbr_metallic_roughness: [material.metallic, material.roughness, 0.0, 0.0],
        specular: [
            material.specular_color[0],
            material.specular_color[1],
            material.specular_color[2],
            material.specular_power,
        ],
        cel_params: [
            material.cel_bands as f32,
            material.band_low,
            material.band_high,
            0.0,
        ],
    }
}

impl Primitive for RenderInstanced3DMesh {
    fn conditional_requirements(&self) -> &'static [ConditionalRequirement] {
        CONDITIONAL_RULES
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let instance_count_param = match ctx.params.get("instance_count") {
            Some(ParamValue::Float(n)) => n.round().max(0_f32) as u32,
            _ => 64,
        };
        let cam = ctx
            .inputs
            .camera("camera")
            .unwrap_or_else(Camera::default_perspective);

        let material = match ctx.inputs.material("material") {
            Some(m) => m,
            None => {
                ctx.error("missing required `material` input; renderer fell back to magenta clear");
                if let Some(target) = ctx.outputs.texture_2d("color") {
                    let gpu = ctx.gpu_encoder();
                    gpu.native_enc.clear_texture(target, 1.0, 0.0, 1.0, 1.0);
                }
                return;
            }
        };

        let needs_light = material.requires_light();
        let needs_envmap = material.requires_envmap();
        let light_wired = ctx.inputs.light("light");
        let envmap_wired = ctx.inputs.texture_2d("envmap");

        if needs_light && light_wired.is_none() {
            ctx.error(format!(
                "{:?} material requires `light` input but it is unwired; renderer fell back to magenta",
                material.kind
            ));
            if let Some(target) = ctx.outputs.texture_2d("color") {
                let gpu = ctx.gpu_encoder();
                gpu.native_enc.clear_texture(target, 1.0, 0.0, 1.0, 1.0);
            }
            return;
        }
        if needs_envmap && envmap_wired.is_none() {
            ctx.error(format!(
                "{:?} material requires `envmap` input but it is unwired; renderer fell back to magenta",
                material.kind
            ));
            if let Some(target) = ctx.outputs.texture_2d("color") {
                let gpu = ctx.gpu_encoder();
                gpu.native_enc.clear_texture(target, 1.0, 0.0, 1.0, 1.0);
            }
            return;
        }

        let (light_dir, light_color) = match light_wired {
            Some(l) => (
                [-l.dir[0], -l.dir[1], -l.dir[2]],
                [l.color[0], l.color[1], l.color[2], material.ambient],
            ),
            None => (
                [0.3, 0.7, 0.6],
                [1.0, 1.0, 1.0, material.ambient],
            ),
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
        let instance_count = instance_count_param.min(instance_capacity);
        if vertex_count == 0 || instance_count == 0 {
            let gpu = ctx.gpu_encoder();
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            return;
        }

        let aspect = width as f32 / height as f32;
        let view_proj = cam.view_proj(aspect);
        let uniforms = build_uniforms(view_proj, &cam, light_dir, light_color, &material);

        let gpu = ctx.gpu_encoder();
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
        self.ensure_sampler(gpu.device);
        self.ensure_dummy_envmap(gpu.device);
        let pipeline = self.pipeline_for(gpu.device, material.kind).clone();

        let depth_stencil = self.depth_stencil.as_ref().expect("just inserted");
        let depth_tex = self.depth_texture.as_ref().expect("just inserted");
        let sampler = self.sampler.as_ref().expect("just inserted");
        let dummy_envmap = self.dummy_envmap.as_ref().expect("just inserted");
        let envmap_texture = envmap_wired.unwrap_or(dummy_envmap);

        gpu.native_enc.draw_instanced_depth(
            &pipeline,
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
                GpuBinding::Texture {
                    binding: 3,
                    texture: envmap_texture,
                },
                GpuBinding::Sampler {
                    binding: 4,
                    sampler,
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
    fn render_instanced_declares_material_required_inputs() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let instance_layout = ArrayType::of_known::<InstanceTransform>();

        assert_eq!(
            RenderInstanced3DMesh::TYPE_ID,
            "node.render_instanced_3d_mesh"
        );
        let by_name = |n: &str| {
            RenderInstanced3DMesh::INPUTS
                .iter()
                .find(|p| p.name == n)
                .unwrap_or_else(|| panic!("missing input {n}"))
        };
        let vertices = by_name("vertices");
        assert!(vertices.required);
        assert_eq!(vertices.ty, PortType::Array(mesh_layout));
        let instances = by_name("instances");
        assert!(instances.required);
        assert_eq!(instances.ty, PortType::Array(instance_layout));
        let camera = by_name("camera");
        assert!(camera.required);
        assert_eq!(camera.ty, PortType::Camera);
        let material = by_name("material");
        assert!(material.required, "material must be REQUIRED");
        assert_eq!(material.ty, PortType::Material);
        let light = by_name("light");
        assert!(!light.required);
        assert_eq!(light.ty, PortType::Light);
        let envmap = by_name("envmap");
        assert!(!envmap.required);
        assert_eq!(envmap.ty, PortType::Texture2D);
        assert_eq!(RenderInstanced3DMesh::OUTPUTS.len(), 1);
        assert_eq!(RenderInstanced3DMesh::OUTPUTS[0].name, "color");
    }

    #[test]
    fn render_instanced_3d_mesh_declares_conditional_requirements() {
        let prim = RenderInstanced3DMesh::new();
        let node: &dyn EffectNode = &prim;
        let rules = node.conditional_requirements();
        assert_eq!(rules.len(), 3);
        let by_kind = |k: MaterialKind| {
            rules
                .iter()
                .find(|r| r.on_material_kind == k)
                .unwrap_or_else(|| panic!("missing rule for {k:?}"))
        };
        assert_eq!(by_kind(MaterialKind::Phong).required_inputs, &["light"]);
        assert_eq!(by_kind(MaterialKind::Pbr).required_inputs, &["light", "envmap"]);
        assert_eq!(by_kind(MaterialKind::Cel).required_inputs, &["light"]);
    }

    #[test]
    fn render_instanced_has_only_instance_count_param() {
        // Scattered light/colour scalars deleted in the Material migration.
        let names: Vec<&str> = RenderInstanced3DMesh::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["instance_count"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = RenderInstanced3DMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.render_instanced_3d_mesh");
    }
}
