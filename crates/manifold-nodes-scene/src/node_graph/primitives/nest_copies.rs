//! `node.nest_copies` — a full set of inner copies at every outer copy, so
//! copies of something that has its own copies keep them (a swarm drawing a
//! Group with Copies on draws the whole layout at every particle).
//!
//! Copy `i * inner_count + j` is inner copy `j` placed by outer copy `i`.
//! The inner count is live (a Copies Count), so the node keeps its own
//! output storage and grows it to outer capacity × inner count when the
//! count rises; it never shrinks or reallocates per frame, and `count`
//! says how many copies to draw.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

const COPY_BYTES: u64 = std::mem::size_of::<InstanceTransform>() as u64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NestUniforms {
    inner_count: u32,
    total: u32,
    _pad0: u32,
    _pad1: u32,
}

manifold_node_engine::primitive! {
    name: NestCopies,
    type_id: "node.nest_copies",
    purpose: "Place every inner copy at every outer copy: copy i*M + j (M = inner_count, port-shadowed, at most the inner array's length) is inner copy j carried by outer copy i, so position = P + s1*R1*p2, rotation = R1*R2 (Euler XYZ as the renderer composes it) and scale = s1*s2. A zero-scale copy on either side stays a hole; a mirror marker is not carried. `count` publishes outer length × M. The output is storage the node keeps, grown to the needed size when M rises, so a live count never rebuilds the graph.",
    inputs: {
        outer: Array(InstanceTransform) required,
        inner: Array(InstanceTransform) required,
        inner_count: ScalarF32 optional,
    },
    outputs: {
        copies: Array(InstanceTransform),
        count: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("inner_count"),
            label: "Inner Count",
            ty: ParamType::Int,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4_000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire a copies layout's instances into inner and its active_count into inner_count, and the copies that should carry it (a swarm's node.swarm_to_copies) into outer; draw copies with instance_count from count. The order is outer-major, so copy k belongs to outer copy k / inner_count.",
    examples: [],
    picker: { label: "Nest Copies", category: Atom },
    summary: "Puts a whole set of copies at every copy of another set: copies of copies.",
    category: Geometry3D,
    role: Map,
    aliases: ["nest copies", "copies of copies", "nested instances", "compose instances"],
    boundary_reason: Blocked,
    extra_fields: {
        // Grown when outer × inner count rises; the renderer reads it.
        storage: Option<GpuBuffer> = None,
        storage_copies: u64 = 0,
        // The size last refused for memory, so the refusal is said once.
        refused: u64 = 0,
    },
}

