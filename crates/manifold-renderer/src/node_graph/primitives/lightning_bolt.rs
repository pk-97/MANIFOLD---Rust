//! `node.lightning_bolt` — grow a midpoint-displacement lightning bolt
//! with recursive branching, on demand. One CPU operation per strike:
//! the geometry is regenerated only on a rising `strike` count (or the
//! optional beat-quantized auto-strike), and the emitted edge topology
//! is live for exactly ONE frame — the graph's feedback/afterglow chain
//! owns the decay, matching how a real return stroke reads: a
//! single-frame flash, then glow.
//!
//! Outputs are shaped for `node.draw_lines`' explicit-topology path:
//! one shared `points` buffer, a parallel `widths` buffer (per-point
//! thickness multipliers — thick trunk, hairline branch tips), and two
//! `EdgePair` topologies so core and branches can be drawn at
//! different intensities. Variable bolt size inside fixed-capacity
//! Array buffers is expressed via `EdgePair::SENTINEL` padding, which
//! `draw_lines` already skips; the drafts-doc sketch of two separate
//! `CurvePoint` arrays can't express that (the sequential draw path
//! renders the whole buffer capacity) nor the disjoint branch
//! polylines, which is why the topology lives in edge buffers.
//!
//! Coordinates are `draw_lines` pre-aspect curve space (origin-centred,
//! y in roughly ±0.5). Defaults strike top → bottom, portrait-native.
//!
//! Determinism: the generator is a pure function of (seed, params).
//! `seed_mode = Fixed` replays the identical bolt every strike;
//! `Reroll` hashes a per-strike counter into the seed.

use std::borrow::Cow;

use crate::generators::mesh_common::{CurvePoint, EdgePair};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const SEED_MODES: &[&str] = &["Reroll", "Fixed"];

/// Hairline width at every filament tip; the visual "vanishes to
/// nothing" endpoint of the taper.
const TIP_WIDTH: f32 = 0.05;
/// Core taper: trunk 1.0 at the strike origin down to this at the far
/// endpoint. Branches start from the local core width × branch_decay.
const CORE_END_WIDTH: f32 = 0.35;

