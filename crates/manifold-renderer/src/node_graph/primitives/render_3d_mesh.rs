//! `node.render_mesh` — bundled 3D mesh renderer that dispatches
//! a per-MaterialKind fragment shader (Unlit / Phong / PBR / Cel) over
//! a triangle-list `Array<MeshVertex>` with depth testing.
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
//! - per-MaterialKind render pipeline cache (`AHashMap<MaterialKind, _>`)
//! - G-buffer pipelines (world_pos, world_normal) — preserved unchanged
//! - depth-stencil state, depth texture resized to current output dims
//! - dummy 1×1 envmap + sampler used for the PBR pipeline when an envmap
//!   binding is required but the wire is unwired (runtime error path)

use ahash::AHashMap;
use manifold_gpu::{GpuBinding, GpuLoadAction};

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::{ConditionalRequirement, EffectNodeContext};
use crate::node_graph::material::{Material, MaterialKind};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaterialRenderUniforms {
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
    purpose: "Bundled 3D mesh renderer (TouchDesigner / Blender shape). Reads an Array<MeshVertex> as a triangle list, takes a Camera + Material + optional Light + optional envmap + optional surface textures (normal_map / roughness_map), and emits a shaded `color` Texture2D plus optional G-buffer outputs (`world_pos`, `world_normal`). The material's MaterialKind picks the fragment shader — Unlit / Phong / PBR / Cel. Per-kind requirements: Unlit needs no light. Phong / Cel need `light`. PBR needs `light` AND `envmap`. Surface textures sample at each fragment's mesh UV (the per-vertex `uv` channel interpolated through the rasterizer), so the texture sticks to the geometry as the camera moves — the industry-standard mesh-UV pattern. normal_map is interpreted as a world-space signed normal; roughness_map's red channel replaces the material's scalar roughness when wired; base_color_map (rgba) modulates the material's base colour and supplies the cutout-alpha source; metallic_map's red channel replaces the material's scalar metallic when wired.",
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
    },
    outputs: {
        color: Texture2D,
        world_pos: Texture2D,
        world_normal: Texture2D,
    },
    params: [],
    depth_rule: SourceHeight,
    composition_notes: "Vertex count must be a multiple of 3 (trailing partial triangle truncated). Wire a `node.{unlit,phong,pbr,cel}_material` into `material` to pick the shading model. Pair with `node.orbit_camera` for `camera`, `node.light` for `light`, and (PBR only) `node.bake_environment` for `envmap`. The G-buffer outputs (`world_pos`, `world_normal`) stay available for downstream deferred-shading-style work; they don't depend on material and won't compile their pipelines unless wired downstream. The `color` output is the primary path. Output formats are Rgba16Float.",
    examples: [],
    picker: { label: "Render Mesh", category: Atom },
    summary: "Draws a 3D mesh to the screen with a camera, a light, and a material. The final step that turns geometry into an image.",
    category: Geometry3D,
    role: Filter,
    aliases: ["render mesh", "render 3d mesh", "draw 3d", "rasterize", "Render TOP"],
    boundary_reason: DrawCall,
    extra_fields: {
        pipelines: AHashMap<MaterialKind, manifold_gpu::GpuRenderPipeline> = AHashMap::new(),
        world_pos_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        world_normal_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        depth_stencil: Option<manifold_gpu::GpuDepthStencilState> = None,
        depth_texture: Option<manifold_gpu::GpuTexture> = None,
        depth_width: u32 = 0,
        depth_height: u32 = 0,
        // Memoryless 4x-MSAA color + depth for the antialiased color pass.
        // The G-buffer passes deliberately stay single-sample (resolving
        // world positions/normals across a silhouette averages to garbage),
        // so they keep their own single-sample `depth_texture`.
        msaa_color: Option<manifold_gpu::GpuTexture> = None,
        msaa_depth: Option<manifold_gpu::GpuTexture> = None,
        msaa_width: u32 = 0,
        msaa_height: u32 = 0,
        dummy_envmap: Option<manifold_gpu::GpuTexture> = None,
    },
}

