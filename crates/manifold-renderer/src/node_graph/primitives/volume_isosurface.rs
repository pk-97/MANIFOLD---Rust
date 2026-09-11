//! Bounded raycast of the water density volume into screen-space surface fields.

use std::borrow::Cow;

use crate::node_graph::camera::{Camera, CameraMode};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::{Primitive, PrimitiveSpec};
use manifold_gpu::{GpuBinding, GpuSamplerDesc, GpuTextureFormat};

use super::CUBE_HALF;

crate::primitive! {
    name: VolumeIsosurface,
    type_id: "node.volume_isosurface",
    purpose: "Bounded screen-space raycast of a Texture3D density field. Finds all isovalue crossings in the fixed four-metre water volume, sums disjoint inside intervals for thickness, and emits depth, normals, coverage, and foam in one pass.",
    inputs: {
        density: Texture3D required,
        camera: Camera required,
        collider: Transform optional,
        isovalue: ScalarF32 optional,
        step_scale: ScalarF32 optional,
        cube_half_x: ScalarF32 optional,
        cube_half_y: ScalarF32 optional,
        cube_half_z: ScalarF32 optional,
    },
    outputs: {
        depth: Texture2D,
        thickness: Texture2D,
        normals: Texture2D,
        coverage: Texture2D,
        foam: Texture2D,
    },
    params: [
        ParamDef { name: Cow::Borrowed("isovalue"), label: "Isovalue", ty: ParamType::Float, default: ParamValue::Float(0.5), range: Some((0.05, 2.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("step_scale"), label: "Step Scale", ty: ParamType::Float, default: ParamValue::Float(0.75), range: Some((0.5, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cube_half_x"), label: "Collider Half X", ty: ParamType::Float, default: ParamValue::Float(CUBE_HALF[0]), range: Some((0.001, 2.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cube_half_y"), label: "Collider Half Y", ty: ParamType::Float, default: ParamValue::Float(CUBE_HALF[1]), range: Some((0.001, 2.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cube_half_z"), label: "Collider Half Z", ty: ParamType::Float, default: ParamValue::Float(CUBE_HALF[2]), range: Some((0.001, 2.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Full-canvas output. Density is normalized in the fixed water box [-2,0,-2] to [2,4,2]; all five outputs are Rgba32Float so downstream water shading can retain physical thickness and gradients.",
    examples: [],
    picker: { label: "Volume Isosurface", category: Atom },
    summary: "Turns a 3D density field into a bounded camera-facing liquid surface.",
    category: Geometry3D,
    role: Filter,
    aliases: ["volume isosurface", "density raycast", "water surface"],
    fusion_kind: Boundary,
    // BUG-5mqt: Transform-derived values cannot be recomputed by the fusion registry.
    // Still emitted through standalone codegen; the graph retains this explicit boundary.
    boundary_reason: Blocked,
    wgsl_body: include_str!("shaders/volume_isosurface_body.wgsl"),
    input_access: [Gather],
    derived_uniforms: ["cam_pos:vec3", "cam_fwd:vec3", "cam_right:vec3", "cam_up:vec3", "fov_y", "near", "far", "collider_center:vec3", "collider_enabled:u32"],
}

pub fn shader_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_boundary_spec::<VolumeIsosurface>()
        .expect("node.volume_isosurface codegen")
        .replace(
            "texture_storage_2d<rgba16float",
            "texture_storage_2d<rgba32float",
        )
}

pub fn prewarm_pipeline(device: &manifold_gpu::GpuDevice) {
    let _ = device.create_compute_pipeline(&shader_source(), "cs_main", "node.volume_isosurface");
}

fn uniform_words(
    cam: &Camera,
    controls: [f32; 5],
    collider: Option<[f32; 3]>,
    writes: [bool; 5],
) -> [u32; 32] {
    let mut words = [0u32; 32];
    let fov = match cam.mode {
        CameraMode::Perspective { fov_y } => fov_y,
        _ => 0.0,
    };
    let basis = [cam.pos, cam.fwd, cam.right, cam.up];
    for (dst, value) in words[..5].iter_mut().zip(controls) {
        *dst = value.to_bits();
    }
    for (dst, value) in words[5..17].iter_mut().zip(basis.into_iter().flatten()) {
        *dst = value.to_bits();
    }
    for (dst, value) in words[17..20].iter_mut().zip([fov, cam.near, cam.far]) {
        *dst = value.to_bits();
    }
    for (dst, value) in words[20..23].iter_mut().zip(collider.unwrap_or([0.0; 3])) {
        *dst = value.to_bits();
    }
    words[23] = u32::from(collider.is_some());
    for (dst, write) in words[24..29].iter_mut().zip(writes) {
        *dst = u32::from(write);
    }
    words
}

impl Primitive for VolumeIsosurface {
    fn output_canvas_scale(
        &self,
        port: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        VolumeIsosurface::OUTPUTS
            .iter()
            .find(|p| p.name == port)
            .map(|_| (1, 1))
    }
    fn output_format(&self, port: &str) -> Option<GpuTextureFormat> {
        VolumeIsosurface::OUTPUTS
            .iter()
            .find(|p| p.name == port)
            .map(|_| GpuTextureFormat::Rgba32Float)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(volume) = ctx.inputs.texture_3d("density") else {
            return;
        };
        let slots = [
            ctx.outputs.texture_2d("depth"),
            ctx.outputs.texture_2d("thickness"),
            ctx.outputs.texture_2d("normals"),
            ctx.outputs.texture_2d("coverage"),
            ctx.outputs.texture_2d("foam"),
        ];
        let Some(primary) = slots.iter().flatten().next().copied() else {
            return;
        };
        let [depth, thickness, normals, coverage, foam] = slots.map(|s| s.unwrap_or(primary));
        let (w, h) = (primary.width, primary.height);
        let Some(cam) = ctx.inputs.camera("camera") else {
            ctx.error("node.volume_isosurface: camera required");
            return;
        };
        let controls = [
            ctx.scalar_or_param("isovalue", 0.5),
            ctx.scalar_or_param("step_scale", 0.75),
            ctx.scalar_or_param("cube_half_x", CUBE_HALF[0]),
            ctx.scalar_or_param("cube_half_y", CUBE_HALF[1]),
            ctx.scalar_or_param("cube_half_z", CUBE_HALF[2]),
        ];
        let collider = ctx.inputs.transform("collider").map(|t| t.pos);
        let valid_controls = controls.iter().all(|v| v.is_finite())
            && (0.05..=2.0).contains(&controls[0])
            && (0.5..=1.0).contains(&controls[1])
            && controls[2..].iter().all(|v| *v > 0.0);
        let valid_camera = matches!(cam.mode, CameraMode::Perspective { fov_y } if fov_y.is_finite() && fov_y > 0.0 && fov_y < 3.13)
            && cam.near.is_finite()
            && cam.far.is_finite()
            && cam.near > 0.0
            && cam.far > cam.near
            && [cam.pos, cam.fwd, cam.right, cam.up]
                .into_iter()
                .flatten()
                .all(|v| v.is_finite());
        if !valid_controls
            || !valid_camera
            || collider.is_some_and(|c| c.iter().any(|v| !v.is_finite()))
        {
            ctx.error("node.volume_isosurface: invalid camera, threshold, step or collider");
            return;
        }
        let params = uniform_words(&cam, controls, collider, slots.map(|s| s.is_some()));
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &shader_source(),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.volume_isosurface",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::cast_slice(&params),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: volume,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: depth,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: thickness,
                },
                GpuBinding::Texture {
                    binding: 5,
                    texture: normals,
                },
                GpuBinding::Texture {
                    binding: 6,
                    texture: coverage,
                },
                GpuBinding::Texture {
                    binding: 7,
                    texture: foam,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.volume_isosurface",
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isosurface_generated_shader_validates() {
        let source = shader_source();
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
    }
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn isosurface_gpu_slab_empty_disjoint_and_solid() {
        use manifold_gpu::{GpuTextureDesc, GpuTextureDimension, GpuTextureUsage};
        let device = crate::test_device();
        let fill = device.create_compute_pipeline(
            r#"
struct U{mode:u32,p0:u32,p1:u32,p2:u32}
@group(0) @binding(0) var<uniform> u:U;
@group(0) @binding(1) var out:texture_storage_3d<rgba16float,write>;
@compute @workgroup_size(4,4,4)
fn cs_main(@builtin(global_invocation_id) id:vec3<u32>){
 let z=-2.0+(f32(id.z)+0.5)*4.0/32.0;
 var rho=0.0;
 if(u.mode==1u && z > -1.0 && z < 1.0){rho=1.0;}
 if(u.mode==2u && ((z > -1.0 && z < -0.5)||(z > 0.5 && z < 1.0))){rho=1.0;}
 if(u.mode==3u){rho=1.0;}
 textureStore(out,vec3<i32>(id),vec4<f32>(rho,0.4,0.0,0.0));
}"#,
            "cs_main",
            "iso-analytic-field",
        );
        let volume = device.create_texture(&GpuTextureDesc {
            width: 32,
            height: 32,
            depth: 32,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D3,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "iso-field",
            mip_levels: 1,
        });
        let outputs: Vec<_> = (0..5)
            .map(|_| {
                device.create_texture(&GpuTextureDesc {
                    width: 17,
                    height: 17,
                    depth: 1,
                    format: GpuTextureFormat::Rgba32Float,
                    dimension: GpuTextureDimension::D2,
                    usage: GpuTextureUsage::RENDER_TARGET_FULL,
                    label: "iso-out",
                    mip_levels: 1,
                })
            })
            .collect();
        let pipeline = device.create_compute_pipeline(&shader_source(), "cs_main", "iso-oracle");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let mut cam = Camera::default_perspective();
        cam.pos = [0.0, 2.0, -4.0];
        cam.fwd = [0.0, 0.0, 1.0];
        cam.right = [1.0, 0.0, 0.0];
        cam.up = [0.0, 1.0, 0.0];
        cam.near = 0.1;
        cam.far = 20.0;
        let run = |mode: u32, solid: Option<[f32; 3]>, cam: &Camera| {
            let mut enc = device.create_encoder("iso-proof");
            let fill_u = [mode, 0, 0, 0];
            enc.dispatch_compute(
                &fill,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::cast_slice(&fill_u),
                    },
                    GpuBinding::Texture {
                        binding: 1,
                        texture: &volume,
                    },
                ],
                [8, 8, 8],
                "iso-fill",
            );
            let words = uniform_words(cam, [0.5, 0.75, 0.5, 0.5, 0.35], solid, [true; 5]);
            let mut bindings = vec![
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::cast_slice(&words),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: &volume,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler: &sampler,
                },
            ];
            for (i, out) in outputs.iter().enumerate() {
                bindings.push(GpuBinding::Texture {
                    binding: i as u32 + 3,
                    texture: out,
                });
            }
            enc.dispatch_compute(&pipeline, &bindings, [2, 2, 1], "iso-proof");
            let rb: Vec<_> = (0..5)
                .map(|_| device.create_buffer_shared(17 * 17 * 16))
                .collect();
            for (out, b) in outputs.iter().zip(&rb) {
                enc.copy_texture_to_buffer(out, b, 17, 17, 17 * 16);
            }
            enc.commit_and_wait_completed();
            rb.iter()
                .map(|b| {
                    let raw = unsafe {
                        std::slice::from_raw_parts(
                            b.mapped_ptr().unwrap().cast::<f32>(),
                            17 * 17 * 4,
                        )
                    };
                    let idx = (8 * 17 + 8) * 4;
                    [raw[idx], raw[idx + 1], raw[idx + 2], raw[idx + 3]]
                })
                .collect::<Vec<_>>()
        };
        let empty = run(0, None, &cam);
        assert_eq!(empty[0][0], 1.0);
        assert_eq!(empty[1][0], 0.0);
        assert_eq!(empty[3][0], 0.0);
        let slab = run(1, None, &cam);
        let raw = 20.0 / (0.1 - 20.0) * (0.1 / 3.0 - 1.0);
        assert!((slab[0][0] - raw).abs() < 0.0001, "depth {:?}", slab[0]);
        assert!((slab[1][0] - 2.0).abs() < 0.008, "thickness {:?}", slab[1]);
        assert!(slab[2][2] < -0.99);
        assert!((slab[4][0] - 0.4).abs() < 0.002);
        let split = run(2, None, &cam);
        assert!(
            (split[1][0] - 1.0).abs() < 0.01,
            "air gaps count as water: {:?}",
            split[1]
        );
        // Box z=[-1.2,-0.5] removes the front 0.5m from the [-1,1] slab.
        let solid = run(1, Some([0.0, 2.0, -0.85]), &cam);
        let raw_solid = 20.0 / (0.1 - 20.0) * (0.1 / 3.5 - 1.0);
        assert!((solid[0][0] - raw_solid).abs() < 0.0001);
        assert!((solid[1][0] - 1.5).abs() < 0.008);
        assert!(solid[2][2] < -0.99);
        cam.pos[0] = 3.0;
        let miss = run(3, None, &cam);
        assert_eq!(miss[3][0], 0.0, "parallel ray outside box must miss");
        cam.pos = [0.0, 2.0, 0.0];
        let inside = run(3, None, &cam);
        assert!((inside[1][0] - 1.9).abs() < 0.008, "camera inside volume");
    }
}