fn xorshift(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// Uniform in [0, 1).
fn rand01(state: &mut u32) -> f32 {
    (xorshift(state) >> 8) as f32 / 16_777_216.0
}

/// Uniform in [-1, 1).
fn rand_pm(state: &mut u32) -> f32 {
    rand01(state) * 2.0 - 1.0
}

crate::primitive! {
    name: LightningBolt,
    type_id: "node.lightning_bolt",
    purpose: "Generate a lightning bolt as polyline geometry on each strike: midpoint-displacement core from (x0,y0) to (x1,y1) plus two generations of recursively displaced branches, with per-point width taper (thick trunk, hairline tips). `strike` is a trigger-count stream (rising value = new bolt); `auto_strike_beats` > 0 additionally fires a bolt every N beats. The emitted core/branch edge topologies are live for exactly one frame (sentinel-padded otherwise) — wire the draws into a feedback chain for afterglow. Outputs feed node.draw_lines: shared `points`, parallel `widths` (wire to draw_lines.widths for the taper), and separate `core_edges` / `branch_edges` so core and branches draw at different intensities. `strike_pulse` is 1.0 on the strike frame (feed node.envelope_follower_ar for a flash envelope); `age` counts frames since the last strike (-1 before the first).",
    inputs: {
        strike: ScalarF32 optional,
        x0: ScalarF32 optional,
        y0: ScalarF32 optional,
        x1: ScalarF32 optional,
        y1: ScalarF32 optional,
    },
    outputs: {
        points: Array(CurvePoint),
        widths: Array(f32),
        core_edges: Array(EdgePair),
        branch_edges: Array(EdgePair),
        age: ScalarF32,
        strike_pulse: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("strike"),
            label: "Strike",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("x0"),
            label: "Start X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("y0"),
            label: "Start Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.45),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("x1"),
            label: "End X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("y1"),
            label: "End Y",
            ty: ParamType::Float,
            default: ParamValue::Float(-0.45),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("jag"),
            label: "Jaggedness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.35),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("branch_count"),
            label: "Branches",
            ty: ParamType::Int,
            default: ParamValue::Float(5.0),
            range: Some((0.0, 12.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("branch_decay"),
            label: "Branch Decay",
            ty: ParamType::Float,
            default: ParamValue::Float(0.55),
            range: Some((0.1, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("detail"),
            label: "Detail",
            ty: ParamType::Int,
            default: ParamValue::Float(6.0),
            range: Some((3.0, 8.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("reach"),
            label: "Reach",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("auto_strike_beats"),
            label: "Auto Strike (beats)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("seed_mode"),
            label: "Seed Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 1.0)),
            enum_values: SEED_MODES,
        },
        ParamDef {
            name: Cow::Borrowed("seed"),
            label: "Seed",
            ty: ParamType::Int,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 100_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Points",
            ty: ParamType::Int,
            default: ParamValue::Float(2048.0),
            range: Some((64.0, 16384.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Coordinates are draw_lines pre-aspect curve space (origin-centred, y ±0.5); defaults strike top→bottom for the portrait rig. `reach` scatters both endpoints' x by ±reach/2 per strike so consecutive bolts land in different places. The bolt is visible for one frame only — compose the draws through node.feedback (Max) × node.scale_offset_image decay for the afterglow, and drive node.flash from strike_pulse → envelope_follower_ar for the frame kick. Capacity: points/widths/core_edges/branch_edges buffers are all pre-allocated at max_capacity; generation stops emitting branches when full.",
    examples: ["Lightning"],
    picker: { label: "Lightning Bolt", category: Atom },
    summary: "Grows a jagged lightning bolt with branches each time it is struck — thick at the trunk, hairline at the tips. Feed its points and edges into Draw Lines.",
    category: Generate,
    role: Source,
    aliases: ["lightning", "bolt", "electric", "arc", "discharge"],
    boundary_reason: NonGpu,
    extra_fields: {
        points: Vec<CurvePoint> = Vec::new(),
        widths: Vec<f32> = Vec::new(),
        core_edges: Vec<EdgePair> = Vec::new(),
        branch_edges: Vec<EdgePair> = Vec::new(),
        last_strike: Option<u32> = None,
        last_auto_slot: Option<i64> = None,
        strike_counter: u32 = 0,
        // Frames since the last strike; -1.0 before any strike.
        age_frames: f32 = -1.0,
        struck_this_frame: bool = false,
        // Dirty-tracking for the Array writes (no-per-frame-allocation
        // discipline): buffers are written on the strike frame, the
        // sentinel clear is written exactly once on the frame after,
        // and steady-state frames write nothing. `clear_state` resets
        // `initialized` so resize/rebuild (which zero-fills fresh
        // buffers — and EdgePair{0,0} is a VALID edge, not a sentinel)
        // always gets a sentinel rewrite.
        pending_sentinel: bool = false,
        initialized: bool = false,
        sentinel_scratch: Vec<EdgePair> = Vec::new(),
    },
}

/// Everything the pure generator needs; split from `run` so tests can
/// drive it without a GPU context.
struct BoltParams {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    jag: f32,
    branch_count: u32,
    branch_decay: f32,
    detail: u32,
    reach: f32,
    capacity: usize,
}

impl LightningBolt {
    /// Regenerate the bolt geometry into the scratch vecs. Pure
    /// function of (seed, p) — the determinism contract.
    fn generate(&mut self, seed: u32, p: &BoltParams) {
        self.points.clear();
        self.widths.clear();
        self.core_edges.clear();
        self.branch_edges.clear();

        let mut rng = if seed == 0 { 0x9E37_79B9 } else { seed };
        // Warm the stream so consecutive integer seeds decorrelate.
        for _ in 0..4 {
            xorshift(&mut rng);
        }

        // Endpoint scatter: each strike lands somewhere new within
        // ±reach/2 of the configured endpoints.
        let a = [p.x0 + rand_pm(&mut rng) * p.reach * 0.5, p.y0];
        let b = [p.x1 + rand_pm(&mut rng) * p.reach * 0.5, p.y1];

        // ── Core: midpoint displacement ──
        let mut core = vec![a, b];
        subdivide(&mut rng, &mut core, p.detail, p.jag);
        let core_len = core.len();
        if core_len.min(p.capacity) < 2 {
            return;
        }
        let emit = core_len.min(p.capacity);
        for (i, pt) in core.iter().take(emit).enumerate() {
            let t = i as f32 / (core_len - 1) as f32;
            self.points.push(CurvePoint { xy: *pt });
            self.widths.push(1.0 + (CORE_END_WIDTH - 1.0) * t);
        }
        for i in 0..emit - 1 {
            self.core_edges.push(EdgePair { a: i as u32, b: (i + 1) as u32 });
        }

        // ── Branches: two generations off the core ──
        let bolt_span = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        let branch_detail = p.detail.saturating_sub(2).max(2);
        for _ in 0..p.branch_count {
            let t = 0.15 + 0.7 * rand01(&mut rng);
            let i = ((core_len - 1) as f32 * t) as usize;
            let i = i.clamp(1, core_len.saturating_sub(2));
            if i >= emit {
                continue;
            }
            let start = core[i];
            let tangent = norm2(sub2(core[i + 1], core[i - 1]));
            let side = if rand01(&mut rng) < 0.5 { 1.0 } else { -1.0 };
            let angle = side * (0.35 + 0.4 * rand01(&mut rng));
            let dir = rotate2(tangent, angle);
            let len = bolt_span * (1.0 - t) * (0.25 + 0.35 * rand01(&mut rng));
            let width = (1.0 + (CORE_END_WIDTH - 1.0) * t) * p.branch_decay;

            let first = self.emit_branch(&mut rng, start, dir, len, width, branch_detail, p);

            // Second generation: one sub-branch from the branch's
            // interior, decayed again — hierarchical filaments are
            // the anti-cheese tell of real lightning.
            if let Some((b_start, b_dir)) = first
                && rand01(&mut rng) < 0.6
            {
                let sub_angle = if rand01(&mut rng) < 0.5 { 0.5 } else { -0.5 };
                self.emit_branch(
                    &mut rng,
                    b_start,
                    rotate2(b_dir, sub_angle),
                    len * 0.5,
                    width * p.branch_decay,
                    branch_detail.saturating_sub(1).max(2),
                    p,
                );
            }
        }
    }

    /// Emit one displaced branch polyline into points/widths/
    /// branch_edges (capacity-guarded). Returns a (midpoint, direction)
    /// pair usable as a sub-branch origin, or None if skipped.
    fn emit_branch(
        &mut self,
        rng: &mut u32,
        start: [f32; 2],
        dir: [f32; 2],
        len: f32,
        width: f32,
        detail: u32,
        p: &BoltParams,
    ) -> Option<([f32; 2], [f32; 2])> {
        if len <= 1e-4 || width <= TIP_WIDTH {
            return None;
        }
        let end = [start[0] + dir[0] * len, start[1] + dir[1] * len];
        let mut poly = vec![start, end];
        // Branches jag a little harder than the trunk — thin wild
        // filaments vs the more direct main channel.
        subdivide(rng, &mut poly, detail, p.jag * 1.3);
        if self.points.len() + poly.len() > p.capacity {
            return None;
        }
        let base = self.points.len() as u32;
        let n = poly.len();
        for (i, pt) in poly.iter().enumerate() {
            let t = i as f32 / (n - 1) as f32;
            self.points.push(CurvePoint { xy: *pt });
            self.widths.push(width + (TIP_WIDTH - width) * t);
        }
        for i in 0..n - 1 {
            self.branch_edges.push(EdgePair { a: base + i as u32, b: base + i as u32 + 1 });
        }
        let mid = poly[n / 2];
        Some((mid, dir))
    }
}

fn sub2(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn norm2(v: [f32; 2]) -> [f32; 2] {
    let l = (v[0] * v[0] + v[1] * v[1]).sqrt().max(1e-6);
    [v[0] / l, v[1] / l]
}

fn rotate2(v: [f32; 2], angle: f32) -> [f32; 2] {
    let (s, c) = angle.sin_cos();
    [v[0] * c - v[1] * s, v[0] * s + v[1] * c]
}

/// In-place midpoint displacement: `levels` rounds of subdividing
/// every segment at its midpoint, displaced along the segment normal
/// by ±jag·segment_length·0.6. Displacement amplitude halves per
/// level with the segment length — the classic 1/f bolt profile.
fn subdivide(rng: &mut u32, poly: &mut Vec<[f32; 2]>, levels: u32, jag: f32) {
    for _ in 0..levels {
        let mut next = Vec::with_capacity(poly.len() * 2 - 1);
        for w in poly.windows(2) {
            let (a, b) = (w[0], w[1]);
            let seg = sub2(b, a);
            let seg_len = (seg[0] * seg[0] + seg[1] * seg[1]).sqrt();
            let n = norm2([-seg[1], seg[0]]);
            let disp = rand_pm(rng) * jag * seg_len * 0.6;
            next.push(a);
            next.push([
                (a[0] + b[0]) * 0.5 + n[0] * disp,
                (a[1] + b[1]) * 0.5 + n[1] * disp,
            ]);
        }
        next.push(*poly.last().expect("polyline never empty"));
        *poly = next;
    }
}

impl Primitive for LightningBolt {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        let capacity = match params.get("max_capacity") {
            Some(ParamValue::Float(f)) => *f as u32,
            _ => 2048,
        }
        .clamp(64, 16384);
        match port_name {
            "points" | "widths" | "core_edges" | "branch_edges" => Some(capacity),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let strike_count = ctx.scalar_or_param("strike", 0.0).round().max(0.0) as u32;
        let auto_beats = match ctx.params.get("auto_strike_beats") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

        // Strike detection: rising trigger count (first observation
        // arms without firing, per the cross-primitive convention) OR
        // a new auto-strike beat slot.
        let mut struck = match self.last_strike {
            Some(last) => strike_count > last,
            None => false,
        };
        self.last_strike = Some(strike_count);
        if auto_beats > 1e-3 {
            let slot = (ctx.time.beats.0 / auto_beats as f64).floor() as i64;
            if self.last_auto_slot != Some(slot) {
                struck |= self.last_auto_slot.is_some();
                self.last_auto_slot = Some(slot);
            }
        }

        let float_param = |name: &str, default: f32| match ctx.params.get(name) {
            Some(ParamValue::Float(f)) => *f,
            _ => default,
        };

        if struck {
            self.strike_counter = self.strike_counter.wrapping_add(1);
            let seed_mode = match ctx.params.get("seed_mode") {
                Some(ParamValue::Enum(e)) => *e,
                _ => 0,
            };
            let base_seed = float_param("seed", 1.0).max(0.0) as u32;
            let seed = if seed_mode == 1 {
                base_seed
            } else {
                base_seed
                    .wrapping_mul(0x0100_0193)
                    .wrapping_add(self.strike_counter.wrapping_mul(0x9E37_79B9))
            };

            let capacity = self
                .array_output_capacity("points", ctx.params, &[])
                .unwrap_or(2048) as usize;
            let p = BoltParams {
                x0: ctx.scalar_or_param("x0", 0.0),
                y0: ctx.scalar_or_param("y0", 0.45),
                x1: ctx.scalar_or_param("x1", 0.0),
                y1: ctx.scalar_or_param("y1", -0.45),
                jag: float_param("jag", 0.35).clamp(0.0, 1.0),
                branch_count: float_param("branch_count", 5.0).max(0.0) as u32,
                branch_decay: float_param("branch_decay", 0.55).clamp(0.1, 1.0),
                detail: (float_param("detail", 6.0) as u32).clamp(3, 8),
                reach: float_param("reach", 0.3).clamp(0.0, 1.0),
                capacity,
            };
            self.generate(seed, &p);
            self.age_frames = 0.0;
        } else if self.age_frames >= 0.0 {
            self.age_frames += 1.0;
        }
        self.struck_this_frame = struck;

        ctx.outputs
            .set_scalar("age", ParamValue::Float(self.age_frames));
        ctx.outputs.set_scalar(
            "strike_pulse",
            ParamValue::Float(if struck { 1.0 } else { 0.0 }),
        );

        // ── Write the Array outputs (dirty-tracked, no steady-state
        // work) ──
        // Strike frame: points/widths + live edge topologies. Frame
        // after: one sentinel clear. First frame of a fresh/rebuilt
        // instance: sentinel clear too, because freshly allocated
        // buffers are zero-filled and EdgePair{0,0} is a valid edge
        // (a stack of dots at point 0), not a sentinel. Every other
        // frame: no writes at all.
        let write_sentinels = if struck {
            if let Some(dst) = ctx.outputs.array("points") {
                let cap = (dst.size as usize) / std::mem::size_of::<CurvePoint>();
                let n = self.points.len().min(cap);
                if n > 0 {
                    // Safety (all writes below): shared-memory buffer
                    // pre-allocated by the chain build; write clamped
                    // to capacity; sequential executor on the content
                    // thread means no GPU race.
                    unsafe {
                        dst.write(0, bytemuck::cast_slice(&self.points[..n]));
                    }
                }
            }
            if let Some(dst) = ctx.outputs.array("widths") {
                let cap = (dst.size as usize) / std::mem::size_of::<f32>();
                let n = self.widths.len().min(cap);
                if n > 0 {
                    unsafe {
                        dst.write(0, bytemuck::cast_slice(&self.widths[..n]));
                    }
                }
            }
            self.pending_sentinel = true;
            self.initialized = true;
            false
        } else if self.pending_sentinel {
            self.pending_sentinel = false;
            true
        } else if !self.initialized {
            self.initialized = true;
            true
        } else {
            false
        };

        if struck || write_sentinels {
            // Staged in the reusable scratch — event-rate fills only
            // (strike / clear frames), zero steady-state work.
            let mut scratch = std::mem::take(&mut self.sentinel_scratch);
            for (port, live) in [
                ("core_edges", &self.core_edges),
                ("branch_edges", &self.branch_edges),
            ] {
                let Some(dst) = ctx.outputs.array(port) else { continue };
                let cap = (dst.size as usize) / std::mem::size_of::<EdgePair>();
                if scratch.len() != cap {
                    scratch = vec![EdgePair::SENTINEL; cap];
                } else {
                    scratch.fill(EdgePair::SENTINEL);
                }
                if struck {
                    let n = live.len().min(cap);
                    scratch[..n].copy_from_slice(&live[..n]);
                }
                unsafe {
                    dst.write(0, bytemuck::cast_slice(&scratch));
                }
            }
            self.sentinel_scratch = scratch;
        }
    }

    fn clear_state(&mut self) {
        self.points.clear();
        self.widths.clear();
        self.core_edges.clear();
        self.branch_edges.clear();
        self.last_strike = None;
        self.last_auto_slot = None;
        self.strike_counter = 0;
        self.age_frames = -1.0;
        self.struck_this_frame = false;
        // Force a sentinel rewrite on the next frame: rebuild/resize
        // hands us zero-filled buffers, and EdgePair{0,0} is a valid
        // edge, not a sentinel.
        self.pending_sentinel = false;
        self.initialized = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    fn params() -> BoltParams {
        BoltParams {
            x0: 0.0,
            y0: 0.45,
            x1: 0.0,
            y1: -0.45,
            jag: 0.35,
            branch_count: 5,
            branch_decay: 0.55,
            detail: 6,
            reach: 0.3,
            capacity: 2048,
        }
    }

    #[test]
    fn declares_bolt_ports_and_registers() {
        use crate::node_graph::ports::{ArrayType, PortType};
        assert_eq!(LightningBolt::TYPE_ID, "node.lightning_bolt");
        let names: Vec<&str> = LightningBolt::OUTPUTS.iter().map(|o| o.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["points", "widths", "core_edges", "branch_edges", "age", "strike_pulse"]
        );
        assert_eq!(
            LightningBolt::OUTPUTS[0].ty,
            PortType::Array(ArrayType::of_known::<CurvePoint>())
        );
        assert_eq!(
            LightningBolt::OUTPUTS[1].ty,
            PortType::Array(ArrayType::of_known::<f32>())
        );
        assert_eq!(
            LightningBolt::OUTPUTS[2].ty,
            PortType::Array(ArrayType::of_known::<EdgePair>())
        );
        let prim = LightningBolt::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.lightning_bolt");
    }

    /// The determinism contract: identical (seed, params) → identical
    /// polylines, widths, and topology, across two fresh generations.
    #[test]
    fn fixed_seed_generates_identical_bolt_twice() {
        let p = params();
        let mut a = LightningBolt::new();
        a.generate(42, &p);
        let mut b = LightningBolt::new();
        b.generate(42, &p);
        assert!(!a.points.is_empty());
        assert_eq!(a.points.len(), b.points.len());
        for (pa, pb) in a.points.iter().zip(&b.points) {
            assert_eq!(pa.xy, pb.xy);
        }
        assert_eq!(a.widths, b.widths);
        assert_eq!(a.core_edges.len(), b.core_edges.len());
        assert_eq!(a.branch_edges.len(), b.branch_edges.len());
        for (ea, eb) in a.core_edges.iter().zip(&b.core_edges) {
            assert_eq!((ea.a, ea.b), (eb.a, eb.b));
        }
        for (ea, eb) in a.branch_edges.iter().zip(&b.branch_edges) {
            assert_eq!((ea.a, ea.b), (eb.a, eb.b));
        }
    }

    #[test]
    fn different_seeds_generate_different_bolts() {
        let p = params();
        let mut a = LightningBolt::new();
        a.generate(1, &p);
        let mut b = LightningBolt::new();
        b.generate(2, &p);
        let same = a.points.len() == b.points.len()
            && a.points.iter().zip(&b.points).all(|(x, y)| x.xy == y.xy);
        assert!(!same, "seeds 1 and 2 must not produce identical bolts");
    }

    /// Core structure: detail=6 → 2^6 segments = 65 core points, all
    /// edge indices in range, widths tapering trunk → tip.
    #[test]
    fn core_has_expected_subdivision_and_taper() {
        let mut p = params();
        p.branch_count = 0;
        let mut bolt = LightningBolt::new();
        bolt.generate(7, &p);
        assert_eq!(bolt.points.len(), 65);
        assert_eq!(bolt.core_edges.len(), 64);
        assert!(bolt.branch_edges.is_empty());
        assert_eq!(bolt.widths[0], 1.0);
        assert!((bolt.widths[64] - CORE_END_WIDTH).abs() < 1e-6);
        // Endpoints anchored (jag displaces interiors only) up to reach
        // scatter on x.
        assert_eq!(bolt.points[0].xy[1], 0.45);
        assert_eq!(bolt.points[64].xy[1], -0.45);
        for e in &bolt.core_edges {
            assert!((e.a as usize) < bolt.points.len());
            assert!((e.b as usize) < bolt.points.len());
        }
    }

    #[test]
    fn branches_emit_in_range_edges_with_decayed_widths() {
        let p = params();
        let mut bolt = LightningBolt::new();
        bolt.generate(99, &p);
        assert!(!bolt.branch_edges.is_empty(), "branch_count=5 must emit branches");
        assert_eq!(bolt.points.len(), bolt.widths.len());
        let core_points = 65;
        for e in &bolt.branch_edges {
            assert!((e.a as usize) < bolt.points.len());
            assert!((e.b as usize) < bolt.points.len());
            assert!(e.a as usize >= core_points, "branch edges live after the core block");
        }
        // Every branch width is below the trunk's 1.0 (branch_decay < 1).
        for w in &bolt.widths[core_points..] {
            assert!(*w < 1.0, "branch widths must be decayed, got {w}");
        }
    }

    /// Capacity guard: a tiny buffer never overflows — branches are
    /// dropped, never truncated mid-polyline.
    #[test]
    fn generation_respects_capacity() {
        let mut p = params();
        p.capacity = 80; // room for the 65-point core + one small branch at most
        let mut bolt = LightningBolt::new();
        bolt.generate(3, &p);
        assert!(bolt.points.len() <= 80);
        assert_eq!(bolt.points.len(), bolt.widths.len());
        for e in bolt.core_edges.iter().chain(&bolt.branch_edges) {
            assert!((e.a as usize) < bolt.points.len());
            assert!((e.b as usize) < bolt.points.len());
        }
    }

    #[test]
    fn clear_state_resets_strike_tracking() {
        let mut bolt = LightningBolt::new();
        bolt.generate(5, &params());
        bolt.last_strike = Some(3);
        bolt.age_frames = 12.0;
        Primitive::clear_state(&mut bolt);
        assert!(bolt.points.is_empty());
        assert_eq!(bolt.last_strike, None);
        assert_eq!(bolt.age_frames, -1.0);
    }
}
