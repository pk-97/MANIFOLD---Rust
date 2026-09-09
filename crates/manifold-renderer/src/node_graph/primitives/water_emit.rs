//! `node.water_emit` — S5 solver stage: deterministic lattice emission.
//!
//! First stage of the repeated water region body (design step 1): grows the
//! live set by birthing cell-centred h/2-spacing lattice records into the
//! unused tail of the particle wire — ordinal births, no GPU append/readback.
//! The per-owner cursor (born count + fractional carry) lives CPU-side in the
//! `StateStore` and resets with the boundary clock; the birth window reaches
//! the kernel as derived uniforms. Capacity exhaustion (or a drained
//! emission box) stops emission and reports Full once per episode; existing
//! water always passes through untouched.
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md sections 5 and 6;
//! docs/WATER_IMPLEMENTATION_PLAN.md section 2.2 (stage port table).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::state_store::NodeState;
use crate::node_graph::water::{
    GRID_SPACING, REST_DENSITY, SEED_ACTIVE_PARTICLES, WaterParticle,
};

/// Default emission box: a 0.5×1.0×0.5 m column above the pool top
/// (`node.seed_water`'s default pool ends at y = 0.75), 8×16×8 = 1024
/// lattice slots at h/2 spacing — the "short pour" of design section 8.
pub const EMIT_MIN: [f32; 3] = [-0.25, 0.75, -0.25];
/// Default emission box top — see [`EMIT_MIN`].
pub const EMIT_MAX: [f32; 3] = [0.25, 1.75, 0.25];

/// The birth window for one substep, computed by [`EmitCursor::advance`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmitPlan {
    /// Absolute slot index of the first birth this substep.
    pub lo: u32,
    /// Absolute slot index one past the last birth (exclusive).
    pub hi: u32,
    /// Emission has stopped: particle capacity or the emission box is
    /// exhausted while the rate is positive.
    pub full: bool,
    /// True on the first substep of a Full episode — `run` reports it once.
    pub report_full: bool,
}

/// Per-owner emission state: the deterministic birth cursor. `born` counts
/// particles birthed since the last reset; `frac` carries the fractional
/// birth remainder across substeps (design: "residual fractional births
/// carry forward").
#[derive(Clone, Copy, Debug)]
pub struct EmitCursor {
    born: u32,
    frac: f64,
    last_step_time: f32,
    epoch: u64,
    initialized: bool,
    full_reported: bool,
}

impl Default for EmitCursor {
    fn default() -> Self {
        Self {
            born: 0,
            frac: 0.0,
            last_step_time: 0.0,
            epoch: 0,
            initialized: false,
            full_reported: false,
        }
    }
}

impl NodeState for EmitCursor {}

impl EmitCursor {
    /// Advance the cursor one substep and return the birth window. Pure CPU
    /// logic — `run` calls this once per region iteration; the GPU tests
    /// drive it directly with the same call sequence.
    #[allow(clippy::too_many_arguments)]
    pub fn advance(
        &mut self,
        epoch: u64,
        step_time: f32,
        rate: f64,
        step_dt: f64,
        first_free: u32,
        capacity: u32,
        lattice_count: u32,
    ) -> EmitPlan {
        // Reset with the boundary: first observation, epoch change (seek /
        // project load), or the clock restarted (a reset frame schedules zero
        // ticks, so the regression shows up on the next advancing substep).
        // Reset dominates emission on the same frame — the reset frame
        // itself never reaches the region body.
        if !self.initialized || epoch != self.epoch || step_time < self.last_step_time {
            self.born = 0;
            self.frac = 0.0;
            self.full_reported = false;
            self.initialized = true;
            self.epoch = epoch;
        }

        let born_before = self.born;
        if rate > 0.0 && step_dt > 0.0 {
            self.frac += rate * step_dt;
            let whole = self.frac.floor();
            self.frac -= whole;
            self.born = self.born.saturating_add(whole as u32);
        }

        // Exhaustion: the unused tail (capacity - first_free) or the
        // emission box, whichever is smaller. A positive rate with an
        // already-exhausted tail or a zero-lattice box reports Full on the
        // first advance — a visible misconfiguration, not a silent no-op.
        let max_born = capacity.saturating_sub(first_free).min(lattice_count);
        let full = rate > 0.0 && self.born >= max_born && (self.born > 0 || max_born == 0);
        if self.born > max_born {
            self.born = max_born;
        }
        let report_full = full && !self.full_reported;
        if report_full {
            self.full_reported = true;
        }

        self.last_step_time = step_time;
        EmitPlan {
            lo: first_free + born_before.min(self.born),
            hi: first_free + self.born,
            full,
            report_full,
        }
    }

