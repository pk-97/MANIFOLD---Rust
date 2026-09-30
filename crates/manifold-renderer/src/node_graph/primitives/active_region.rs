//! `node.active_region` — the box a lattice solve runs on this frame
//! (docs/FFT_WATER_SOLVER_DESIGN.md P3c). CPU only: it turns the late
//! readings of node.occupied_bounds into a window of the lattice.
//!
//! The window is the newest readings grown by `pad` on every side, the box
//! around the last `hold` frames' worth of them, each side rounded up to a
//! multiple of `step` so few shapes come up. It grows the frame a reading
//! needs it to and shrinks only once every larger reading has aged out. It is
//! the whole lattice when there is no reading yet, when the newest is more
//! than `max_age` frames old, after `reset_trigger` changes, and for `hold`
//! frames after an escape: a reading whose occupied nodes, grown by one node,
//! were not all inside the window used on the frame the reading was taken.

use std::borrow::Cow;
use std::collections::VecDeque;

use super::cosine_spectrum::lattice_nodes_with;
use super::occupied_bounds::{END_PORTS, MIN_PORTS};
use super::sort_particles_into_cells::float_param;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::state_store::NodeState;

/// A box of lattice nodes: `size` nodes from `origin` on each axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Window {
    pub origin: [u32; 3],
    pub size: [u32; 3],
}

impl Window {
    pub fn whole(nodes: [u32; 3]) -> Self {
        Self { origin: [0; 3], size: nodes }
    }

    /// Whether the nodes [low, end) all lie inside.
    pub fn holds(&self, low: [u32; 3], end: [u32; 3]) -> bool {
        (0..3).all(|a| low[a] >= self.origin[a] && end[a] <= self.origin[a] + self.size[a])
    }
}

/// The node's settings.
#[derive(Clone, Copy, Debug)]
pub(super) struct RegionPolicy {
    pub nodes: [u32; 3],
    pub pad: u32,
    pub step: u32,
    pub hold: u64,
    pub max_age: u64,
}

impl RegionPolicy {
    /// [low, end) grown by `by` nodes on every side, inside the lattice.
    pub fn grow(&self, low: [u32; 3], end: [u32; 3], by: u32) -> ([u32; 3], [u32; 3]) {
        (
            std::array::from_fn(|a| low[a].saturating_sub(by)),
            std::array::from_fn(|a| end[a].saturating_add(by).min(self.nodes[a])),
        )
    }

    /// The window on the ladder that holds [low, end): each side the extent
    /// rounded up to a multiple of `step` (at most the lattice), placed at
    /// `low`, shifted back inside the lattice where it would stick out.
    pub fn snap(&self, low: [u32; 3], end: [u32; 3]) -> Window {
        let size: [u32; 3] = std::array::from_fn(|a| {
            let extent = end[a].saturating_sub(low[a]).max(1);
            (extent.div_ceil(self.step) * self.step).min(self.nodes[a])
        });
        Window { origin: std::array::from_fn(|a| low[a].min(self.nodes[a] - size[a])), size }
    }
}

/// One reading of node.occupied_bounds.
#[derive(Clone, Copy, Debug)]
pub(super) struct Observation {
    pub low: [u32; 3],
    pub end: [u32; 3],
    pub count: u32,
    /// Frames since the reading was taken; at least 1.
    pub age: u64,
}

/// Windows remembered for the escape check; more than any `max_age`.
const HISTORY: usize = 64;

pub struct RegionState {
    frame: u64,
    /// Readings taken before this frame describe a liquid since reset.
    epoch: u64,
    /// The frame the newest reading used was taken on.
    newest: Option<u64>,
    /// (frame, low, end) of each reading's grown box, oldest first.
    targets: VecDeque<(u64, [u32; 3], [u32; 3])>,
    used: [Option<(u64, Window)>; HISTORY],
    last_trigger: Option<i32>,
    escapes: u32,
}

impl RegionState {
    pub fn new() -> Self {
        Self {
            frame: 0,
            epoch: 0,
            newest: None,
            targets: VecDeque::new(),
            used: [None; HISTORY],
            last_trigger: None,
            escapes: 0,
        }
    }

    pub fn escapes(&self) -> u32 {
        self.escapes
    }

    fn reset(&mut self) {
        self.epoch = self.frame;
        self.newest = None;
        self.targets.clear();
    }

    fn used_at(&self, frame: u64) -> Option<Window> {
        self.used[(frame % HISTORY as u64) as usize].and_then(|(at, window)| (at == frame).then_some(window))
    }

