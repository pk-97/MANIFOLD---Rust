//! `node.render_scene` — multi-object 3D scene renderer: N objects drawn
//! into ONE shared depth buffer (real occlusion between objects) lit by
//! up to 4 shared lights.
//!
//! Hand-written dynamic [`EffectNode`] (mux_texture-shaped, NOT the
//! `primitive!` macro): the `objects` param (1..=8) and `lights` param
//! (0..=4) each rebuild the port list and the per-object transform
//! params in [`reconfigure`](EffectNode::reconfigure) — same pattern as
//! `node.switch_texture`'s `num_inputs`.
//!
//! `node.render_mesh` / `node.render_copies` each render ONE object into
//! their own private depth buffer — two objects from either of those
//! primitives cannot occlude each other. `render_scene` closes that gap:
//! object 0 clears the shared colour+depth target, objects 1..N load
//! onto it (`GpuLoadAction::Clear` then `GpuLoadAction::Load`, per
//! `manifold_gpu`'s `draw_instanced_depth`), so the depth test resolves
//! real occlusion regardless of draw order.
//!
//! Per docs/REALTIME_3D_DESIGN.md §2 D2/D3 and §5 P1: no shadows (P2),
//! no atmosphere/fog (P3), no per-object surface-texture inputs or
//! port-shadowed transforms in P1 (transform params are plain params —
//! a documented additive follow-up). ONE shared `envmap` input lights
//! every PBR object in the scene (an environment map is scene-wide by
//! nature, not per-object).

use ahash::AHashMap;
use manifold_gpu::{GpuBinding, GpuLoadAction};

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, ParamValues,
};
use crate::node_graph::material::{AlphaMode, Material, MaterialKind};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::primitive::PrimitiveDescription;

pub const RENDER_SCENE_TYPE_ID: &str = "node.render_scene";

/// Hard cap on object count. Bounds the static port/param-name tables
/// below. Per docs/REALTIME_3D_DESIGN.md §2 D3.
const MAX_OBJECTS: usize = 8;
/// Hard cap on light count. Light is a CPU struct — no `Array<Light>`
/// port type is invented; each light gets its own dynamic `light_N`
/// port instead (D3).
const MAX_LIGHTS: usize = 4;

const DEFAULT_OBJECTS: u32 = 2;
const DEFAULT_LIGHTS: u32 = 1;

// ---------------------------------------------------------------------
// Static name tables. A dynamic-port node can't `format!` a `&'static
// str` per instance, so every name a port or param could ever need
// lives here, mirroring `mux_texture::IN_PORT_NAMES`.
// ---------------------------------------------------------------------

const MESH_NAMES: [&str; MAX_OBJECTS] = [
    "mesh_0", "mesh_1", "mesh_2", "mesh_3", "mesh_4", "mesh_5", "mesh_6", "mesh_7",
];
const MATERIAL_NAMES: [&str; MAX_OBJECTS] = [
    "material_0", "material_1", "material_2", "material_3", "material_4", "material_5",
    "material_6", "material_7",
];
const LIGHT_NAMES: [&str; MAX_LIGHTS] = ["light_0", "light_1", "light_2", "light_3"];