    /// Births delivered since the last reset (diagnostics and tests).
    pub fn born(&self) -> u32 {
        self.born
    }
}

/// Generated-codegen uniform layout: scalar params in PARAMS order (the six
/// emission box bounds, `grid_spacing`, `rest_density`, `rate`, then the
/// allocation-only `first_free` Int -> i32), then the derived `birth_lo` /
/// `birth_hi` window (u32) packed per dispatch by `run()`, then the
/// codegen-injected `dispatch_count`. 10 + 2 + 1 = 13 words -> 3 pads = 64
/// bytes. Field order must match the generated WGSL `Params` exactly.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EmitUniforms {
    pub emit_min_x: f32,
    pub emit_min_y: f32,
    pub emit_min_z: f32,
    pub emit_max_x: f32,
    pub emit_max_y: f32,
    pub emit_max_z: f32,
    pub grid_spacing: f32,
    pub rest_density: f32,
    pub rate: f32,
    pub first_free: i32,
    pub birth_lo: u32,
    pub birth_hi: u32,
    pub dispatch_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
    pub _pad2: u32,
}

crate::primitive! {
    name: WaterEmit,
    type_id: "node.water_emit",
    purpose: "Emit new Live Water particles deterministically (design step 1): birth cell-centred h/2-spacing lattice records into the unused tail of the particle wire at the configured rate in particles/s, with the fractional remainder carrying forward across substeps. Ordinal births fill slots from `first_free` (where the seed lattice ends) — no GPU append or readback; the per-owner cursor lives CPU-side and resets with the boundary clock. Capacity exhaustion or a drained emission box stops emission and reports Full once per episode while existing water continues untouched. Inactive slots stay inactive until born; no recycling, drains, or particle death in V1. `rate` accepts a wire (pour envelopes) or the param. Runs as the first stage of the repeated water region body, before the grid transfer stages.",
    inputs: {
        in: Array(WaterParticle) required,
        rate: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        step_time: ScalarF32 optional,
    },
    outputs: {
        out: Array(WaterParticle),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("emit_min_x"),
            label: "Emit Min X",
            ty: ParamType::Float,
            default: ParamValue::Float(EMIT_MIN[0]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("emit_min_y"),
            label: "Emit Min Y",
            ty: ParamType::Float,
            default: ParamValue::Float(EMIT_MIN[1]),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("emit_min_z"),
            label: "Emit Min Z",
            ty: ParamType::Float,
            default: ParamValue::Float(EMIT_MIN[2]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("emit_max_x"),
            label: "Emit Max X",
            ty: ParamType::Float,
            default: ParamValue::Float(EMIT_MAX[0]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("emit_max_y"),
            label: "Emit Max Y",
            ty: ParamType::Float,
            default: ParamValue::Float(EMIT_MAX[1]),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("emit_max_z"),
            label: "Emit Max Z",
            ty: ParamType::Float,
            default: ParamValue::Float(EMIT_MAX[2]),
            range: Some((-2.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("grid_spacing"),
            label: "Grid Spacing h",
            ty: ParamType::Float,
            default: ParamValue::Float(GRID_SPACING),
            range: Some((0.01, 0.25)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rest_density"),
            label: "Rest Density",
            ty: ParamType::Float,
            default: ParamValue::Float(REST_DENSITY),
            range: Some((100.0, 2000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rate"),
            label: "Emit rate (particles/s)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("first_free"),
            label: "First free slot",
            ty: ParamType::Int,
            default: ParamValue::Float(SEED_ACTIVE_PARTICLES as f32),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "First stage of the repeated water region body (design step 1): `water_state.out -> water_emit -> water_impulse -> clear_grid -> ...`. Wire `rate` from an envelope/Value node for pours; leave it at 0 for a still pool. `step_dt`/`step_time` come from node.water_state's step outputs. `first_free` must match node.seed_water's lattice count (default 65,536): it is where the unused tail begins. The output aliases the input wire (pure per-element birth into zeroed slots).",
    examples: [],
    picker: { label: "Water Emit", category: Atom },
    summary: "Pours new water particles into the unused tail of the particle buffer at a set rate, on the same lattice as the seed.",
    category: Particles3D,
    role: Filter,
    aliases: ["water emit", "emit water", "pour water", "water faucet"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/water_emit_body.wgsl"),
    input_access: [Coincident],
    derived_uniforms: ["birth_lo:u32", "birth_hi:u32"],
    wgsl_includes: [include_str!("shaders/water_common.wgsl")],
}

impl Primitive for WaterEmit {
    /// The birth is a pure per-element write into an inactive slot — the
    /// output aliases the input wire.
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities
                .iter()
                .find(|(p, _)| *p == "in")
                .map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let particle_size = std::mem::size_of::<WaterParticle>() as u64;
        let capacity = (in_buf.size.min(out_buf.size) / particle_size) as u32;
        if capacity == 0 {
            return;
        }

        let read = |name: &str, default: f32| match ctx.params.get(name) {
            Some(ParamValue::Float(f)) => *f,
            _ => default,
        };
        let grid_spacing = read("grid_spacing", GRID_SPACING);
        let spacing = grid_spacing * 0.5;
        let emit_min = [
            read("emit_min_x", EMIT_MIN[0]),
            read("emit_min_y", EMIT_MIN[1]),
            read("emit_min_z", EMIT_MIN[2]),
        ];
        let emit_max = [
            read("emit_max_x", EMIT_MAX[0]),
            read("emit_max_y", EMIT_MAX[1]),
            read("emit_max_z", EMIT_MAX[2]),
        ];
        // Lattice capacity of the emission box; a degenerate box births
        // nothing and reports Full rather than dividing by zero.
        let lattice_count = {
            let mut count: u32 = 1;
            for axis in 0..3 {
                let extent = emit_max[axis] - emit_min[axis];
                if extent <= 0.0 {
                    count = 0;
                    break;
                }
                let n = (extent / spacing + 0.5).floor().max(0.0) as u32;
                count = count.saturating_mul(n);
            }
            count
        };
        let first_free = read("first_free", SEED_ACTIVE_PARTICLES as f32).max(0.0) as u32;
        let rate = ctx.scalar_or_param("rate", 0.0) as f64;
        let step_dt = ctx.scalar_or_param("step_dt", 0.0) as f64;
        let step_time = ctx.scalar_or_param("step_time", 0.0);

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        let mut cursor = {
            let store = ctx
                .state
                .as_deref_mut()
                .expect("WaterEmit requires a StateStore");
            match store.get::<EmitCursor>(node_id, owner_key) {
                Some(existing) => *existing,
                None => EmitCursor::default(),
            }
        };
        let frame = ctx.simulation_frame;
        let epoch = frame.map(|f| f.epoch).unwrap_or(0);
        let plan = cursor.advance(
            epoch,
            step_time,
            rate,
            step_dt,
            first_free,
            capacity,
            lattice_count,
        );
        {
            let store = ctx
                .state
                .as_deref_mut()
                .expect("WaterEmit requires a StateStore");
            store.insert(node_id, owner_key, cursor);
        }
        if plan.report_full {
            ctx.error(
                "WaterEmit: particle capacity or emission box exhausted — emission stopped (Full); \
                 existing water continues"
                    .to_string(),
            );
        }
        if plan.hi <= plan.lo {
            // No births this substep: the output aliases the input, so the
            // wire is already correct — nothing to dispatch.
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel
            // is generated from the `wgsl_body` so the atom participates in
            // freeze fusion.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.water_emit standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.water_emit",
            )
        });

        let uniforms = EmitUniforms {
            emit_min_x: emit_min[0],
            emit_min_y: emit_min[1],
            emit_min_z: emit_min[2],
            emit_max_x: emit_max[0],
            emit_max_y: emit_max[1],
            emit_max_z: emit_max[2],
            grid_spacing,
            rest_density: read("rest_density", REST_DENSITY),
            rate: rate as f32,
            first_free: first_free as i32,
            birth_lo: plan.lo,
            birth_hi: plan.hi,
            dispatch_count: capacity,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // uniform(0), in(1), out(2).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: in_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.water_emit",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    const DT: f32 = 1.0 / 960.0;
    /// Monotone step-time helper: frame-major, then substep.
    fn t(frame: u64, substep: u32) -> f32 {
        frame as f32 * 10.0 + (substep as f32 + 1.0) * DT
    }

    /// Default-box lattice count: 8×16×8 = 1024 at h/2 spacing.
    const DEFAULT_LATTICE: u32 = 1024;

    #[test]
    fn emit_declares_particle_in_particle_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<WaterParticle>();
        assert_eq!(WaterEmit::TYPE_ID, "node.water_emit");
        assert_eq!(WaterEmit::INPUTS[0].name, "in");
        assert_eq!(WaterEmit::INPUTS[0].ty, PortType::Array(particle_layout));
        assert_eq!(WaterEmit::OUTPUTS.len(), 1);
        assert_eq!(WaterEmit::OUTPUTS[0].name, "out");
        assert_eq!(WaterEmit::OUTPUTS[0].ty, PortType::Array(particle_layout));
    }

    #[test]
    fn emit_registers_and_aliases() {
        let prim = WaterEmit::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.water_emit");
        assert_eq!(node.aliased_array_io(), &[("in", "out")]);
    }

    #[test]
    fn emit_codegen_binds_coincident_input() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<WaterEmit>()
            .expect("node.water_emit standalone codegen");
        assert!(wgsl.contains("struct Element"));
        assert!(wgsl.contains("birth_lo"));
        assert!(wgsl.contains("birth_hi"));
    }

    #[test]
    fn emit_cursor_fractional_carry_across_substeps() {
        let mut cursor = EmitCursor::default();
        // 100 particles/s at 960 substeps/s = 0.1041667 births/substep: nine
        // substeps of carry before the first whole birth, then a steady
        // 0-or-1 rhythm that totals exactly floor(rate * simulated_time).
        let rate = 100.0f64;
        let mut total = 0u32;
        let mut windows = Vec::new();
        for substep in 0..960u32 {
            let plan = cursor.advance(0, t(1, substep), rate, DT as f64, 65_536, 131_072, DEFAULT_LATTICE);
            total += plan.hi - plan.lo;
            windows.push(plan);
            assert!(plan.hi >= plan.lo);
            if substep > 0 {
                assert!(plan.lo >= windows[windows.len() - 2].hi, "windows must not overlap");
            }
        }
        assert_eq!(total, 100, "960 substeps at 100/s must birth exactly 100");
        // The first birth lands after the carry reaches 1.0: floor chain of
        // 0.1041667*n first crosses 1 at n = 10 (1.0417).
        assert_eq!(windows[0].hi - windows[0].lo, 0);
        assert!(windows[8].hi == windows[8].lo, "9th substep still carrying");
        assert_eq!(windows[9].hi - windows[9].lo, 1, "first birth at the 10th substep");
    }

    #[test]
    fn emit_cursor_stops_at_capacity_and_reports_full_once() {
        let mut cursor = EmitCursor::default();
        // Capacity tail of 16, huge box: capacity binds.
        let mut reports = 0;
        let mut total = 0u32;
        for substep in 0..64u32 {
            let plan = cursor.advance(0, t(1, substep), 1.0e6, DT as f64, 100, 116, 1024);
            total += plan.hi - plan.lo;
            if plan.report_full {
                reports += 1;
            }
            assert!(plan.hi <= 116, "births past capacity");
        }
        assert_eq!(total, 16, "tail is exactly 16 slots");
        assert_eq!(reports, 1, "Full is reported once per episode");
        let after = cursor.advance(0, t(2, 0), 1.0e6, DT as f64, 100, 116, 1024);
        assert!(after.full, "stays Full");
        assert!(!after.report_full, "no repeat report");
        assert_eq!(after.hi - after.lo, 0, "no births while Full");
    }

    #[test]
    fn emit_cursor_box_exhaustion_is_full() {
        let mut cursor = EmitCursor::default();
        // Box of 4 lattice slots, huge tail: the box binds.
        let mut total = 0u32;
        for substep in 0..32u32 {
            let plan = cursor.advance(0, t(1, substep), 1.0e6, DT as f64, 0, 131_072, 4);
            total += plan.hi - plan.lo;
        }
        assert_eq!(total, 4, "only the box lattice can be born");
        assert!(cursor.advance(0, t(2, 0), 1.0e6, DT as f64, 0, 131_072, 4).full);
    }

    #[test]
    fn emit_cursor_degenerate_box_births_nothing_and_reports() {
        let mut cursor = EmitCursor::default();
        let plan = cursor.advance(0, t(1, 0), 100.0, DT as f64, 0, 131_072, 0);
        assert_eq!(plan.hi - plan.lo, 0);
        assert!(plan.report_full, "a zero-lattice box is a visible misconfiguration");
    }

    #[test]
    fn emit_cursor_reset_regeneration_restarts_the_cursor() {
        let mut cursor = EmitCursor::default();
        for substep in 0..100u32 {
            let _ = cursor.advance(0, t(1, substep), 1.0e4, DT as f64, 0, 131_072, DEFAULT_LATTICE);
        }
        assert!(cursor.born() > 0);
        // Boundary reset: the next advancing substep sees a regressed clock.
        let plan = cursor.advance(0, t(0, 0), 1.0e4, DT as f64, 0, 131_072, DEFAULT_LATTICE);
        assert_eq!(plan.lo, 0, "births restart at the tail base after reset");
        assert_eq!(cursor.born(), plan.hi - plan.lo);
    }

    #[test]
    fn emit_cursor_pause_advances_nothing() {
        let mut cursor = EmitCursor::default();
        // Zero-substep frames never call advance (the region iterates zero
        // times) — simulate a pause gap by simply not advancing, then resume:
        // no catch-up burst, the pour continues at the same rate.
        let mut total = 0u32;
        for substep in 0..10u32 {
            let plan = cursor.advance(0, t(1, substep), 960.0, DT as f64, 0, 131_072, DEFAULT_LATTICE);
            total += plan.hi - plan.lo;
        }
        // 960/s at dt = 1/960 = exactly 1 birth per substep.
        assert_eq!(total, 10);
        for substep in 0..10u32 {
            let plan = cursor.advance(0, t(2, substep), 960.0, DT as f64, 0, 131_072, DEFAULT_LATTICE);
            total += plan.hi - plan.lo;
        }
        assert_eq!(total, 20, "resume continues the pour with no backlog");
    }

    #[test]
    fn emit_cursor_epoch_change_resets() {
        let mut cursor = EmitCursor::default();
        let _ = cursor.advance(3, t(1, 0), 1.0e4, DT as f64, 0, 131_072, DEFAULT_LATTICE);
        assert!(cursor.born() > 0);
        let plan = cursor.advance(4, t(2, 0), 1.0e4, DT as f64, 0, 131_072, DEFAULT_LATTICE);
        assert_eq!(plan.lo, 0);
    }

    #[test]
    fn emit_cursor_rate_zero_never_births() {
        let mut cursor = EmitCursor::default();
        for substep in 0..100u32 {
            let plan = cursor.advance(0, t(1, substep), 0.0, DT as f64, 0, 131_072, DEFAULT_LATTICE);
            assert_eq!(plan.hi - plan.lo, 0);
            assert!(!plan.full, "rate zero is not Full");
        }
    }
}
