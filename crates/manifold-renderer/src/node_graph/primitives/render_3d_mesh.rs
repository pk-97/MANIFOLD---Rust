//! `node.render_mesh` — single-mesh adapter over the shared scene material
//! renderer, with independent world-position and world-normal G-buffer passes.
//!
//! Material system M4: the renderer takes a REQUIRED `material: Material`
//! input + an optional `light: Light` + an optional `envmap: Texture2D`.
//! Per-kind requirements are encoded in
//! [`conditional_requirements`](Primitive::conditional_requirements) —
//! the validator checks them at preset-load when the material source is
//! statically resolvable; at runtime the renderer emits a magenta clear
//! plus `ctx.error(...)` for the missing-input case (per the
//! "no silent fallbacks" rule).
//!
//! State held by the primitive instance (via `extra_fields`):
//! - G-buffer pipelines (world_pos, world_normal) — preserved unchanged
//! - depth-stencil state, depth texture resized to current output dims
//! - dummy 1×1 texture + sampler used by the G-buffer binding layout

use manifold_gpu::{GpuBinding, GpuLoadAction};

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::{ConditionalRequirement, EffectNodeContext};
use crate::node_graph::material::MaterialKind;
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GBufferUniforms {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    light_dir: [f32; 4],
    light_color: [f32; 4],
    base_color: [f32; 4],
    emission: [f32; 4],
    pbr_metallic_roughness: [f32; 4],
    specular: [f32; 4],
    cel_params: [f32; 4],
    /// `(use_normal_map, use_roughness_map, use_base_color_map,
    /// use_metallic_map)` presence flags for the per-pixel surface
    /// texture sampling. 1.0 = sample at per-fragment mesh UV; 0.0 = use
    /// the material's scalar value.
    texture_flags: [f32; 4],
    /// `(alpha_mode, alpha_cutoff, 0, 0)`. `alpha_mode` is `1.0` for
    /// [`crate::node_graph::material::AlphaMode::Mask`] (cutout via
    /// `discard`), `0.0` for Opaque. `alpha_cutoff` is the discard
    /// threshold. Kept as its own vec4 so the block stays 16-byte aligned.
    alpha_params: [f32; 4],
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
    name: Render3DMesh,
    type_id: "node.render_mesh",
    purpose: "Single-mesh adapter over the shared scene material evaluator. Legacy normal_map remains a signed world-space normal and roughness_map/metallic_map retain absolute red-channel semantics; complete material map families use the shared scene evaluator.",
    inputs: {
        vertices: Array(MeshVertex) required,
        camera: Camera required,
        material: Material required,
        light: Light optional,
        envmap: Texture2D optional,
        normal_map: Texture2D optional,
        roughness_map: Texture2D optional,
        base_color_map: Texture2D optional,
        metallic_map: Texture2D optional,
        mr_map: Texture2D optional,
        occlusion_map: Texture2D optional,
        emissive_map: Texture2D optional,
        sheen_color_map: Texture2D optional,
        sheen_roughness_map: Texture2D optional,
        iridescence_map: Texture2D optional,
        iridescence_thickness_map: Texture2D optional,
        anisotropy_map: Texture2D optional,
        clearcoat_map: Texture2D optional,
        clearcoat_roughness_map: Texture2D optional,
        clearcoat_normal_map: Texture2D optional,
        specular_map: Texture2D optional,
        specular_color_map: Texture2D optional,
        transmission_map: Texture2D optional,
        volume_thickness_map: Texture2D optional,
        diffuse_transmission_map: Texture2D optional,
        diffuse_transmission_color_map: Texture2D optional,
    },
    outputs: {
        color: Texture2D,
        world_pos: Texture2D,
        world_normal: Texture2D,
    },
    params: [],
    depth_rule: SourceHeight,
    composition_notes: "Vertex count must be a multiple of 3 (trailing partial triangle truncated). Wire a material, camera, optional light/envmap, and any legacy or complete material maps. Color uses the shared scene evaluator; the G-buffer outputs (`world_pos`, `world_normal`) remain independent and available for downstream deferred-shading-style work. Output formats are Rgba16Float.",
    examples: [],
    picker: { label: "Render Mesh", category: Atom },
    summary: "Draws a 3D mesh to the screen with a camera, a light, and a material. The final step that turns geometry into an image.",
    category: Geometry3D,
    role: Filter,
    aliases: ["render mesh", "render 3d mesh", "draw 3d", "rasterize", "Render TOP"],
    boundary_reason: DrawCall,
    extra_fields: {
        scene_renderer: super::render_scene::RenderScene =
            super::render_scene::RenderScene::for_single_mesh(),
        world_pos_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        world_normal_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        depth_stencil: Option<manifold_gpu::GpuDepthStencilState> = None,
        depth_texture: Option<manifold_gpu::GpuTexture> = None,
        depth_width: u32 = 0,
        depth_height: u32 = 0,
        dummy_envmap: Option<manifold_gpu::GpuTexture> = None,
    },
}