const POS_X_NAMES: [&str; MAX_OBJECTS] = [
    "pos_x_0", "pos_x_1", "pos_x_2", "pos_x_3", "pos_x_4", "pos_x_5", "pos_x_6", "pos_x_7",
];
const POS_Y_NAMES: [&str; MAX_OBJECTS] = [
    "pos_y_0", "pos_y_1", "pos_y_2", "pos_y_3", "pos_y_4", "pos_y_5", "pos_y_6", "pos_y_7",
];
const POS_Z_NAMES: [&str; MAX_OBJECTS] = [
    "pos_z_0", "pos_z_1", "pos_z_2", "pos_z_3", "pos_z_4", "pos_z_5", "pos_z_6", "pos_z_7",
];
const ROT_X_NAMES: [&str; MAX_OBJECTS] = [
    "rot_x_0", "rot_x_1", "rot_x_2", "rot_x_3", "rot_x_4", "rot_x_5", "rot_x_6", "rot_x_7",
];
const ROT_Y_NAMES: [&str; MAX_OBJECTS] = [
    "rot_y_0", "rot_y_1", "rot_y_2", "rot_y_3", "rot_y_4", "rot_y_5", "rot_y_6", "rot_y_7",
];
const ROT_Z_NAMES: [&str; MAX_OBJECTS] = [
    "rot_z_0", "rot_z_1", "rot_z_2", "rot_z_3", "rot_z_4", "rot_z_5", "rot_z_6", "rot_z_7",
];
const SCALE_X_NAMES: [&str; MAX_OBJECTS] = [
    "scale_x_0", "scale_x_1", "scale_x_2", "scale_x_3", "scale_x_4", "scale_x_5", "scale_x_6",
    "scale_x_7",
];
const SCALE_Y_NAMES: [&str; MAX_OBJECTS] = [
    "scale_y_0", "scale_y_1", "scale_y_2", "scale_y_3", "scale_y_4", "scale_y_5", "scale_y_6",
    "scale_y_7",
];
const SCALE_Z_NAMES: [&str; MAX_OBJECTS] = [
    "scale_z_0", "scale_z_1", "scale_z_2", "scale_z_3", "scale_z_4", "scale_z_5", "scale_z_6",
    "scale_z_7",
];

const RENDER_SCENE_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: "color",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

/// Per-object uniform, one draw call per object. Superset shape shared
/// with `render_3d_mesh.wgsl`'s Material fields, plus a per-object
/// `model` matrix and a shared `lights` accumulator. See
/// `shaders/render_scene.wgsl` for the authoritative layout comment.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RenderSceneUniforms {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    base_color: [f32; 4],
    emission: [f32; 4],
    pbr_metallic_roughness: [f32; 4],
    specular: [f32; 4],
    cel_params: [f32; 4],
    /// Always zero in P1 — no per-object surface-texture inputs.
    texture_flags: [f32; 4],
    alpha_params: [f32; 4],
    /// `(light_count, ambient, 0, 0)`.
    scene_params: [f32; 4],
    /// `lights[i*2]` = (dir, intensity); `lights[i*2+1]` = (color, _).
    lights: [[f32; 4]; MAX_LIGHTS * 2],
}

const _: () = assert!(std::mem::size_of::<RenderSceneUniforms>() == 400);

pub struct RenderScene {
    inputs: Vec<NodeInput>,
    params: Vec<ParamDef>,
    num_objects: u32,
    num_lights: u32,
    pipelines: AHashMap<MaterialKind, manifold_gpu::GpuRenderPipeline>,
    depth_stencil: Option<manifold_gpu::GpuDepthStencilState>,
    depth_texture: Option<manifold_gpu::GpuTexture>,
    depth_width: u32,
    depth_height: u32,
    dummy_texture: Option<manifold_gpu::GpuTexture>,
    sampler: Option<manifold_gpu::GpuSampler>,
}

impl RenderScene {
    pub fn new() -> Self {
        let mut s = Self {
            inputs: Vec::new(),
            params: Vec::new(),
            num_objects: 0,
            num_lights: 0,
            pipelines: AHashMap::new(),
            depth_stencil: None,
            depth_texture: None,
            depth_width: 0,
            depth_height: 0,
            dummy_texture: None,
            sampler: None,
        };
        s.rebuild(DEFAULT_OBJECTS, DEFAULT_LIGHTS);
        s
    }