impl Primitive for NestCopies {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "copies"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "copies").then_some(self.storage.as_ref()).flatten()
    }

    /// Planned at the outer capacity; the storage it provides grows past it.
    fn array_output_capacity(&self, port_name: &str, _params: &ParamValues, input_capacities: &[(&str, u32)]) -> Option<u32> {
        (port_name == "copies").then(|| input_capacities.iter().find(|(port, _)| *port == "outer").map_or(1, |&(_, n)| n.max(1)))
    }

    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        self.pipeline.get_or_insert_with(|| device.create_compute_pipeline(include_str!("shaders/nest_copies.wgsl"), "cs_main", "node.nest_copies"));
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(outer), Some(inner)) = (ctx.inputs.array("outer"), ctx.inputs.array("inner")) else {
            ctx.outputs.set_scalar("count", ParamValue::Float(0.0));
            return;
        };
        let outer_copies = outer.size / COPY_BYTES;
        let inner_copies = inner.size / COPY_BYTES;
        let inner_count = (ctx.scalar_or_param("inner_count", 1.0).round().max(0.0) as u64).min(inner_copies);
        let mut wanted = outer_copies * inner_count;
        let mut refusal = None;
        ctx.mark_gpu_accessed();
        let Some(gpu) = ctx.gpu.as_deref_mut() else {
            return;
        };
        if wanted > self.storage_copies {
            // Grows only when the count rises. The one limit is the GPU's
            // memory: past it the copies that fit are drawn and the rest refused.
            let fits = gpu.device.modifier_memory_snapshot().is_none_or(|memory| {
                memory.current_allocated_bytes.saturating_add(wanted * COPY_BYTES) <= memory.recommended_max_working_set_bytes
            });
            if fits {
                self.storage = Some(gpu.device.create_buffer_shared(wanted * COPY_BYTES));
                self.storage_copies = wanted;
            } else {
                if self.refused != wanted {
                    self.refused = wanted;
                    let error = format!(
                        "Nest Copies: {wanted} copies need {} MB, past the GPU's memory; drawing the {} that fit",
                        wanted * COPY_BYTES / 1_000_000,
                        self.storage_copies
                    );
                    refusal = Some(error);
                }
                wanted = self.storage_copies - self.storage_copies % inner_count.max(1);
            }
        }
        let Some(storage) = self.storage.as_ref() else {
            ctx.outputs.set_scalar("count", ParamValue::Float(0.0));
            return;
        };
        let total = wanted.min(u64::from(u32::MAX)) as u32;
        if total > 0 {
            let pipeline = self
                .pipeline
                .get_or_insert_with(|| gpu.device.create_compute_pipeline(include_str!("shaders/nest_copies.wgsl"), "cs_main", "node.nest_copies"));
            let uniforms = NestUniforms { inner_count: inner_count as u32, total, _pad0: 0, _pad1: 0 };
            gpu.native_enc.dispatch_compute(
                pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                    GpuBinding::Buffer { binding: 1, buffer: outer, offset: 0 },
                    GpuBinding::Buffer { binding: 2, buffer: inner, offset: 0 },
                    GpuBinding::Buffer { binding: 3, buffer: storage, offset: 0 },
                ],
                [total.div_ceil(256), 1, 1],
                "node.nest_copies",
            );
        }
        ctx.outputs.set_scalar("count", ParamValue::Float(total as f32));
        if let Some(error) = refusal {
            ctx.error(error);
        }
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! The kernel through the node against the same composition done on the
    //! CPU with the renderer's Euler convention.
    use super::*;
    use manifold_node_engine::scene::transform::{rotation_to_render_scene_euler, trs_matrix, transform_point};
    use manifold_node_engine::testkit::array_harness::{Harness, params, read};

    fn copy(pos: [f32; 3], scale: f32, rot: [f32; 3]) -> InstanceTransform {
        InstanceTransform { pos_scale: [pos[0], pos[1], pos[2], scale], rot_pad: [rot[0], rot[1], rot[2], 0.0] }
    }

    #[test]
    fn every_inner_copy_rides_every_outer_copy() {
        let mut h = Harness::new();
        let outer = [copy([1.0, 2.0, 3.0], 2.0, [0.0, 0.0, 0.0]), copy([-4.0, 0.0, 1.0], 0.5, [0.3, -0.6, 1.0]), copy([9.0; 3], 0.0, [0.0; 3])];
        let inner = [copy([1.0, 0.0, 0.0], 1.0, [0.0, 0.0, 0.0]), copy([0.0, 1.0, -2.0], 3.0, [0.2, 0.4, -0.1]), copy([5.0; 3], 1.0, [0.0; 3])];
        let (outer_slot, _) = h.array(&outer, outer.len());
        let (inner_slot, _) = h.array(&inner, inner.len());
        let (out, _) = h.array::<InstanceTransform>(&[], 1);
        let count = h.scalar();
        let inputs = [("outer", outer_slot), ("inner", inner_slot)];
        let mut node = NestCopies::new();
        // Two of the three inner copies are live.
        let (scalars, errors) = h.run(&mut node, &inputs, &[("copies", out), ("count", count)], &params(&[("inner_count", 2.0)]));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(scalars.iter().find(|(slot, _)| *slot == count).map(|(_, v)| v.clone()), Some(ParamValue::Float(6.0)));
        let nested: Vec<InstanceTransform> = read(&h.buffer(out), 6);
        for (k, got) in nested.iter().enumerate() {
            let (a, b) = (outer[k / 2], inner[k % 2]);
            let scale = a.pos_scale[3] * b.pos_scale[3];
            if scale == 0.0 {
                assert_eq!(got.pos_scale[3], 0.0, "copy {k} stays a hole");
                continue;
            }
            // The composed copy draws a point where outer ∘ inner draws it.
            let trs = |c: InstanceTransform| trs_matrix([c.pos_scale[0], c.pos_scale[1], c.pos_scale[2]], [c.rot_pad[0], c.rot_pad[1], c.rot_pad[2]], [c.pos_scale[3]; 3]);
            let probe = [0.3, -0.7, 1.1];
            let want = transform_point(&trs(a), transform_point(&trs(b), probe));
            let have = transform_point(&trs(*got), probe);
            assert!((0..3).all(|d| (want[d] - have[d]).abs() < 1e-4), "copy {k}: {have:?} want {want:?}");
            assert!((got.pos_scale[3] - scale).abs() < 1e-6);
        }
        // The rotation helper the shader mirrors round-trips the renderer's matrix.
        let r = trs_matrix([0.0; 3], [0.2, 0.4, -0.1], [1.0; 3]);
        let back = rotation_to_render_scene_euler([[r[0][0], r[1][0], r[2][0]], [r[0][1], r[1][1], r[2][1]], [r[0][2], r[1][2], r[2][2]]]);
        assert!((back[0] - 0.2).abs() < 1e-5 && (back[1] - 0.4).abs() < 1e-5 && (back[2] + 0.1).abs() < 1e-5);
    }
}