/// 4x MSAA for the color pass. Memoryless multisample color + depth are
/// tile-resident on Apple Silicon and resolve on-chip — no VRAM cost.
/// Paired with alpha-to-coverage so glTF `Mask` cutout edges antialias too.
const MSAA_SAMPLES: u32 = 4;

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
            label: "node.render_mesh depth",
            mip_levels: 1,
        }));
        self.depth_width = width;
        self.depth_height = height;
    }

    /// Memoryless 4x-MSAA color + depth for the color pass, sized to the
    /// render target. Tile-resident; the color resolves out at pass end.
    fn ensure_msaa_targets(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.msaa_width == width
            && self.msaa_height == height
            && self.msaa_color.is_some()
            && self.msaa_depth.is_some()
        {
            return;
        }
        self.msaa_color = Some(device.create_texture_msaa_memoryless(
            width,
            height,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            MSAA_SAMPLES,
            "node.render_mesh msaa color",
        ));
        self.msaa_depth = Some(device.create_texture_msaa_memoryless(
            width,
            height,
            manifold_gpu::GpuTextureFormat::Depth32Float,
            MSAA_SAMPLES,
            "node.render_mesh msaa depth",
        ));
        self.msaa_width = width;
        self.msaa_height = height;
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
            device.create_render_pipeline_depth_msaa(
                include_str!("shaders/render_3d_mesh.wgsl"),
                "vs_main",
                fs_entry,
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                manifold_gpu::GpuTextureFormat::Depth32Float,
                None,
                MSAA_SAMPLES,
                true, // alpha-to-coverage: antialias the cutout `discard` edge too
                "node.render_mesh",
            )
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn build_uniforms(
    view_proj: [[f32; 4]; 4],
    cam: &Camera,
    light_dir: [f32; 3],
    light_color: [f32; 4],
    material: &Material,
    use_normal_map: bool,
    use_roughness_map: bool,
    use_base_color_map: bool,
    use_metallic_map: bool,
) -> MaterialRenderUniforms {
    MaterialRenderUniforms {
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
        texture_flags: [
            if use_normal_map { 1.0 } else { 0.0 },
            if use_roughness_map { 1.0 } else { 0.0 },
            if use_base_color_map { 1.0 } else { 0.0 },
            if use_metallic_map { 1.0 } else { 0.0 },
        ],
        alpha_params: [
            match material.alpha_mode {
                crate::node_graph::material::AlphaMode::Mask => 1.0,
                // IMPORT_FIDELITY_DESIGN.md D1: render_mesh doesn't implement
                // the sorted blend pass — a Blend material here renders as
                // Opaque coverage until the render_mesh IBL-upgrade trigger
                // (§7 Deferred #3) migrates this renderer too.
                crate::node_graph::material::AlphaMode::Opaque
                | crate::node_graph::material::AlphaMode::Blend => 0.0,
            },
            material.alpha_cutoff,
            0.0,
            0.0,
        ],
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
        let normal_map_wired = ctx.inputs.texture_2d("normal_map");
        let roughness_map_wired = ctx.inputs.texture_2d("roughness_map");
        let base_color_map_wired = ctx.inputs.texture_2d("base_color_map");
        let metallic_map_wired = ctx.inputs.texture_2d("metallic_map");

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

        // Light-derived uniform fields. Sun: parallel direction
        // (negated forward). Point: collapsed to forward direction
        // (per-pixel attenuation lands when world_pos is bound and
        // shaders interpolate position — out of scope for v1's
        // non-G-buffer fragment paths).
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
        let color_target = ctx.outputs.texture_2d("color");
        let world_pos_target = ctx.outputs.texture_2d("world_pos");
        let world_normal_target = ctx.outputs.texture_2d("world_normal");
        let dims_source = color_target.or(world_pos_target).or(world_normal_target);
        let Some(dims_tex) = dims_source else {
            return;
        };
        let width = dims_tex.width;
        let height = dims_tex.height;
        if width == 0 || height == 0 {
            return;
        }

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let vertex_capacity = (vertices.size / vertex_size) as u32;
        let vertex_count = (vertex_capacity / 3) * 3;
        if vertex_count == 0 {
            let gpu = ctx.gpu_encoder();
            if let Some(c) = color_target {
                gpu.native_enc.clear_texture(c, 0.0, 0.0, 0.0, 0.0);
            }
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
        let uniforms = build_uniforms(
            view_proj,
            &cam,
            light_dir,
            light_color,
            &material,
            normal_map_wired.is_some(),
            roughness_map_wired.is_some(),
            base_color_map_wired.is_some(),
            metallic_map_wired.is_some(),
        );

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
        self.ensure_msaa_targets(gpu.device, width, height);
        self.ensure_sampler(gpu.device);
        self.ensure_dummy_envmap(gpu.device);
        // Material pipeline needs a mut borrow on `self.pipelines`;
        // pull a clone out BEFORE we take immutable refs to the other
        // cached resources, so the borrow checker can sequence the
        // accesses.
        let material_pipeline = if color_target.is_some() {
            Some(self.pipeline_for(gpu.device, material.kind).clone())
        } else {
            None
        };

        let depth_stencil = self.depth_stencil.as_ref().expect("just inserted");
        let depth_tex = self.depth_texture.as_ref().expect("just inserted");
        let msaa_color = self.msaa_color.as_ref().expect("just inserted");
        let msaa_depth = self.msaa_depth.as_ref().expect("just inserted");
        let sampler = self.sampler.as_ref().expect("just inserted");
        let dummy_envmap = self.dummy_envmap.as_ref().expect("just inserted");

        // ===== Material color pass =====
        if let (Some(target), Some(pipeline)) = (color_target, material_pipeline.as_ref()) {
            // PBR needs an envmap binding; the validator already
            // guaranteed it's wired (or we returned magenta earlier).
            // Unlit / Phong / Cel pipelines never reference envmap /
            // normal_map / roughness_map, so binding the dummy is
            // harmless — naga's per-entry-point MSL drops the unused
            // arguments. The presence flags in `texture_flags` gate
            // sampling on the entry points that DO reference them.
            let envmap_texture = envmap_wired.unwrap_or(dummy_envmap);
            let normal_map_texture = normal_map_wired.unwrap_or(dummy_envmap);
            let roughness_map_texture = roughness_map_wired.unwrap_or(dummy_envmap);
            let base_color_map_texture = base_color_map_wired.unwrap_or(dummy_envmap);
            let metallic_map_texture = metallic_map_wired.unwrap_or(dummy_envmap);
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
                GpuBinding::Texture {
                    binding: 2,
                    texture: envmap_texture,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: normal_map_texture,
                },
                GpuBinding::Texture {
                    binding: 5,
                    texture: roughness_map_texture,
                },
                GpuBinding::Texture {
                    binding: 6,
                    texture: base_color_map_texture,
                },
                GpuBinding::Texture {
                    binding: 7,
                    texture: metallic_map_texture,
                },
            ];
            // Antialiased color pass: single mesh, one 4x-MSAA pass
            // (memoryless color+depth) resolving out to `target`.
            let draw = manifold_gpu::GpuEncoder::depth_msaa_draw(
                pipeline,
                &bindings,
                vertex_count,
                1,
            );
            gpu.native_enc.draw_instanced_depth_msaa_batch(
                msaa_color,
                target,
                msaa_depth,
                depth_stencil,
                std::slice::from_ref(&draw),
                "node.render_mesh.color",
            );
        }

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
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

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
    }

    #[test]
    fn render_3d_mesh_has_no_legacy_scalar_params() {
        // Material system M4 removed scattered light_intensity / ambient /
        // color_r/g/b — the Material wire is the only surface knob now.
        assert!(
            Render3DMesh::PARAMS.is_empty(),
            "render_3d_mesh should expose no scalar params after Material migration; got {:?}",
            Render3DMesh::PARAMS.iter().map(|p| p.name.as_ref()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn render_3d_mesh_declares_conditional_requirements() {
        let prim = Render3DMesh::new();
        let node: &dyn EffectNode = &prim;
        let rules = node.conditional_requirements();
        assert_eq!(rules.len(), 3, "expected Phong/Pbr/Cel rules, got {rules:?}");
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