    /// (Re)build the input port list AND the parameter list for
    /// `objects` objects / `lights` lights (each clamped to its cap).
    fn rebuild(&mut self, objects: u32, lights: u32) {
        let objects = objects.clamp(1, MAX_OBJECTS as u32);
        let lights = lights.clamp(0, MAX_LIGHTS as u32);
        let n_obj = objects as usize;
        let n_lights = lights as usize;

        let mut inputs = Vec::with_capacity(2 + n_lights + n_obj * 2);
        inputs.push(NodePort {
            name: "camera",
            ty: PortType::Camera,
            kind: PortKind::Input,
            required: true,
        });
        inputs.push(NodePort {
            name: "envmap",
            ty: PortType::Texture2D,
            kind: PortKind::Input,
            required: false,
        });
        for &name in &LIGHT_NAMES[..n_lights] {
            inputs.push(NodePort {
                name,
                ty: PortType::Light,
                kind: PortKind::Input,
                required: false,
            });
        }
        let mesh_ty = PortType::Array(ArrayType::of_known::<MeshVertex>());
        for i in 0..n_obj {
            inputs.push(NodePort {
                name: MESH_NAMES[i],
                ty: mesh_ty,
                kind: PortKind::Input,
                required: true,
            });
            inputs.push(NodePort {
                name: MATERIAL_NAMES[i],
                ty: PortType::Material,
                kind: PortKind::Input,
                required: true,
            });
        }

        let mut params = Vec::with_capacity(2 + n_obj * 9);
        params.push(ParamDef {
            name: "objects",
            label: "Objects",
            ty: ParamType::Int,
            default: ParamValue::Float(DEFAULT_OBJECTS as f32),
            range: Some((1.0, MAX_OBJECTS as f32)),
            enum_values: &[],
        });
        params.push(ParamDef {
            name: "lights",
            label: "Lights",
            ty: ParamType::Int,
            default: ParamValue::Float(DEFAULT_LIGHTS as f32),
            range: Some((0.0, MAX_LIGHTS as f32)),
            enum_values: &[],
        });
        for i in 0..n_obj {
            params.push(ParamDef {
                name: POS_X_NAMES[i],
                label: "Position X",
                ty: ParamType::Float,
                default: ParamValue::Float(0.0),
                range: Some((-100.0, 100.0)),
                enum_values: &[],
            });
            params.push(ParamDef {
                name: POS_Y_NAMES[i],
                label: "Position Y",
                ty: ParamType::Float,
                default: ParamValue::Float(0.0),
                range: Some((-100.0, 100.0)),
                enum_values: &[],
            });
            params.push(ParamDef {
                name: POS_Z_NAMES[i],
                label: "Position Z",
                ty: ParamType::Float,
                default: ParamValue::Float(0.0),
                range: Some((-100.0, 100.0)),
                enum_values: &[],
            });
            params.push(ParamDef {
                name: ROT_X_NAMES[i],
                label: "Rotation X",
                ty: ParamType::Angle,
                default: ParamValue::Float(0.0),
                range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
                enum_values: &[],
            });
            params.push(ParamDef {
                name: ROT_Y_NAMES[i],
                label: "Rotation Y",
                ty: ParamType::Angle,
                default: ParamValue::Float(0.0),
                range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
                enum_values: &[],
            });
            params.push(ParamDef {
                name: ROT_Z_NAMES[i],
                label: "Rotation Z",
                ty: ParamType::Angle,
                default: ParamValue::Float(0.0),
                range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
                enum_values: &[],
            });
            params.push(ParamDef {
                name: SCALE_X_NAMES[i],
                label: "Scale X",
                ty: ParamType::Float,
                default: ParamValue::Float(1.0),
                range: Some((0.01, 10.0)),
                enum_values: &[],
            });
            params.push(ParamDef {
                name: SCALE_Y_NAMES[i],
                label: "Scale Y",
                ty: ParamType::Float,
                default: ParamValue::Float(1.0),
                range: Some((0.01, 10.0)),
                enum_values: &[],
            });
            params.push(ParamDef {
                name: SCALE_Z_NAMES[i],
                label: "Scale Z",
                ty: ParamType::Float,
                default: ParamValue::Float(1.0),
                range: Some((0.01, 10.0)),
                enum_values: &[],
            });
        }

        self.inputs = inputs;
        self.params = params;
        self.num_objects = objects;
        self.num_lights = lights;
    }

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
            label: "node.render_scene depth",
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