impl Render3DMesh {
    fn ensure_depth_texture(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.depth_width == width && self.depth_height == height && self.depth_texture.is_some()
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
            label: "node.render_mesh depth",
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
                ..Default::default()
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
                label: "node.render_mesh dummy envmap",
                mip_levels: 1,
            }));
        }
    }
}

fn build_gbuffer_uniforms(view_proj: [[f32; 4]; 4]) -> GBufferUniforms {
    GBufferUniforms {
        view_proj,
        camera_pos: [0.0; 4],
        light_dir: [0.0; 4],
        light_color: [0.0; 4],
        base_color: [0.0; 4],
        emission: [0.0; 4],
        pbr_metallic_roughness: [0.0; 4],
        specular: [0.0; 4],
        cel_params: [0.0; 4],
        texture_flags: [0.0; 4],
        alpha_params: [0.0; 4],
    }
}

impl Primitive for Render3DMesh {
    fn conditional_requirements(&self) -> &'static [ConditionalRequirement] {
        CONDITIONAL_RULES
    }

    /// Rasterizer outputs are screen-space: always canvas-sized. The
    /// texture inputs (envmap, normal/roughness/base-color/metallic maps)
    /// are scene resources — without this declaration the plan's
    /// max-of-input-dims default would size the render target to the
    /// largest wired map instead of the canvas (BUG-140 class).
    fn output_canvas_scale(
        &self,
        _port: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        Some((1, 1))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cam = ctx
            .inputs
            .camera("camera")
            .unwrap_or_else(Camera::default_perspective);

        // Material is REQUIRED. Missing → structured error + magenta
        // clear on `color` (per the no-silent-fallbacks rule).
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

        // Conditional inputs per kind. Resolve at runtime — the
        // statically-resolvable case was caught at preset-load by the
        // validator, but a material flowing through a mux (or any
        // future Authored kind) lands here.
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

        let (width, height, vertex_count) = {
            let Some(vertices) = ctx.inputs.array("vertices") else {
                return;
            };
            let color_target = ctx.outputs.texture_2d("color");
            let world_pos_target = ctx.outputs.texture_2d("world_pos");
            let world_normal_target = ctx.outputs.texture_2d("world_normal");
            let Some(dims_tex) = color_target.or(world_pos_target).or(world_normal_target) else {
                return;
            };
            let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
            let vertex_capacity = (vertices.size / vertex_size) as u32;
            (
                (dims_tex.width),
                (dims_tex.height),
                (vertex_capacity / 3) * 3,
            )
        };
        if width == 0 || height == 0 {
            return;
        }
        if vertex_count == 0 {
            if let Some(c) = ctx.outputs.texture_2d("color") {
                ctx.gpu_encoder()
                    .native_enc
                    .clear_texture(c, 0.0, 0.0, 0.0, 0.0);
            }
            if let Some(wp) = ctx.outputs.texture_2d("world_pos") {
                ctx.gpu_encoder()
                    .native_enc
                    .clear_texture(wp, 0.0, 0.0, 0.0, 0.0);
            }
            if let Some(wn) = ctx.outputs.texture_2d("world_normal") {
                ctx.gpu_encoder()
                    .native_enc
                    .clear_texture(wn, 0.0, 0.0, 0.0, 0.0);
            }
            return;
        }

        // The shared evaluator owns the color pass, including complete
        // material maps and Blend coverage. Call it before borrowing the
        // encoder for the independent G-buffer passes below.
        self.scene_renderer.render_single_mesh(ctx, None);

        let aspect = width as f32 / height as f32;
        let view_proj = cam.view_proj(aspect);
        let uniforms = build_gbuffer_uniforms(view_proj);
        let vertices = ctx.inputs.array("vertices").expect("validated mesh input");
        let world_pos_target = ctx.outputs.texture_2d("world_pos");
        let world_normal_target = ctx.outputs.texture_2d("world_normal");

        let gpu = ctx.gpu_encoder();

        if self.depth_stencil.is_none() {
            self.depth_stencil = Some(gpu.device.create_depth_stencil_state(
                &manifold_gpu::GpuDepthStencilDesc {
                    compare: manifold_gpu::GpuCompareFunction::Greater,
                    write_enabled: true,
                },
            ));
        }
        self.ensure_depth_texture(gpu.device, width, height);
        self.ensure_sampler(gpu.device);
        self.ensure_dummy_envmap(gpu.device);

        let depth_stencil = self.depth_stencil.as_ref().expect("just inserted");
        let depth_tex = self.depth_texture.as_ref().expect("just inserted");
        let sampler = self.sampler.as_ref().expect("just inserted");
        let dummy_envmap = self.dummy_envmap.as_ref().expect("just inserted");

        // ===== G-buffer passes (independent of material) =====
        // Bind the same vertex buffer + uniform (G-buffer shaders only
        // read view_proj; everything else is inert). Dummy envmap /
        // normal_map / roughness_map + sampler are bound for binding-
        // layout completeness but the entry points don't reference
        // them.
        let gbuffer_bindings = [
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
                texture: dummy_envmap,
            },
            GpuBinding::Sampler {
                binding: 3,
                sampler,
            },
            GpuBinding::Texture {
                binding: 4,
                texture: dummy_envmap,
            },
            GpuBinding::Texture {
                binding: 5,
                texture: dummy_envmap,
            },
            GpuBinding::Texture {
                binding: 6,
                texture: dummy_envmap,
            },
            GpuBinding::Texture {
                binding: 7,
                texture: dummy_envmap,
            },
        ];
        if let Some(wp_target) = world_pos_target {
            if self.world_pos_pipeline.is_none() {
                self.world_pos_pipeline = Some(gpu.device.create_render_pipeline_depth(
                    include_str!("shaders/render_3d_mesh.wgsl"),
                    "vs_main",
                    "fs_world_pos",
                    manifold_gpu::GpuTextureFormat::Rgba16Float,
                    manifold_gpu::GpuTextureFormat::Depth32Float,
                    None,
                    1,
                    "node.render_mesh.world_pos",
                ));
            }
            let pipeline = self.world_pos_pipeline.as_ref().expect("just inserted");
            gpu.native_enc.draw_instanced_depth(
                pipeline,
                wp_target,
                depth_tex,
                depth_stencil,
                &gbuffer_bindings,
                vertex_count,
                1,
                GpuLoadAction::Clear,
                "node.render_mesh.world_pos",
            );
        }
        if let Some(wn_target) = world_normal_target {
            if self.world_normal_pipeline.is_none() {
                self.world_normal_pipeline = Some(gpu.device.create_render_pipeline_depth(
                    include_str!("shaders/render_3d_mesh.wgsl"),
                    "vs_main",
                    "fs_world_normal",
                    manifold_gpu::GpuTextureFormat::Rgba16Float,
                    manifold_gpu::GpuTextureFormat::Depth32Float,
                    None,
                    1,
                    "node.render_mesh.world_normal",
                ));
            }
            let pipeline = self.world_normal_pipeline.as_ref().expect("just inserted");
            gpu.native_enc.draw_instanced_depth(
                pipeline,
                wn_target,
                depth_tex,
                depth_stencil,
                &gbuffer_bindings,
                vertex_count,
                1,
                GpuLoadAction::Clear,
                "node.render_mesh.world_normal",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::EffectNode;

    #[test]
    fn render_3d_mesh_declares_material_required_inputs() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();

        assert_eq!(Render3DMesh::TYPE_ID, "node.render_mesh");
        let by_name = |n: &str| {
            Render3DMesh::INPUTS
                .iter()
                .find(|p| p.name == n)
                .unwrap_or_else(|| panic!("missing input {n}"))
        };
        let vertices = by_name("vertices");
        assert!(vertices.required);
        assert_eq!(vertices.ty, PortType::Array(mesh_layout));
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
        let normal_map = by_name("normal_map");
        assert!(!normal_map.required);
        assert_eq!(normal_map.ty, PortType::Texture2D);
        let roughness_map = by_name("roughness_map");
        assert!(!roughness_map.required);
        assert_eq!(roughness_map.ty, PortType::Texture2D);
        let base_color_map = by_name("base_color_map");
        assert!(!base_color_map.required);
        assert_eq!(base_color_map.ty, PortType::Texture2D);
        let metallic_map = by_name("metallic_map");
        assert!(!metallic_map.required);
        assert_eq!(metallic_map.ty, PortType::Texture2D);
        for name in [
            "mr_map",
            "occlusion_map",
            "emissive_map",
            "sheen_color_map",
            "sheen_roughness_map",
            "iridescence_map",
            "iridescence_thickness_map",
            "anisotropy_map",
            "clearcoat_map",
            "clearcoat_roughness_map",
            "clearcoat_normal_map",
            "specular_map",
            "specular_color_map",
            "transmission_map",
            "volume_thickness_map",
            "diffuse_transmission_map",
            "diffuse_transmission_color_map",
        ] {
            let input = by_name(name);
            assert!(!input.required, "{name} must remain optional");
            assert_eq!(input.ty, PortType::Texture2D, "{name} type");
        }
    }

    #[test]
    fn render_3d_mesh_has_no_legacy_scalar_params() {
        // Material system M4 removed scattered light_intensity / ambient /
        // color_r/g/b — the Material wire is the only surface knob now.
        assert!(
            Render3DMesh::PARAMS.is_empty(),
            "render_3d_mesh should expose no scalar params after Material migration; got {:?}",
            Render3DMesh::PARAMS
                .iter()
                .map(|p| p.name.as_ref())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn render_3d_mesh_declares_conditional_requirements() {
        let prim = Render3DMesh::new();
        let node: &dyn EffectNode = &prim;
        let rules = node.conditional_requirements();
        assert_eq!(
            rules.len(),
            3,
            "expected Phong/Pbr/Cel rules, got {rules:?}"
        );
        let by_kind = |k: MaterialKind| {
            rules
                .iter()
                .find(|r| r.on_material_kind == k)
                .unwrap_or_else(|| panic!("missing rule for {k:?}"))
        };
        assert_eq!(by_kind(MaterialKind::Phong).required_inputs, &["light"]);
        assert_eq!(
            by_kind(MaterialKind::Pbr).required_inputs,
            &["light", "envmap"]
        );
        assert_eq!(by_kind(MaterialKind::Cel).required_inputs, &["light"]);
    }

    #[test]
    fn render_3d_mesh_outputs_color_and_gbuffer() {
        use crate::node_graph::ports::PortType;
        assert_eq!(Render3DMesh::OUTPUTS.len(), 3);
        assert_eq!(Render3DMesh::OUTPUTS[0].name, "color");
        assert_eq!(Render3DMesh::OUTPUTS[0].ty, PortType::Texture2D);
        assert_eq!(Render3DMesh::OUTPUTS[1].name, "world_pos");
        assert_eq!(Render3DMesh::OUTPUTS[2].name, "world_normal");
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Render3DMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.render_mesh");
    }
}