    /// This frame's window, from the newest reading (if any) and the reset
    /// trigger's value (if wired). Call once per frame.
    pub fn step(&mut self, policy: &RegionPolicy, observation: Option<Observation>, trigger: Option<f32>) -> Window {
        if let Some(value) = trigger {
            let current = value.round() as i32;
            if self.last_trigger.is_some_and(|last| last != current) {
                self.reset();
            }
            self.last_trigger = Some(current);
        }
        let frame = self.frame;
        self.frame += 1;
        if let Some(seen) = observation.filter(|o| o.age >= 1 && o.age <= frame) {
            let taken = frame - seen.age;
            if taken >= self.epoch && self.newest.is_none_or(|newest| taken > newest) {
                self.newest = Some(taken);
                if seen.count > 0 {
                    let (low, end) = policy.grow(seen.low, seen.end, 1);
                    if self.used_at(taken).is_some_and(|used| !used.holds(low, end)) {
                        self.escapes += 1;
                        self.targets.push_back((frame, [0; 3], policy.nodes));
                    }
                    let (low, end) = policy.grow(seen.low, seen.end, policy.pad);
                    self.targets.push_back((frame, low, end));
                }
            }
        }
        while self.targets.front().is_some_and(|&(at, _, _)| at + policy.hold <= frame) {
            self.targets.pop_front();
        }
        let fresh = self.newest.is_some_and(|taken| frame - taken <= policy.max_age);
        let window = match self.targets.iter().copied().reduce(|(f, l0, e0), (_, l1, e1)| {
            (f, std::array::from_fn(|a| l0[a].min(l1[a])), std::array::from_fn(|a| e0[a].max(e1[a])))
        }) {
            Some((_, low, end)) if fresh => policy.snap(low, end),
            _ => Window::whole(policy.nodes),
        };
        self.used[(frame % HISTORY as u64) as usize] = Some((frame, window));
        window
    }
}

impl Default for RegionState {
    fn default() -> Self {
        Self::new()
    }
}

/// In the StateStore, so a cleared state starts the region over with the
/// whole lattice, as it does node.occupied_bounds' readings.
impl NodeState for RegionState {}

fn int_param(params: &ParamValues, name: &str, default: f32) -> f32 {
    match params.get(name) {
        Some(ParamValue::Float(v)) => *v,
        _ => default,
    }
}

/// The policy from the params: an even lattice, 2 to 1024 a side; whole
/// numbers for the rest, an even step and a hold and max age of at least 1.
pub(super) fn region_policy(params: &ParamValues) -> Option<RegionPolicy> {
    let nodes = lattice_nodes_with(params, 3)?;
    let whole = |name: &str, default: f32, low: f32, high: f32| {
        let v = int_param(params, name, default);
        ((low..=high).contains(&v) && v.fract() == 0.0).then_some(v as u32)
    };
    let pad = whole("pad", 9.0, 0.0, 1024.0)?;
    let step = whole("step", 8.0, 2.0, 1024.0).filter(|s| s % 2 == 0)?;
    let hold = whole("hold", 30.0, 1.0, 600.0)?;
    let max_age = whole("max_age", 3.0, 1.0, (HISTORY - 1) as f32)?;
    Some(RegionPolicy { nodes, pad, step, hold: u64::from(hold), max_age: u64::from(max_age) })
}

const ILLEGAL: &str = "Active Region: lengths must be even, 2 to 1024; pad, step, hold and max age whole numbers, step even";

const fn int(name: &'static str, label: &'static str, default: f32, low: f32, high: f32) -> ParamDef {
    ParamDef {
        name: Cow::Borrowed(name),
        label,
        ty: ParamType::Int,
        default: ParamValue::Float(default),
        range: Some((low, high)),
        enum_values: &[],
    }
}