    fn ensure_dummy_texture(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.dummy_texture.is_none() {
            self.dummy_texture = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ,
                label: "node.render_scene dummy",
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
                include_str!("shaders/render_scene.wgsl"),
                "vs_main",
                fs_entry,
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                manifold_gpu::GpuTextureFormat::Depth32Float,
                None,
                1,
                "node.render_scene",
            )
        })
    }

    /// AI-composition surface metadata.
    pub fn description() -> PrimitiveDescription {
        PrimitiveDescription {
            type_id: RENDER_SCENE_TYPE_ID,
            purpose: "Multi-object 3D scene renderer: draws `objects` (1..=8) separate Array<MeshVertex> meshes into ONE shared depth buffer, so nearer objects correctly occlude farther ones — the gap node.render_mesh / node.render_copies can't close (each of those renders into its own private depth buffer). Each object carries its own material_n: Material and pos_x/y/z + rot_x/y/z + scale_x/y/z transform params (composed CPU-side into a model matrix). Up to `lights` (0..=4) shared Light inputs light_0..light_{lights-1} accumulate in the Phong/PBR/Cel shading — each light's direct term is summed, ambient + emission are added once. ONE shared envmap input lights every PBR object in the scene (an environment map is scene-wide, not per-object). No shadows (P2), no atmosphere/fog (P3), no per-object surface textures in P1.",
            composition_notes: "objects and lights are reconfigure params: changing either rebuilds the port list (mesh_n/material_n pairs, light_0..N) and the per-object transform params, same dynamic-port pattern as node.switch_texture's num_inputs. Transform params (pos/rot/scale per object) are plain params in P1 — not port-shadowed, so they aren't beat-modulatable yet (a documented additive follow-up, not a v1 gap). A missing mesh_n or material_n, or a PBR material_n with envmap left unwired, is a structured error (ctx.error + magenta clear on `color`), matching render_mesh's no-silent-fallbacks contract. Object 0 clears the shared color+depth target; objects 1..N load onto it — the shared depth buffer resolves occlusion regardless of which object happens to be object 0.",
            examples: &[],
            inputs: &[],
            outputs: &RENDER_SCENE_OUTPUTS,
            params: &[],
        }
    }
}

impl Default for RenderScene {
    fn default() -> Self {
        Self::new()
    }
}

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: std::sync::OnceLock<EffectNodeType> = std::sync::OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(RENDER_SCENE_TYPE_ID))
}

/// Build a column-major 4×4 model matrix from TRS params: `model = T *
/// R * S`. Rotation order matches `render_instanced_3d_mesh.wgsl`'s
/// `euler_xyz` (X → Y → Z, i.e. `R = Rz * Ry * Rx`) so render_scene and
/// render_copies agree on what a given `(rot_x, rot_y, rot_z)` means.
/// Non-uniform scale's normal skew is NOT corrected (no
/// inverse-transpose) — v1 limitation, documented on the shader side.
fn model_matrix(pos: [f32; 3], rot: [f32; 3], scale: [f32; 3]) -> [[f32; 4]; 4] {
    let r = euler_xyz_columns(rot);
    [
        [
            r[0][0] * scale[0],
            r[0][1] * scale[0],
            r[0][2] * scale[0],
            0.0,
        ],
        [
            r[1][0] * scale[1],
            r[1][1] * scale[1],
            r[1][2] * scale[1],
            0.0,
        ],
        [
            r[2][0] * scale[2],
            r[2][1] * scale[2],
            r[2][2] * scale[2],
            0.0,
        ],
        [pos[0], pos[1], pos[2], 1.0],
    ]
}

