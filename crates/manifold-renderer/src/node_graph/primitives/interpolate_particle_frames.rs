//! `node.interpolate_particle_frames` — display-time interpolation at the
//! particle-frame seam (GPU_FLUID_SURFACE_DESIGN.md D3/D10/D11).
//!
//! The output is one record for every slot in frame B.  A's sorted,
//! non-zero particle ids are searched for a matching B id; matches use cubic
//! Hermite interpolation, while births are rewound from B by the remaining
//! part of the tick and grow in with radius × blend.  An unwired or empty A
//! frame is an intentional empty-A path (used by whitewater): its particles
//! are rewound the same way but keep their radius, because nothing was born.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: the scalar params in declaration order,
/// then the injected dispatch count.  All seam metadata is carried as f32
/// scalars so it can be port-shadowed by the frame producer; count and epoch
/// are integer values represented exactly in the f32 range used by the seam.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InterpolationUniforms {
    count_a: f32,
    count_b: f32,
    identity_a: f32,
    identity_b: f32,
    blend: f32,
    span: f32,
    acceleration_x: f32,
    acceleration_y: f32,
    acceleration_z: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: InterpolateParticleFrames,
    type_id: "node.interpolate_particle_frames",
    purpose: "Present frame B's liquid particles at display time. Matching nonzero ids in the same identity epoch use cubic Hermite interpolation with span-scaled endpoint velocities; births are rewound from B by the remaining time and grow in with radius × blend. The output has one slot per particles_b record. An unwired or empty particles_a is an intentional empty-A path: every B particle is rewound like a birth but keeps its radius.",
    inputs: {
        particles_a: Array(FluidParticle) optional,
        particles_b: Array(FluidParticle) required,
        count_a: ScalarF32 optional,
        count_b: ScalarF32 optional,
        identity_a: ScalarF32 optional,
        identity_b: ScalarF32 optional,
        blend: ScalarF32 optional,
        span: ScalarF32 optional,
        acceleration_x: ScalarF32 optional,
        acceleration_y: ScalarF32 optional,
        acceleration_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("count_a"),
            label: "Frame A Count",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 16_777_216.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("count_b"),
            label: "Frame B Count",
            ty: ParamType::Float,
            default: ParamValue::Float(-1.0),
            range: Some((-1.0, 16_777_216.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("identity_a"),
            label: "Frame A Identity",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 16_777_216.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("identity_b"),
            label: "Frame B Identity",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 16_777_216.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("blend"),
            label: "Blend",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("span"),
            label: "Frame Span",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("acceleration_x"),
            label: "Acceleration X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("acceleration_y"),
            label: "Acceleration Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("acceleration_z"),
            label: "Acceleration Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire particles_a/b, count_a/b, identity_a/b, blend and span from node.fluid_surface. Frames are sorted by strictly increasing nonzero id within an identity epoch. A matching id uses cubic Hermite position and derivative velocity; an unmatched B birth uses x_B − τ·v_B + ½·a·τ², v_B − a·τ and radius × blend, where τ = (1 − blend)·span. blend is clamped to 0..1 and span to >=0 in the body, so display time never extrapolates past B. Leave particles_a unwired for the deliberate empty-A path; leave count_b at its -1 default to process the whole B capacity, whose unused slots are already radius 0.",
    examples: [],
    picker: { label: "Interpolate Particle Frames", category: Atom },
    summary: "Smooths a particle simulation between two accepted frames while letting new particles grow in naturally.",
    category: Particles3D,
    role: Filter,
    aliases: ["particle interpolation", "Hermite particles", "frame blend"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/interpolate_particle_frames_body.wgsl"),
    input_access: [BufferGather, Coincident],
    output_capacity: FusedOutputCapacity::FromInput { input: "particles_b" },
}

impl Primitive for InterpolateParticleFrames {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "particles_b")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(particles_b) = ctx.inputs.array("particles_b") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let particle_size = std::mem::size_of::<FluidParticle>() as u64;
        let count = (particles_b.size / particle_size).min(out.size / particle_size) as u32;
        if count == 0 {
            return;
        }

        // BufferGather still needs a valid binding when A is unwired. B is a
        // safe dummy because the empty-A count is zero, so the body never
        // reads it. This keeps the standalone and fused math identical.
        let particles_a_input = ctx.inputs.array("particles_a");
        let particles_a = particles_a_input.unwrap_or(particles_b);
        let uniforms = InterpolationUniforms {
            count_a: particles_a_input
                .map(|_| ctx.scalar_or_param("count_a", 0.0))
                .unwrap_or(0.0),
            count_b: ctx.scalar_or_param("count_b", -1.0),
            identity_a: ctx.scalar_or_param("identity_a", 0.0),
            identity_b: ctx.scalar_or_param("identity_b", 0.0),
            blend: ctx.scalar_or_param("blend", 1.0),
            span: ctx.scalar_or_param("span", 0.0),
            acceleration_x: ctx.scalar_or_param("acceleration_x", 0.0),
            acceleration_y: ctx.scalar_or_param("acceleration_y", 0.0),
            acceleration_z: ctx.scalar_or_param("acceleration_z", 0.0),
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles_a,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: particles_b,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.interpolate_particle_frames",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::freeze::classify::{FusedOutputCapacity, FusionKind, InputAccess};
    use crate::node_graph::freeze::codegen::{ENTRY, standalone_for_spec};
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn interpolate_particle_frames_codegen_contract() {
        assert_eq!(
            InterpolateParticleFrames::TYPE_ID,
            "node.interpolate_particle_frames"
        );
        assert_eq!(
            InterpolateParticleFrames::FUSION_KIND,
            FusionKind::Pointwise
        );
        assert_eq!(
            InterpolateParticleFrames::INPUT_ACCESS,
            &[InputAccess::BufferGather, InputAccess::Coincident]
        );
        assert_eq!(
            InterpolateParticleFrames::FUSED_OUTPUT_CAPACITY,
            FusedOutputCapacity::FromInput {
                input: "particles_b"
            }
        );
        assert_eq!(InterpolateParticleFrames::PARAMS[0].name, "count_a");
        assert_eq!(
            InterpolateParticleFrames::PARAMS[0].default,
            ParamValue::Float(0.0)
        );
        assert_eq!(InterpolateParticleFrames::PARAMS[1].name, "count_b");
        assert_eq!(
            InterpolateParticleFrames::PARAMS[1].default,
            ParamValue::Float(-1.0)
        );
        let node = InterpolateParticleFrames::new();
        assert_eq!(
            node.array_output_capacity("out", &ParamValues::default(), &[("particles_b", 8)]),
            Some(8)
        );
        assert_eq!(
            node.array_output_capacity("out", &ParamValues::default(), &[("particles_a", 8)]),
            None
        );
        let wgsl = standalone_for_spec::<InterpolateParticleFrames>()
            .expect("standalone interpolation codegen");
        assert!(wgsl.contains("buf_particles_a"));
        assert!(wgsl.contains("buf_particles_b"));
        assert!(wgsl.contains("@compute"));
        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        assert_eq!(module.entry_points[0].name, ENTRY);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use crate::testkit::liquid_surface::{Harness, params, read};
    use super::*;
    use crate::node_graph::bindings::Slot;

    fn particle(position: [f32; 3], velocity: [f32; 3], radius: f32, id: u32) -> FluidParticle {
        FluidParticle {
            position_radius: [position[0], position[1], position[2], radius],
            velocity,
            id,
        }
    }

    fn zero_particle() -> FluidParticle {
        FluidParticle::default()
    }

    fn count_limit(value: f64, capacity: usize, negative_means_all: bool) -> usize {
        if value < 0.0 {
            return if negative_means_all { capacity } else { 0 };
        }
        value.min(capacity as f64).max(0.0) as usize
    }

    /// Independent f64 statement of D11. The GPU body is not called from this
    /// helper: the polynomial and birth path are re-derived here so the value
    /// proof can catch a mirrored shader mistake.
    fn cpu_expected(
        a: Option<&[FluidParticle]>,
        b: &[FluidParticle],
        counts: [f64; 2],
        identities: [f64; 2],
        blend: f64,
        span: f64,
        acceleration: [f64; 3],
    ) -> Vec<FluidParticle> {
        let [count_a, count_b] = counts;
        let [identity_a, identity_b] = identities;
        let a_slice = a.unwrap_or(&[]);
        let a_len = count_limit(count_a, a_slice.len(), false);
        let b_len = count_limit(count_b, b.len(), true);
        let t = blend.clamp(0.0, 1.0);
        let h = span.max(0.0);
        b.iter()
            .enumerate()
            .map(|(index, b)| {
                if index >= b_len || b.position_radius[3] <= 0.0 || b.position_radius[3].is_nan() {
                    return zero_particle();
                }
                let matched = if b.id == 0 || identity_a != identity_b {
                    None
                } else {
                    a_slice[..a_len]
                        .binary_search_by_key(&b.id, |p| p.id)
                        .ok()
                        .map(|i| a_slice[i])
                };
                if let Some(a) = matched {
                    if h == 0.0 {
                        return *b;
                    }
                    let t2 = t * t;
                    let t3 = t2 * t;
                    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
                    let h10 = t3 - 2.0 * t2 + t;
                    let h01 = -2.0 * t3 + 3.0 * t2;
                    let h11 = t3 - t2;
                    let position: [f64; 3] = std::array::from_fn(|axis| {
                        h00 * f64::from(a.position_radius[axis])
                            + h10 * h * f64::from(a.velocity[axis])
                            + h01 * f64::from(b.position_radius[axis])
                            + h11 * h * f64::from(b.velocity[axis])
                    });
                    let velocity: [f64; 3] = std::array::from_fn(|axis| {
                        let dh00 = (6.0 * t2 - 6.0 * t) / h;
                        let dh10 = 3.0 * t2 - 4.0 * t + 1.0;
                        let dh01 = (-6.0 * t2 + 6.0 * t) / h;
                        let dh11 = 3.0 * t2 - 2.0 * t;
                        dh00 * f64::from(a.position_radius[axis])
                            + dh10 * f64::from(a.velocity[axis])
                            + dh01 * f64::from(b.position_radius[axis])
                            + dh11 * f64::from(b.velocity[axis])
                    });
                    let radius = f64::from(a.position_radius[3])
                        + (f64::from(b.position_radius[3]) - f64::from(a.position_radius[3])) * t;
                    FluidParticle {
                        position_radius: [
                            position[0] as f32,
                            position[1] as f32,
                            position[2] as f32,
                            radius as f32,
                        ],
                        velocity: velocity.map(|value| value as f32),
                        id: b.id,
                    }
                } else {
                    let tau = (1.0 - t) * h;
                    let position: [f64; 3] = std::array::from_fn(|axis| {
                        f64::from(b.position_radius[axis]) - tau * f64::from(b.velocity[axis])
                            + 0.5 * acceleration[axis] * tau * tau
                    });
                    let velocity: [f64; 3] = std::array::from_fn(|axis| {
                        f64::from(b.velocity[axis]) - acceleration[axis] * tau
                    });
                    FluidParticle {
                        position_radius: [
                            position[0] as f32,
                            position[1] as f32,
                            position[2] as f32,
                            (f64::from(b.position_radius[3]) * if a_len > 0 { t } else { 1.0 }) as f32,
                        ],
                        velocity: velocity.map(|value| value as f32),
                        id: b.id,
                    }
                }
            })
            .collect()
    }

    fn run_case(
        harness: &mut Harness,
        node: &mut InterpolateParticleFrames,
        a: Option<&[FluidParticle]>,
        b: &[FluidParticle],
        values: &[(&'static str, f32)],
    ) -> Vec<FluidParticle> {
        let a_slot = a.map(|values| harness.array(values, values.len()).0);
        let (b_slot, _) = harness.array(b, b.len());
        let (out_slot, _) = harness.array::<FluidParticle>(&[], b.len());
        let mut inputs: Vec<(&'static str, Slot)> = Vec::with_capacity(2);
        if let Some(slot) = a_slot {
            inputs.push(("particles_a", slot));
        }
        inputs.push(("particles_b", b_slot));
        let (_, errors) = harness.run(node, &inputs, &[("out", out_slot)], &params(values));
        assert!(errors.is_empty(), "{errors:?}");
        read(&harness.buffer(out_slot), b.len())
    }

    fn assert_close(got: &[FluidParticle], want: &[FluidParticle], label: &str) {
        assert_eq!(got.len(), want.len(), "{label}: length");
        for (index, (got, want)) in got.iter().zip(want).enumerate() {
            assert_eq!(got.id, want.id, "{label}: particle {index} id");
            for axis in 0..4 {
                let error = (f64::from(got.position_radius[axis])
                    - f64::from(want.position_radius[axis]))
                .abs();
                assert!(
                    error < 2.0e-4,
                    "{label}: particle {index} position/radius axis {axis}: got {}, want {}, error {error}",
                    got.position_radius[axis],
                    want.position_radius[axis]
                );
            }
            for axis in 0..3 {
                let error = (f64::from(got.velocity[axis]) - f64::from(want.velocity[axis])).abs();
                assert!(
                    error < 2.0e-4,
                    "{label}: particle {index} velocity axis {axis}: got {}, want {}, error {error}",
                    got.velocity[axis],
                    want.velocity[axis]
                );
            }
        }
    }

    #[test]
    fn fluid_interpolate_particle_frames_matches_cpu_f64_reference() {
        let a = [
            particle([0.0, 0.5, -1.0], [1.0, 0.25, -0.5], 0.10, 1),
            particle([10.0, -2.0, 3.0], [-0.5, 1.0, 0.75], 0.20, 3),
            particle([20.0, 4.0, 6.0], [0.2, -0.4, 1.5], 0.30, 9),
        ];
        let b = [
            particle([1.0, 0.0, -1.5], [0.5, 0.75, -0.25], 0.15, 1),
            particle([4.0, 1.0, 2.0], [1.0, -0.5, 0.25], 0.40, 2),
            particle([2.0, 2.0, 2.0], [0.0, 0.0, 1.0], 0.50, 0),
            particle([11.0, -1.0, 4.0], [-0.25, 0.5, 0.5], 0.35, 3),
            particle([19.0, 5.0, 7.0], [0.4, -0.2, 1.25], 0.25, 9),
            particle([99.0, 99.0, 99.0], [9.0, 9.0, 9.0], 0.0, 77),
        ];
        let mut harness = Harness::new();
        let mut node = InterpolateParticleFrames::new();
        let values = [
            ("count_a", 3.0),
            ("count_b", 5.0),
            ("identity_a", 4.0),
            ("identity_b", 4.0),
            ("blend", 0.25),
            ("span", 0.5),
            ("acceleration_x", 0.0),
            ("acceleration_y", -9.8),
            ("acceleration_z", 0.0),
        ];
        let got = run_case(&mut harness, &mut node, Some(&a), &b, &values);
        let want = cpu_expected(
            Some(&a),
            &b,
            [3.0, 5.0],
            [4.0, 4.0],
            0.25,
            0.5,
            [0.0, -9.8, 0.0],
        );
        assert_close(&got, &want, "matched/unmatched/id0/tail");

        let values = [
            ("count_a", 3.0),
            ("count_b", 5.0),
            ("identity_a", 4.0),
            ("identity_b", 5.0),
            ("blend", 0.5),
            ("span", 1.0),
            ("acceleration_x", 0.0),
            ("acceleration_y", -9.8),
            ("acceleration_z", 0.0),
        ];
        let got = run_case(&mut harness, &mut node, Some(&a), &b, &values);
        let want = cpu_expected(
            Some(&a),
            &b,
            [3.0, 5.0],
            [4.0, 5.0],
            0.5,
            1.0,
            [0.0, -9.8, 0.0],
        );
        assert_close(&got, &want, "different epoch births");

        for (blend, span, label) in [
            (0.0, 0.5, "endpoint A"),
            (1.0, 0.5, "endpoint B"),
            (-1.0, 0.5, "blend lower clamp"),
            (2.0, -1.0, "blend/span clamp"),
            (0.25, 0.0, "zero span interior blend"),
        ] {
            let values = [
                ("count_a", 3.0),
                ("count_b", 2.0),
                ("identity_a", 4.0),
                ("identity_b", 4.0),
                ("blend", blend),
                ("span", span),
                ("acceleration_x", 0.0),
                ("acceleration_y", -9.8),
                ("acceleration_z", 0.0),
            ];
            let got = run_case(&mut harness, &mut node, Some(&a), &b, &values);
            let want = cpu_expected(
                Some(&a),
                &b,
                [3.0, 2.0],
                [4.0, 4.0],
                f64::from(blend),
                f64::from(span),
                [0.0, -9.8, 0.0],
            );
            assert_close(&got, &want, label);
        }

        // No particles_a input reaches the real `run()` path here. The
        // runtime binds B as the safe gather dummy and forces the empty-A
        // default, so births still rewind and grow rather than holding B.
        let values = [
            ("count_a", 3.0),
            ("blend", 0.25),
            ("span", 0.5),
            ("acceleration_x", 0.0),
            ("acceleration_y", -9.8),
            ("acceleration_z", 0.0),
        ];
        let got = run_case(&mut harness, &mut node, None, &b, &values);
        let want = cpu_expected(
            None,
            &b,
            [0.0, -1.0],
            [0.0, 0.0],
            0.25,
            0.5,
            [0.0, -9.8, 0.0],
        );
        assert_close(&got, &want, "actual unwired A");
    }

    fn coefficient_of_variation(values: &[f64]) -> f64 {
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let variance = values
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / values.len() as f64;
        variance.sqrt() / mean
    }

    #[test]
    fn fluid_interpolated_motion_is_even() {
        let mut harness = Harness::new();
        let mut node = InterpolateParticleFrames::new();
        let mut interpolated = Vec::new();
        let mut unblended = Vec::new();
        let step = 1.0_f32 / 30.0;
        for display_frame in 0..60u32 {
            // Each pair of display frames presents one seeded A/B tick pair:
            // A and B differ by v * (1/30 s), so the Hermite path is linear
            // at the real solver span. There are 60 display dispatches over
            // 30 tick intervals, with no startup hold omitted from the
            // measured window.
            let tick = display_frame / 2;
            let a: Vec<_> = (0..3)
                .map(|index| {
                    particle(
                        [tick as f32 * step + index as f32, index as f32 * 0.25, 0.0],
                        [1.0, 0.0, 0.0],
                        0.1,
                        index + 1,
                    )
                })
                .collect();
            let b: Vec<_> = (0..3)
                .map(|index| {
                    particle(
                        [
                            (tick + 1) as f32 * step + index as f32,
                            index as f32 * 0.25,
                            0.0,
                        ],
                        [1.0, 0.0, 0.0],
                        0.1,
                        index + 1,
                    )
                })
                .collect();
            let blend = if display_frame.is_multiple_of(2) {
                0.0
            } else {
                0.5
            };
            let values = [
                ("count_a", 3.0),
                ("count_b", 3.0),
                ("identity_a", 1.0),
                ("identity_b", 1.0),
                ("blend", blend),
                ("span", step),
                ("acceleration_x", 0.0),
                ("acceleration_y", 0.0),
                ("acceleration_z", 0.0),
            ];
            let got = run_case(&mut harness, &mut node, Some(&a), &b, &values);
            interpolated.push(
                got.iter()
                    .map(|particle| f64::from(particle.position_radius[0]))
                    .sum::<f64>()
                    / got.len() as f64,
            );
            let values = [
                ("count_a", 3.0),
                ("count_b", 3.0),
                ("identity_a", 1.0),
                ("identity_b", 1.0),
                ("blend", 1.0),
                ("span", step),
                ("acceleration_x", 0.0),
                ("acceleration_y", 0.0),
                ("acceleration_z", 0.0),
            ];
            let got = run_case(&mut harness, &mut node, Some(&a), &b, &values);
            unblended.push(
                got.iter()
                    .map(|particle| f64::from(particle.position_radius[0]))
                    .sum::<f64>()
                    / got.len() as f64,
            );
        }
        let interpolated_steps: Vec<f64> = interpolated
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .collect();
        let unblended_steps: Vec<f64> = unblended
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .collect();
        assert!(
            coefficient_of_variation(&interpolated_steps) < 0.25,
            "interpolated CV: {}",
            coefficient_of_variation(&interpolated_steps)
        );
        assert!(
            coefficient_of_variation(&unblended_steps) > 0.75,
            "unblended CV: {}",
            coefficient_of_variation(&unblended_steps)
        );
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