crate::primitive! {
    name: ActiveRegion,
    type_id: "node.active_region",
    purpose: "The window of a lattice (nodes_x/y/z) a solve runs on this frame, from node.occupied_bounds' late readings: the occupied box grown by pad nodes on every side, joined over the last hold frames, each side rounded up to a multiple of step and kept inside the lattice. The whole lattice with no reading yet, a reading more than max_age frames old, after reset_trigger's whole value changes, and for hold frames after an escape (occupied nodes, grown by one, outside the window used when they were read). Outputs origin_x/y/z and size_x/y/z, and escapes, the count of escapes so far.",
    inputs: {
        min_x: ScalarF32 optional,
        min_y: ScalarF32 optional,
        min_z: ScalarF32 optional,
        end_x: ScalarF32 optional,
        end_y: ScalarF32 optional,
        end_z: ScalarF32 optional,
        count: ScalarF32 optional,
        age: ScalarF32 optional,
        reset_trigger: ScalarF32 optional,
    },
    outputs: {
        origin_x: ScalarF32,
        origin_y: ScalarF32,
        origin_z: ScalarF32,
        size_x: ScalarF32,
        size_y: ScalarF32,
        size_z: ScalarF32,
        escapes: ScalarF32,
    },
    params: [
        float_param!("nodes_x", "Nodes X", 64.0, 2.0, 1024.0),
        float_param!("nodes_y", "Nodes Y", 64.0, 2.0, 1024.0),
        float_param!("nodes_z", "Nodes Z", 64.0, 2.0, 1024.0),
        int("pad", "Pad", 9.0, 0.0, 1024.0),
        int("step", "Step", 8.0, 2.0, 1024.0),
        int("hold", "Hold Frames", 30.0, 1.0, 600.0),
        int("max_age", "Max Age", 3.0, 1.0, 63.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire node.occupied_bounds' eight outputs in, and origin/size into a windowed cosine transform: size into nodes_x/y/z of every atom of the chain, origin into both node.cosine_reorder atoms. Wire the liquid's reset control into reset_trigger too, so a restarted liquid gets the whole lattice until its first reading. Pad must cover the collar a solve needs around the occupied nodes plus the travel between the reading and its use.",
    examples: [],
    picker: { label: "Active Region", category: Atom },
    summary: "Picks the part of a 3D grid worth solving this frame, from where things were a frame ago.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["active box", "window", "region of interest"],
    boundary_reason: NonGpu,
}

const ORIGIN_PORTS: [&str; 3] = ["origin_x", "origin_y", "origin_z"];
const SIZE_PORTS: [&str; 3] = ["size_x", "size_y", "size_z"];

/// The wired reading, if there is one: whole numbers, a box inside the
/// lattice when anything is occupied.
fn observation(ctx: &EffectNodeContext<'_, '_>, nodes: [u32; 3]) -> Result<Option<Observation>, String> {
    let scalar = |name: &str| match ctx.inputs.scalar(name) {
        Some(ParamValue::Float(v)) => Some(v),
        _ => None,
    };
    let Some(age) = scalar("age") else { return Ok(None) };
    let whole = |v: f32| (v.is_finite() && v >= 0.0 && v.fract() == 0.0 && v <= 16_777_216.0).then_some(v as u32);
    let read = |name: &str| scalar(name).and_then(whole).ok_or_else(|| format!("Active Region: {name} is not a whole number"));
    let age = whole(age).ok_or("Active Region: age is not a whole number")?;
    if age == 0 {
        return Ok(None);
    }
    let count = read("count")?;
    let mut low = [0; 3];
    let mut end = [0; 3];
    for a in 0..3 {
        low[a] = read(MIN_PORTS[a])?;
        end[a] = read(END_PORTS[a])?;
        if count > 0 && (low[a] >= end[a] || end[a] > nodes[a]) {
            return Err(format!("Active Region: occupied box {low:?}..{end:?} leaves the {nodes:?} lattice"));
        }
    }
    Ok(Some(Observation { low, end, count, age: u64::from(age) }))
}

impl Primitive for ActiveRegion {
    fn params_refusal(&self, params: &ParamValues) -> Option<String> {
        region_policy(params).is_none().then(|| ILLEGAL.to_string())
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(policy) = region_policy(ctx.params) else {
            ctx.error(ILLEGAL.to_string());
            return;
        };
        let seen = match observation(ctx, policy.nodes) {
            Ok(seen) => seen,
            Err(refusal) => {
                ctx.error(refusal);
                None
            }
        };
        let trigger = match ctx.inputs.scalar("reset_trigger") {
            Some(ParamValue::Float(v)) if v.is_finite() => Some(v),
            _ => None,
        };
        let (node, owner) = (ctx.node_id, ctx.owner_key);
        let Some(store) = ctx.state.as_deref_mut() else {
            ctx.error("Active Region: needs the state store its history lives in".to_string());
            return;
        };
        if store.get::<RegionState>(node, owner).is_none() {
            store.insert(node, owner, RegionState::new());
        }
        let region = store.get::<RegionState>(node, owner).expect("region inserted above");
        let window = region.step(&policy, seen, trigger);
        let escapes = region.escapes();
        for a in 0..3 {
            ctx.outputs.set_scalar(ORIGIN_PORTS[a], ParamValue::Float(window.origin[a] as f32));
            ctx.outputs.set_scalar(SIZE_PORTS[a], ParamValue::Float(window.size[a] as f32));
        }
        ctx.outputs.set_scalar("escapes", ParamValue::Float(escapes as f32));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(n: u32) -> RegionPolicy {
        RegionPolicy { nodes: [n; 3], pad: 9, step: 8, hold: 30, max_age: 3 }
    }

    fn seen(low: [u32; 3], end: [u32; 3], age: u64) -> Option<Observation> {
        Some(Observation { low, end, count: 1, age })
    }

    /// Every window is a legal transform lattice inside the lattice, holds the
    /// newest reading grown by the pad, and never shrinks before `hold`
    /// frames have passed.
    #[test]
    fn active_region_windows_are_legal_and_hold_their_reading() {
        for n in [16u32, 32, 48, 64, 80, 96, 128] {
            let p = policy(n);
            let mut state = RegionState::new();
            let mut seed = 0x9e37_79b9u32;
            let mut rand = |m: u32| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed % m
            };
            let mut last: Option<(u64, Window)> = None;
            for frame in 0..400u64 {
                let a: [u32; 3] = std::array::from_fn(|_| rand(n));
                let b: [u32; 3] = std::array::from_fn(|_| rand(n));
                let low: [u32; 3] = std::array::from_fn(|i| a[i].min(b[i]));
                let end: [u32; 3] = std::array::from_fn(|i| a[i].max(b[i]) + 1);
                let observation = (frame > 0).then(|| Observation { low, end, count: 1 + rand(9), age: 1 });
                let w = state.step(&p, observation.filter(|o| o.age <= frame), None);
                for i in 0..3 {
                    assert!(w.size[i] >= 2 && w.size[i].is_multiple_of(2), "{w:?}");
                    assert!(w.origin[i] + w.size[i] <= n, "{w:?} leaves {n}");
                }
                if frame > 0 {
                    let (gl, ge) = p.grow(low, end, p.pad);
                    assert!(w.holds(gl, ge), "{w:?} misses {gl:?}..{ge:?}");
                }
                // Frame 1 leaves the no-reading whole box for the first reading.
                if let Some((_, prev)) = last {
                    let shrank = (0..3).any(|i| w.size[i] < prev.size[i]);
                    assert!(!shrank || frame == 1 || frame >= p.hold, "shrank at {frame}");
                }
                last = Some((frame, w));
            }
        }
    }

    #[test]
    fn active_region_whole_box_without_a_fresh_reading_or_after_reset() {
        let p = policy(64);
        let mut state = RegionState::new();
        assert_eq!(state.step(&p, None, Some(0.0)), Window::whole([64; 3]), "no reading yet");
        let w = state.step(&p, seen([10, 0, 10], [20, 12, 20], 1), Some(0.0));
        assert_eq!(w, Window { origin: [1, 0, 1], size: [32, 24, 32] });
        for _ in 0..3 {
            assert_eq!(state.step(&p, seen([10, 0, 10], [20, 12, 20], 1), Some(0.0)), w);
        }
        assert_eq!(state.step(&p, None, Some(1.0)), Window::whole([64; 3]), "reset");
        assert_eq!(
            state.step(&p, seen([10, 0, 10], [20, 12, 20], 2), Some(1.0)),
            Window::whole([64; 3]),
            "a reading from before the reset"
        );
        let mut stale = RegionState::new();
        stale.step(&p, None, None);
        stale.step(&p, seen([10, 0, 10], [20, 12, 20], 1), None);
        for _ in 0..3 {
            stale.step(&p, None, None);
        }
        assert_eq!(stale.step(&p, None, None), Window::whole([64; 3]), "reading older than max_age");
    }

    #[test]
    fn active_region_escape_forces_the_whole_box() {
        let p = policy(64);
        let mut state = RegionState::new();
        state.step(&p, None, None);
        let w = state.step(&p, seen([10, 0, 10], [12, 4, 12], 1), None);
        assert!(w.size[0] < 64);
        // Taken on the frame that used `w`, but reaching past it.
        let escaped = state.step(&p, seen([10, 0, 10], [40, 4, 12], 1), None);
        assert_eq!(state.escapes(), 1);
        assert_eq!(escaped, Window::whole([64; 3]));
        for _ in 0..p.hold - 1 {
            assert_eq!(state.step(&p, seen([10, 0, 10], [12, 4, 12], 1), None), Window::whole([64; 3]));
        }
    }
}