/// Column-major 3×3 rotation matrix for XYZ Euler angles (radians),
/// composed as `Rz * Ry * Rx` — bit-for-bit the same column layout as
/// `render_instanced_3d_mesh.wgsl`'s `euler_xyz`.
fn euler_xyz_columns(rot: [f32; 3]) -> [[f32; 3]; 3] {
    let (cx, sx) = (rot[0].cos(), rot[0].sin());
    let (cy, sy) = (rot[1].cos(), rot[1].sin());
    let (cz, sz) = (rot[2].cos(), rot[2].sin());

    let rx = [[1.0, 0.0, 0.0], [0.0, cx, sx], [0.0, -sx, cx]];
    let ry = [[cy, 0.0, -sy], [0.0, 1.0, 0.0], [sy, 0.0, cy]];
    let rz = [[cz, sz, 0.0], [-sz, cz, 0.0], [0.0, 0.0, 1.0]];

    mat3_mul(mat3_mul(rz, ry), rx)
}

/// Multiply two 3×3 column-major matrices: result = A * B. Mirrors
/// `generators::mesh_pipeline::mat4_mul`'s convention one dimension down.
fn mat3_mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for col in 0..3 {
        for row in 0..3 {
            out[col][row] =
                a[0][row] * b[col][0] + a[1][row] * b[col][1] + a[2][row] * b[col][2];
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn build_uniforms(
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    cam: &Camera,
    material: &Material,
    light_count: f32,
    lights: [[f32; 4]; MAX_LIGHTS * 2],
) -> RenderSceneUniforms {
    RenderSceneUniforms {
        view_proj,
        model,
        camera_pos: [cam.pos[0], cam.pos[1], cam.pos[2], 1.0],
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
        texture_flags: [0.0, 0.0, 0.0, 0.0],
        alpha_params: [
            match material.alpha_mode {
                AlphaMode::Mask => 1.0,
                AlphaMode::Opaque => 0.0,
            },
            material.alpha_cutoff,
            0.0,
            0.0,
        ],
        scene_params: [light_count, material.ambient, 0.0, 0.0],
        lights,
    }
}

impl EffectNode for RenderScene {
    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }

    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }

    fn outputs(&self) -> &[NodeOutput] {
        &RENDER_SCENE_OUTPUTS
    }

    fn parameters(&self) -> &[ParamDef] {
        &self.params
    }

    fn reconfigure(&mut self, params: &ParamValues) {
        let objects = params
            .get("objects")
            .and_then(|v| v.as_scalar())
            .map(|f| f.round().clamp(1.0, MAX_OBJECTS as f32) as u32)
            .unwrap_or(DEFAULT_OBJECTS);
        let lights = params
            .get("lights")
            .and_then(|v| v.as_scalar())
            .map(|f| f.round().clamp(0.0, MAX_LIGHTS as f32) as u32)
            .unwrap_or(DEFAULT_LIGHTS);
        if objects != self.num_objects || lights != self.num_lights {
            self.rebuild(objects, lights);
        }
    }

    fn evaluate<'ctx, 'gpu>(&mut self, ctx: &mut EffectNodeContext<'ctx, 'gpu>) {
        let objects = self.num_objects as usize;
        let lights_n = self.num_lights as usize;

        let cam = ctx
            .inputs
            .camera("camera")
            .unwrap_or_else(Camera::default_perspective);
        let envmap_wired = ctx.inputs.texture_2d("envmap");

        // Build the shared lights array from whichever light_N ports are
        // actually wired (unwired slots simply don't contribute — 0
        // lights is a valid scene state, matched by ambient + emission
        // only). Sun-style negated-direction convention, matching
        // render_3d_mesh's existing (non-per-pixel-Point) derivation.
        let mut lights_uniform = [[0.0f32; 4]; MAX_LIGHTS * 2];
        let mut light_count: u32 = 0;
        for &name in &LIGHT_NAMES[..lights_n] {
            if let Some(l) = ctx.inputs.light(name) {
                let idx = light_count as usize;
                lights_uniform[idx * 2] = [-l.dir[0], -l.dir[1], -l.dir[2], 1.0];
                lights_uniform[idx * 2 + 1] = [l.color[0], l.color[1], l.color[2], 0.0];
                light_count += 1;
            }
        }

        let Some(dims_tex) = ctx.outputs.texture_2d("color") else {
            return;
        };
        let width = dims_tex.width;
        let height = dims_tex.height;
        if width == 0 || height == 0 {
            return;
        }
        let aspect = width as f32 / height as f32;
        let view_proj = cam.view_proj(aspect);

        // ---- Pass 1 (mutable phase): validate every object's required
        // inputs, compose its model matrix + uniforms, and get-or-compile
        // its pipeline. Structured error + magenta clear + return on the
        // first unmet requirement (no-silent-fallbacks, matching
        // render_mesh / render_copies).
        struct ObjectDraw<'ctx> {
            vertices: &'ctx manifold_gpu::GpuBuffer,
            uniforms: RenderSceneUniforms,
            pipeline: manifold_gpu::GpuRenderPipeline,
        }

        let mut draws: Vec<ObjectDraw<'ctx>> = Vec::with_capacity(objects);

        for n in 0..objects {
            let Some(vertices) = ctx.inputs.array(MESH_NAMES[n]) else {
                ctx.error(format!(
                    "missing required `{}` input; renderer fell back to magenta clear",
                    MESH_NAMES[n]
                ));
                if let Some(target) = ctx.outputs.texture_2d("color") {
                    let gpu = ctx.gpu_encoder();
                    gpu.native_enc.clear_texture(target, 1.0, 0.0, 1.0, 1.0);
                }
                return;
            };
            let Some(material) = ctx.inputs.material(MATERIAL_NAMES[n]) else {
                ctx.error(format!(
                    "missing required `{}` input; renderer fell back to magenta clear",
                    MATERIAL_NAMES[n]
                ));
                if let Some(target) = ctx.outputs.texture_2d("color") {
                    let gpu = ctx.gpu_encoder();
                    gpu.native_enc.clear_texture(target, 1.0, 0.0, 1.0, 1.0);
                }
                return;
            };
            if material.requires_envmap() && envmap_wired.is_none() {
                ctx.error(format!(
                    "{:?} material on `{}` requires `envmap` input but it is unwired; renderer fell back to magenta",
                    material.kind, MATERIAL_NAMES[n]
                ));
                if let Some(target) = ctx.outputs.texture_2d("color") {
                    let gpu = ctx.gpu_encoder();
                    gpu.native_enc.clear_texture(target, 1.0, 0.0, 1.0, 1.0);
                }
                return;
            }

            let pos = [
                ctx.params.get(POS_X_NAMES[n]).and_then(|v| v.as_scalar()).unwrap_or(0.0),
                ctx.params.get(POS_Y_NAMES[n]).and_then(|v| v.as_scalar()).unwrap_or(0.0),
                ctx.params.get(POS_Z_NAMES[n]).and_then(|v| v.as_scalar()).unwrap_or(0.0),
            ];
            let rot = [
                ctx.params.get(ROT_X_NAMES[n]).and_then(|v| v.as_scalar()).unwrap_or(0.0),
                ctx.params.get(ROT_Y_NAMES[n]).and_then(|v| v.as_scalar()).unwrap_or(0.0),
                ctx.params.get(ROT_Z_NAMES[n]).and_then(|v| v.as_scalar()).unwrap_or(0.0),
            ];
            let scale = [
                ctx.params.get(SCALE_X_NAMES[n]).and_then(|v| v.as_scalar()).unwrap_or(1.0),
                ctx.params.get(SCALE_Y_NAMES[n]).and_then(|v| v.as_scalar()).unwrap_or(1.0),
                ctx.params.get(SCALE_Z_NAMES[n]).and_then(|v| v.as_scalar()).unwrap_or(1.0),
            ];
            let model = model_matrix(pos, rot, scale);
            let uniforms = build_uniforms(
                view_proj,
                model,
                &cam,
                &material,
                light_count as f32,
                lights_uniform,
            );

            let pipeline = {
                let gpu = ctx.gpu_encoder();
                self.pipeline_for(gpu.device, material.kind).clone()
            };

            draws.push(ObjectDraw {
                vertices,
                uniforms,
                pipeline,
            });
        }

        if draws.is_empty() {
            return;
        }

        // ---- Ensure cached GPU resources (mutable phase). ----
        {
            let gpu = ctx.gpu_encoder();
            if self.depth_stencil.is_none() {
                self.depth_stencil = Some(gpu.device.create_depth_stencil_state(
                    &manifold_gpu::GpuDepthStencilDesc {
                        compare: manifold_gpu::GpuCompareFunction::Less,
                        write_enabled: true,
                    },
                ));
            }
            self.ensure_depth_texture(gpu.device, width, height);
            self.ensure_sampler(gpu.device);
            self.ensure_dummy_texture(gpu.device);
        }

        // ---- Pass 2 (immutable phase): draw. Object 0 clears the
        // shared color+depth target; every subsequent object loads onto
        // it, so the depth test resolves real occlusion between objects.
        let Some(target) = ctx.outputs.texture_2d("color") else {
            return;
        };
        let depth_stencil = self.depth_stencil.as_ref().expect("just inserted");
        let depth_tex = self.depth_texture.as_ref().expect("just inserted");
        let sampler = self.sampler.as_ref().expect("just inserted");
        let dummy = self.dummy_texture.as_ref().expect("just inserted");
        let envmap_texture = envmap_wired.unwrap_or(dummy);

        let gpu = ctx.gpu_encoder();
        let mut drew_any = false;
        for draw in &draws {
            let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
            let vertex_capacity = (draw.vertices.size / vertex_size) as u32;
            let vertex_count = (vertex_capacity / 3) * 3;
            if vertex_count == 0 {
                continue;
            }
            let bindings = [
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&draw.uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: draw.vertices,
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
                    texture: dummy,
                },
                GpuBinding::Texture {
                    binding: 5,
                    texture: dummy,
                },
                GpuBinding::Texture {
                    binding: 6,
                    texture: dummy,
                },
                GpuBinding::Texture {
                    binding: 7,
                    texture: dummy,
                },
            ];
            let load_action = if drew_any {
                GpuLoadAction::Load
            } else {
                GpuLoadAction::Clear
            };
            gpu.native_enc.draw_instanced_depth(
                &draw.pipeline,
                target,
                depth_tex,
                depth_stencil,
                &bindings,
                vertex_count,
                1,
                load_action,
                "node.render_scene",
            );
            drew_any = true;
        }
        if !drew_any {
            // Every object had zero drawable vertices — clear so stale
            // pool contents don't leak through (matches render_mesh's
            // vertex_count == 0 fallback).
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 0.0);
        }
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: RENDER_SCENE_TYPE_ID,
        create: || Box::new(RenderScene::new()),
        picker: Some(crate::node_graph::palette::PickerInfo {
            label: "Render Scene",
            category: crate::node_graph::palette::PaletteCategory::Atom,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params_with(objects: f32, lights: f32) -> ParamValues {
        let mut p = ParamValues::default();
        p.insert("objects", ParamValue::Float(objects));
        p.insert("lights", ParamValue::Float(lights));
        p
    }

    #[test]
    fn defaults_to_two_objects_one_light() {
        let s = RenderScene::new();
        // camera + envmap + light_0 + (mesh_0,material_0) + (mesh_1,material_1)
        assert_eq!(s.inputs().len(), 2 + 1 + 4);
        assert!(s.inputs().iter().any(|p| p.name == "mesh_1"));
        assert!(s.inputs().iter().any(|p| p.name == "material_1"));
        assert!(!s.inputs().iter().any(|p| p.name == "mesh_2"));
        assert!(s.inputs().iter().any(|p| p.name == "light_0"));
        assert!(!s.inputs().iter().any(|p| p.name == "light_1"));
        // objects + lights + 2 objects * 9 transform params
        assert_eq!(s.parameters().len(), 2 + 2 * 9);
    }

    #[test]
    fn reconfigure_grows_and_shrinks_ports_and_params() {
        let mut s = RenderScene::new();
        let node: &mut dyn EffectNode = &mut s;
        node.reconfigure(&params_with(5.0, 3.0));
        assert!(node.inputs().iter().any(|p| p.name == "mesh_4"));
        assert!(node.inputs().iter().any(|p| p.name == "material_4"));
        assert!(!node.inputs().iter().any(|p| p.name == "mesh_5"));
        assert!(node.inputs().iter().any(|p| p.name == "light_2"));
        assert!(!node.inputs().iter().any(|p| p.name == "light_3"));
        assert!(node.parameters().iter().any(|p| p.name == "pos_x_4"));

        node.reconfigure(&params_with(1.0, 0.0));
        assert!(!node.inputs().iter().any(|p| p.name == "mesh_1"));
        assert!(!node.inputs().iter().any(|p| p.name == "light_0"));
        assert!(node.inputs().iter().any(|p| p.name == "mesh_0"));
    }

    #[test]
    fn reconfigure_clamps_to_caps() {
        let mut s = RenderScene::new();
        let node: &mut dyn EffectNode = &mut s;
        node.reconfigure(&params_with(999.0, 999.0));
        assert!(node.inputs().iter().any(|p| p.name == "mesh_7"));
        assert!(!node.inputs().iter().any(|p| p.name == "mesh_8"));
        assert!(node.inputs().iter().any(|p| p.name == "light_3"));
    }

    #[test]
    fn camera_and_mesh_material_ports_are_required_envmap_and_lights_are_not() {
        let s = RenderScene::new();
        let by_name = |n: &str| s.inputs().iter().find(|p| p.name == n).unwrap();
        assert!(by_name("camera").required);
        assert!(!by_name("envmap").required);
        assert!(!by_name("light_0").required);
        assert!(by_name("mesh_0").required);
        assert!(by_name("material_0").required);
    }

    #[test]
    fn registers_with_palette_type_id() {
        let s = RenderScene::new();
        let node: &dyn EffectNode = &s;
        assert_eq!(node.type_id().as_str(), "node.render_scene");
    }

    #[test]
    fn model_matrix_identity_at_origin_no_rotation_unit_scale() {
        let m = model_matrix([0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let expected = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        for c in 0..4 {
            for r in 0..4 {
                assert!(
                    (m[c][r] - expected[c][r]).abs() < 1e-6,
                    "col {c} row {r}: got {} expected {}",
                    m[c][r],
                    expected[c][r]
                );
            }
        }
    }

    #[test]
    fn model_matrix_translates_a_point() {
        let m = model_matrix([2.0, 3.0, 4.0], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        // Column 3 (translation) should carry the position.
        assert_eq!(m[3], [2.0, 3.0, 4.0, 1.0]);
    }

    #[test]
    fn model_matrix_scales_columns_independently() {
        let m = model_matrix([0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [2.0, 3.0, 4.0]);
        assert!((m[0][0] - 2.0).abs() < 1e-6);
        assert!((m[1][1] - 3.0).abs() < 1e-6);
        assert!((m[2][2] - 4.0).abs() < 1e-6);
    }
}
