//! Reconstruct a normalized particle density volume from the linked cell bins.

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::WaterParticle;
use manifold_gpu::GpuBinding;
use std::borrow::Cow;

pub fn shader_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<WaterDensityField>()
        .expect("node.water_density_field codegen")
}

pub fn prewarm_pipeline(device: &manifold_gpu::GpuDevice) {
    let _ = device.create_compute_pipeline(&shader_source(), "cs_main", "node.water_density_field");
}

crate::primitive! {
    name: WaterDensityField,
    type_id: "node.water_density_field",
    purpose: "Reconstruct normalized water density and foam from bounded particle bins into an RGBA16Float Texture3D using the compact poly6 kernel.",
    inputs: {
        particles: Array(WaterParticle) required,
        heads: Array(u32) required,
        next: Array(u32) required,
        foam: Array(f32) required,
        radius: ScalarF32 optional,
    },
    outputs: { density: Texture3D },
    params: [
        ParamDef { name: Cow::Borrowed("vol_res"), label: "Volume Resolution", ty: ParamType::Int, default: ParamValue::Float(128.0), range: Some((16.0, 512.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("vol_depth"), label: "Volume Depth", ty: ParamType::Int, default: ParamValue::Float(128.0), range: Some((16.0, 512.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("radius"), label: "Kernel Radius", ty: ParamType::Float, default: ParamValue::Float(0.10), range: Some((0.0625, 0.125)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Uses node.water_particle_bins' fixed 32³ linked cells over origin [-2,0,-2] and extent 4m. Output dimensions follow vol_res × vol_res × vol_depth; radius is port-shadowed.",
    examples: [],
    picker: { label: "Water Density Field", category: Atom },
    summary: "Builds a normalized volumetric density field and foam channel from binned water particles.",
    category: Particles3D,
    role: Filter,
    aliases: ["water density", "density field"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/water_density_field_body.wgsl"),
    input_access: [BufferIndex, BufferIndex, BufferIndex, BufferIndex],
}

impl Primitive for WaterDensityField {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(heads) = ctx.inputs.array("heads") else {
            return;
        };
        let Some(next) = ctx.inputs.array("next") else {
            return;
        };
        let Some(foam) = ctx.inputs.array("foam") else {
            return;
        };
        let Some(density) = ctx.outputs.texture_3d("density") else {
            return;
        };
        let radius = ctx.scalar_or_param("radius", 0.10);
        if !radius.is_finite() || !(0.0625..=0.125).contains(&radius) || particles.size < 96 {
            ctx.error("node.water_density_field: invalid radius or particle storage");
            return;
        }
        if heads.size < 32768 * 4
            || next.size < particles.size / 96 * 4
            || foam.size < particles.size / 96 * 4
        {
            ctx.error("node.water_density_field: insufficient linked-bin or foam capacity");
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = shader_source();
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.water_density_field",
            )
        });
        let uniforms = [density.width, density.depth, radius.to_bits(), 0_u32];
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: heads,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: next,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: foam,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 5,
                    texture: density,
                },
            ],
            [
                density
                    .width
                    .div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
                density
                    .height
                    .div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
                density
                    .depth
                    .div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
            ],
            "node.water_density_field",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn density_generated_shader_validates() {
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
    fn density_gpu_matches_normalized_kernel_and_foam() {
        use manifold_gpu::{
            GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
        };
        let device = crate::test_device();
        let pipeline =
            device.create_compute_pipeline(&shader_source(), "cs_main", "density-oracle");
        let n = 32u32;
        let out = device.create_texture(&GpuTextureDesc {
            width: n,
            height: n,
            depth: n,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D3,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "density-oracle",
            mip_levels: 1,
        });
        let center = [0.0625f32, 0.5625, 0.0625];
        let make = |pos: [f32; 3], volume: f32| {
            let mut p: WaterParticle = bytemuck::Zeroable::zeroed();
            p.position_mass = [pos[0], pos[1], pos[2], volume * 1000.0];
            p
        };
        let mut lattice = Vec::new();
        for z in -3..=3 {
            for y in -3..=3 {
                for x in -3..=3 {
                    lattice.push(make(
                        [
                            center[0] + x as f32 * 0.04,
                            center[1] + y as f32 * 0.04,
                            center[2] + z as f32 * 0.04,
                        ],
                        0.04f32.powi(3),
                    ));
                }
            }
        }
        let fixtures = [
            vec![
                make(center, 0.0001),
                make([center[0] + 0.04, center[1], center[2]], 0.0002),
            ],
            vec![make(center, 0.0)],
            lattice,
        ];
        for (case, particles) in fixtures.iter().enumerate() {
            let mut heads = vec![0u32; 32768];
            let mut next = vec![0u32; particles.len()];
            let foam: Vec<f32> = (0..particles.len())
                .map(|i| if i % 2 == 0 { 0.25 } else { 1.0 })
                .collect();
            for (i, p) in particles.iter().enumerate() {
                if p.position_mass[3] == 0.0 {
                    continue;
                }
                let x = ((p.position_mass[0] + 2.0) / 0.125).floor() as usize;
                let y = (p.position_mass[1] / 0.125).floor() as usize;
                let z = ((p.position_mass[2] + 2.0) / 0.125).floor() as usize;
                let bin = (z * 32 + y) * 32 + x;
                next[i] = heads[bin];
                heads[bin] = i as u32 + 1;
            }
            let upload = |bytes: &[u8]| {
                let b = device.create_buffer_shared(bytes.len() as u64);
                unsafe {
                    b.write(0, bytes);
                }
                b
            };
            let pbuf = upload(bytemuck::cast_slice(particles));
            let hbuf = upload(bytemuck::cast_slice(&heads));
            let nbuf = upload(bytemuck::cast_slice(&next));
            let fbuf = upload(bytemuck::cast_slice(&foam));
            let uniforms = [n, n, 0.1f32.to_bits(), 0u32];
            let mut enc = device.create_encoder("density-oracle");
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::cast_slice(&uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: &pbuf,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: &hbuf,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: &nbuf,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 4,
                        buffer: &fbuf,
                        offset: 0,
                    },
                    GpuBinding::Texture {
                        binding: 5,
                        texture: &out,
                    },
                ],
                [8, 8, 8],
                "density-oracle",
            );
            let rb = device.create_buffer_shared(u64::from(n * n * n * 8));
            enc.copy_texture_3d_to_buffer(&out, &rb, n, n, n, n * 8);
            enc.commit_and_wait_completed();
            let values = unsafe {
                std::slice::from_raw_parts(
                    rb.mapped_ptr().unwrap().cast::<u16>(),
                    (n * n * n * 4) as usize,
                )
            };
            for z in 0..n {
                for y in 0..n {
                    for x in 0..n {
                        let pos = [
                            -2.0 + (x as f64 + 0.5) * 4.0 / n as f64,
                            (y as f64 + 0.5) * 4.0 / n as f64,
                            -2.0 + (z as f64 + 0.5) * 4.0 / n as f64,
                        ];
                        let mut rho = 0.0;
                        let mut fs = 0.0;
                        for (p, f) in particles.iter().zip(&foam) {
                            let d2: f64 = (0..3)
                                .map(|a| (pos[a] - p.position_mass[a] as f64).powi(2))
                                .sum();
                            let weight = (p.position_mass[3] as f64 / 1000.0) * 315.0
                                / (64.0 * std::f64::consts::PI * 0.1f64.powi(3))
                                * (1.0 - d2 / 0.01).max(0.0).powi(3);
                            rho += weight;
                            fs += weight * (*f as f64);
                        }
                        let idx = ((z * n + y) * n + x) as usize * 4;
                        let actual = half::f16::from_bits(values[idx]).to_f64();
                        let actual_foam = half::f16::from_bits(values[idx + 1]).to_f64();
                        assert!(
                            (actual - rho).abs() < 0.003,
                            "case{case} voxel{x},{y},{z}: {actual} vs {rho}"
                        );
                        if rho > 1e-6 {
                            assert!((actual_foam - fs / rho).abs() < 0.003);
                        } else {
                            assert_eq!(actual, 0.0);
                            assert_eq!(actual_foam, 0.0);
                        }
                        if case == 2 && [x, y, z] == [16, 4, 16] {
                            assert!(
                                (actual - 1.0).abs() < 0.03,
                                "interior normalized density {actual}"
                            );
                        }
                    }
                }
            }
        }
    }
}
