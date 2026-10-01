//! Ported from FLIP Fluids rigidboundaryvelocity.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
//!
//! `node.face_impulse_to_bodies` — sum a face grid of impulses per body
//! into its linear and angular impulse and velocity change
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (solids in the water)). A
//! barriered two-pass reduction (docs/ADDING_PRIMITIVES.md exclusion 1), as
//! node.dot_products: workgroup partial sums per body, then one thread per
//! body adds its partials in a fixed order.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use super::sort_particles_into_cells::float_param;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid::TICK;
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::liquid::bodies::LiquidBody;
use crate::node_graph::liquid::lattice::{cell_lattice, face_count};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const SHADER: &str = include_str!("shaders/face_impulse_to_bodies.wgsl");

/// Floats per body in the sums: linear impulse, angular impulse, dv, dω.
pub(crate) const BODY_SUM_FLOATS: u32 = 16;
/// Bodies one node sums: the finalize pass is one 64-thread workgroup.
const MAX_BODIES: u32 = MAX_FLUID_ROLES as u32;
/// Partial sums per body, at most; one per 4096 face records below that.
const MAX_GROUPS: u32 = 64;
/// Floats per partial: six sums padded to eight.
const PARTIAL_STRIDE: u32 = 8;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SumParams {
    lattice_min: [f32; 3],
    cell_size: f32,
    nodes: [u32; 3],
    body_count: u32,
    first: u32,
    groups: u32,
    has_base: u32,
    has_reaction: u32,
    tick_seconds: f32,
    max_bodies: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: FaceImpulseToBodies,
    type_id: "node.face_impulse_to_bodies",
    purpose: "Each body's total of a face grid of impulses (node.pressure_face_impulse or node.friction_face_impulse: per face, an impulse along each axis in N·s and the axes' owner code in velocity w), 16 floats per body into out: the linear impulse, the angular impulse about the body's centre of mass (bodies, posed tick_seconds on; r to the face centre on a lattice of nodes_x/y/z cells from lattice_min, cell_size apart), then the velocity change 1/m times the linear impulse and the angular velocity change, the body's world inverse inertia times the angular impulse, each a vec4 with w 0. base, when wired, is added to the impulses first. A body that takes no reaction (1/m 0 or no shape) changes no velocity; bodies past body_count are zero. The sums run in a fixed order. reaction, when wired, receives the same 16 floats per body in place.",
    inputs: {
        impulses: Array(FaceSample) required,
        bodies: Array(LiquidBody) required,
        base: Array(f32) optional,
        reaction: Array(f32) optional,
        body_count: ScalarF32 optional,
        rows: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
        reaction_out: Array(f32),
    },
    params: [
        float_param!("lattice_min_x", "Lattice Min X", -2.0, -1000.0, 1000.0),
        float_param!("lattice_min_y", "Lattice Min Y", 0.0, -1000.0, 1000.0),
        float_param!("lattice_min_z", "Lattice Min Z", -2.0, -1000.0, 1000.0),
        float_param!("cell_size", "Cell Size", 0.0625, 1.0e-4, 100.0),
        float_param!("nodes_x", "Cells X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 1.0, 1024.0),
        float_param!("body_count", "Bodies", 0.0, 0.0, MAX_FLUID_ROLES as f32),
        float_param!("rows", "Rows", 0.0, 0.0, 16_777_216.0),
        float_param!("tick_seconds", "Tick (s)", TICK as f32, 0.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The GPU FLIP two-way body coupling's sum, not yet wired into any preset (BUG-6zj3 (step body owner code)). Inside the pressure solve on node.pressure_face_impulse of the search direction, feeding node.body_pressure_product's sums; per water step on the solved pressure and on node.friction_face_impulse, chained through base so the tick's sums grow step by step and feed the next step's solid face velocity. The tick's last sum takes the domain's reaction, which the domain reads back as each body's impulse. bodies, body_count and rows from the liquid's domain.",
    examples: [],
    picker: { label: "Face Impulse to Bodies", category: Atom },
    summary: "Adds up how hard the water pushes on each floating object, and how that changes the object's motion.",
    category: Particles3D,
    role: Map,
    aliases: ["body reaction", "buoyancy", "two-way coupling", "reduce", "sum per body"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        partial: Option<GpuComputePipeline> = None,
        finalize: Option<GpuComputePipeline> = None,
        partials: Option<GpuBuffer> = None,
    },
}

impl Primitive for FaceImpulseToBodies {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        match port {
            "out" => Some(MAX_BODIES * BODY_SUM_FLOATS),
            "reaction_out" => inputs.iter().find(|(p, _)| *p == "reaction").map(|&(_, n)| n),
            _ => None,
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("reaction", "reaction_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        {
            let gpu = ctx.gpu_encoder();
            if self.partial.is_none() {
                self.partial = Some(gpu.device.create_compute_pipeline(SHADER, "partial_main", "node.face_impulse_to_bodies"));
                self.finalize = Some(gpu.device.create_compute_pipeline(SHADER, "finalize_main", "node.face_impulse_to_bodies"));
            }
            if self.partials.is_none() {
                self.partials = Some(gpu.device.create_buffer(u64::from(MAX_BODIES * MAX_GROUPS * PARTIAL_STRIDE) * 4));
            }
        }
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Face Impulse to Bodies: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let min = ["lattice_min_x", "lattice_min_y", "lattice_min_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        let body_count = ctx.scalar_or_param("body_count", 0.0);
        let rows = ctx.scalar_or_param("rows", 0.0);
        let tick_seconds = ctx.scalar_or_param("tick_seconds", TICK as f32);
        if !(cell_size.is_finite() && cell_size > 0.0 && min.iter().all(|v| v.is_finite()) && tick_seconds.is_finite()) {
            ctx.error("Face Impulse to Bodies: cell_size must be positive and the lattice and tick finite".to_string());
            return;
        }
        if !(body_count >= 0.0 && body_count <= MAX_BODIES as f32 && rows >= body_count && rows.fract() == 0.0 && body_count.fract() == 0.0) {
            ctx.error(format!(
                "Face Impulse to Bodies: body_count {body_count} and rows {rows} must be whole, body_count at most {MAX_BODIES} and rows"
            ));
            return;
        }
        let (Some(impulses), Some(bodies), Some(out)) = (ctx.inputs.array("impulses"), ctx.inputs.array("bodies"), ctx.outputs.array("out")) else {
            return;
        };
        let base = ctx.inputs.array("base");
        let reaction = ctx.outputs.array("reaction_out");
        let faces = face_count(nodes);
        let sums = u64::from(MAX_BODIES * BODY_SUM_FLOATS) * 4;
        let body_count = body_count as u32;
        if faces * size_of::<FaceSample>() as u64 > impulses.size
            || rows as u64 * size_of::<LiquidBody>() as u64 > bodies.size
            || sums > out.size
            || base.is_some_and(|b| u64::from(body_count * BODY_SUM_FLOATS) * 4 > b.size)
            || reaction.is_some_and(|r| u64::from(body_count * BODY_SUM_FLOATS) * 4 > r.size)
        {
            ctx.error(format!("Face Impulse to Bodies: {body_count} bodies on a {nodes:?} lattice are larger than the arrays"));
            return;
        }
        let groups = (faces.div_ceil(4096) as u32).clamp(1, MAX_GROUPS);
        let uniforms = SumParams {
            lattice_min: min,
            cell_size,
            nodes,
            body_count,
            first: rows as u32 - body_count,
            groups,
            has_base: u32::from(base.is_some()),
            has_reaction: u32::from(reaction.is_some()),
            tick_seconds,
            max_bodies: MAX_BODIES,
            _pad0: 0,
            _pad1: 0,
        };
        let partials = self.partials.as_ref().expect("partials allocated");
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: impulses, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: bodies, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: base.unwrap_or(impulses), offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: partials, offset: 0 },
            GpuBinding::Buffer { binding: 5, buffer: out, offset: 0 },
            GpuBinding::Buffer { binding: 6, buffer: reaction.unwrap_or(out), offset: 0 },
        ];
        let gpu = ctx.gpu_encoder();
        if body_count > 0 {
            gpu.native_enc.dispatch_compute(
                self.partial.as_ref().expect("pipeline created"),
                &bindings,
                [groups, body_count, 1],
                "node.face_impulse_to_bodies.partial",
            );
        }
        gpu.native_enc.dispatch_compute(
            self.finalize.as_ref().expect("pipeline created"),
            &bindings,
            [1, 1, 1],
            "node.face_impulse_to_bodies.finalize",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::SHADER;

    /// GPU FLIP's step gathers instead of scattering; this hand shader has no
    /// codegen body for the liquid conformance check to read.
    #[test]
    fn face_impulse_to_bodies_uses_no_atomics() {
        assert!(!SHADER.contains("atomic"));
    }
}
