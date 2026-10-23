//! The GPU side of the shared force lattice: the stable buffers a domain
//! publishes, the staging ring that fills them, the field scalars an atom
//! binds and the trilinear WGSL include. The CPU lattice is
//! `manifold_physics::force_lattice::ForceLattice`.

use manifold_gpu::{FrameClock, GpuBuffer};
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_physics::force_lattice::{FORCE_LATTICE_RESERVE, FORCE_NODE_FLOATS, ForceLattice};

/// Trilinear reads of a force lattice; pure math, included by each atom that
/// reads one (its own buffer reads stay in its body).
pub const FORCE_LATTICE: &str = include_str!("shaders/force_lattice.wgsl");

/// Staging slots: one per frame that writes, three frames in flight plus one.
pub const STAGING_SLOTS: usize = 4;

/// The field scalars and lattices an atom binds: wired lattices too small
/// for the wired field are refused by name; unwired ones read nothing.
pub struct ForceLatticeBinding<'a> {
    pub nodes: [i32; 3],
    pub spacing: f32,
    /// The lattice's first node, from `field_min_x/y/z`; 0 where an atom
    /// places the lattice itself.
    pub min: [f32; 3],
    pub force_lattices: i32,
    pub impulse_tick: i32,
    pub first_tick: i32,
    pub forces: Option<&'a GpuBuffer>,
    pub impulses: Option<&'a GpuBuffer>,
}

impl<'a> ForceLatticeBinding<'a> {
    pub fn read(
        ctx: &EffectNodeContext<'_, '_>,
        forces: Option<&'a GpuBuffer>,
        impulses: Option<&'a GpuBuffer>,
        atom: &str,
    ) -> Result<Self, String> {
        let nodes = ["field_nodes_x", "field_nodes_y", "field_nodes_z"]
            .map(|name| ctx.scalar_or_param(name, 2.0).round().max(2.0) as i32);
        let spacing = ctx.scalar_or_param("field_spacing", 0.25);
        let min = ["field_min_x", "field_min_y", "field_min_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let force_lattices =
            if forces.is_some() { ctx.scalar_or_param("force_lattices", 0.0).round().max(0.0) as i32 } else { 0 };
        let impulse_tick = if impulses.is_some() { ctx.scalar_or_param("impulse_tick", -1.0).round().max(-1.0) as i32 } else { -1 };
        let first_tick = ctx.scalar_or_param("first_tick", 0.0).round().max(0.0) as i32;
        let node_count = nodes.iter().map(|&n| n as u64).product::<u64>();
        for (name, buffer, lattices, node_bytes) in [
            ("forces", forces, force_lattices.max(0) as u64, FORCE_NODE_FLOATS as u64 * 4),
            ("impulses", impulses, u64::from(impulse_tick >= 0), 16),
        ] {
            if lattices > 0 && buffer.is_some_and(|buffer| buffer.size < lattices * node_count * node_bytes) {
                return Err(format!(
                    "{atom}: the {name} buffer holds fewer than {lattices} lattice(s) of {} × {} × {} field nodes; wire the domain's {name} and field scalars",
                    nodes[0], nodes[1], nodes[2]
                ));
            }
        }
        if (force_lattices > 0 || impulse_tick >= 0) && !(spacing.is_finite() && spacing > 0.0) {
            return Err(format!("{atom}: field_spacing must be positive"));
        }
        if min.iter().any(|v| !v.is_finite()) {
            return Err(format!("{atom}: field_min must be finite"));
        }
        Ok(Self { nodes, spacing, min, force_lattices, impulse_tick, first_tick, forces, impulses })
    }
}

/// What one upload copies: the lattices back to back, each with its own
/// dirty flag, the hit records and the frame's tick lengths (both copied
/// whenever there are any).
pub struct LatticeUpload<'a> {
    pub lattice: ForceLattice,
    pub forces: &'a [[f32; 4]],
    pub forces_dirty: bool,
    pub impulses: &'a [[f32; 4]],
    pub impulses_dirty: bool,
    pub hits: &'a [[u32; 4]],
    /// Each of this frame's ticks' simulated seconds, for an atom that runs
    /// every tick in one dispatch; empty for a domain that dispatches per
    /// tick with the length in its uniforms.
    pub tick_seconds: &'a [f32],
    /// The far terms beside the force lattices, copied with them; empty for
    /// a domain that reads only its lattice (`manifold_physics::force_lattice::FarForces`).
    pub far: &'a [[f32; 4]],
}

/// One staging slot: the impulse lattice, the force lattices, the hit
/// records then the tick lengths, written by the CPU, copied into the stable
/// buffers by the GPU.
struct StagingSlot {
    buffer: GpuBuffer,
    /// Frame-clock stamp of the frame that copied from it; 0 never read.
    read_stamp: u64,
}

/// What the stable buffers and every staging slot hold room for. Live
/// frames fit the reserve made up front; only offline catch-up (more ticks
/// than a live frame runs) or a new lattice grows it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Capacity {
    lattice: ForceLattice,
    /// Force lattices.
    forces: usize,
    /// Impulse records, four floats each: whole lattices of the field nodes.
    impulses: usize,
    hits: usize,
    ticks: usize,
    /// Far-term words, four floats each.
    far: usize,
}

impl Capacity {
    /// Room for `needed` and at least `FORCE_LATTICE_RESERVE` force
    /// lattices, impulse lattices and tick lengths, so a swarm's first hit,
    /// or a later hit frame with more ticks, finds the room already there.
    fn reserve(needed: Capacity) -> Self {
        let reserve = FORCE_LATTICE_RESERVE as usize;
        Self {
            lattice: needed.lattice,
            forces: needed.forces.max(reserve),
            impulses: needed.impulses.max(reserve * needed.lattice.node_count()),
            hits: needed.hits,
            ticks: needed.ticks.max(reserve),
            far: needed.far,
        }
    }

    fn holds(&self, needed: &Capacity) -> bool {
        self.lattice == needed.lattice
            && self.forces >= needed.forces
            && self.impulses >= needed.impulses
            && self.hits >= needed.hits
            && self.ticks >= needed.ticks
            && self.far >= needed.far
    }

    /// Staging layout: impulses, forces, hit records, tick lengths, far terms.
    fn impulse_bytes(&self) -> u64 {
        (self.impulses as u64 * 16).max(16)
    }

    fn hits_offset(&self) -> u64 {
        self.impulse_bytes() + self.forces as u64 * self.lattice.force_bytes()
    }

    fn ticks_offset(&self) -> u64 {
        self.hits_offset() + (self.hits as u64 * 16).max(16)
    }

    fn far_offset(&self) -> u64 {
        self.ticks_offset() + self.ticks as u64 * 4
    }

    fn far_bytes(&self) -> u64 {
        (self.far as u64 * 16).max(16)
    }

    fn staging_bytes(&self) -> u64 {
        self.far_offset() + self.far as u64 * 16
    }
}

struct Buffers {
    capacity: Capacity,
    hits: GpuBuffer,
    tick_seconds: GpuBuffer,
    forces: GpuBuffer,
    impulses: GpuBuffer,
    far: GpuBuffer,
    staging: Vec<StagingSlot>,
}

/// The stable `forces`, `impulses` and hit buffers a domain publishes, and
/// the staging ring that fills them in encoder order.
#[derive(Default)]
pub struct LatticeBuffers {
    clock: Option<FrameClock>,
    gpu: Option<Buffers>,
}

impl LatticeBuffers {
    pub fn forces(&self) -> Option<&GpuBuffer> {
        self.gpu.as_ref().map(|gpu| &gpu.forces)
    }

    pub fn impulses(&self) -> Option<&GpuBuffer> {
        self.gpu.as_ref().map(|gpu| &gpu.impulses)
    }

    pub fn hits(&self) -> Option<&GpuBuffer> {
        self.gpu.as_ref().map(|gpu| &gpu.hits)
    }

    pub fn tick_seconds(&self) -> Option<&GpuBuffer> {
        self.gpu.as_ref().map(|gpu| &gpu.tick_seconds)
    }

    pub fn far(&self) -> Option<&GpuBuffer> {
        self.gpu.as_ref().map(|gpu| &gpu.far)
    }

    /// Copy what changed into the stable buffers, in encoder order. Live never
    /// waits for a staging slot; offline waits for the oldest. A resize
    /// rewrites everything.
    pub fn upload(&mut self, gpu: &mut GpuEncoder<'_>, offline: bool, data: LatticeUpload<'_>) -> Result<(), String> {
        let LatticeUpload { lattice, forces, impulses, hits, tick_seconds, far, .. } = data;
        let (mut forces_dirty, mut impulses_dirty) = (data.forces_dirty, data.impulses_dirty);
        if self.clock.is_none() {
            self.clock = gpu.device.frame_clock();
        }
        let bytes = lattice.force_bytes();
        let force_bytes = (forces.len() * 16) as u64;
        let impulse_bytes = (impulses.len() * 16) as u64;
        let hit_bytes = (hits.len() * 16) as u64;
        let tick_bytes = (tick_seconds.len() * 4) as u64;
        let far_bytes = (far.len() * 16) as u64;
        let needed = Capacity {
            lattice,
            // Two [f32; 4] per force node.
            forces: forces.len() / (2 * lattice.node_count()).max(1),
            impulses: impulses.len(),
            hits: hits.len(),
            ticks: tick_seconds.len(),
            far: far.len(),
        };
        if self.gpu.as_ref().is_none_or(|buffers| !buffers.capacity.holds(&needed)) {
            let capacity = Capacity::reserve(needed);
            let create = |what: &str, size: u64| {
                gpu.device
                    .try_create_buffer_shared(size)
                    .map_err(|error| format!("Force lattice: the {what} need {size} bytes: {error}"))
            };
            let forces_buffer = create("forces", capacity.forces as u64 * bytes)?;
            let impulses_buffer = create("impulses", capacity.impulse_bytes())?;
            let hits_buffer = create("hits", (capacity.hits as u64 * 16).max(16))?;
            let ticks_buffer = create("tick lengths", capacity.ticks as u64 * 4)?;
            let far_buffer = create("far terms", capacity.far_bytes())?;
            forces_buffer.zero_fill();
            impulses_buffer.zero_fill();
            hits_buffer.zero_fill();
            ticks_buffer.zero_fill();
            far_buffer.zero_fill();
            let staging = (0..STAGING_SLOTS)
                .map(|_| create("staging lattices", capacity.staging_bytes()).map(|buffer| StagingSlot { buffer, read_stamp: 0 }))
                .collect::<Result<_, _>>()?;
            self.gpu = Some(Buffers {
                capacity,
                hits: hits_buffer,
                tick_seconds: ticks_buffer,
                forces: forces_buffer,
                impulses: impulses_buffer,
                far: far_buffer,
                staging,
            });
            forces_dirty = true;
            impulses_dirty = true;
        }
        if !forces_dirty && !impulses_dirty && hits.is_empty() && tick_seconds.is_empty() {
            return Ok(());
        }
        let buffers = self.gpu.as_mut().expect("allocated above");
        let clock = self.clock.as_ref();
        let retired = |slot: &StagingSlot| clock.is_none_or(|clock| clock.is_complete(slot.read_stamp));
        let index = match buffers.staging.iter().position(retired) {
            Some(index) => index,
            None => {
                let (index, stamp) = buffers
                    .staging
                    .iter()
                    .enumerate()
                    .map(|(index, slot)| (index, slot.read_stamp))
                    .min_by_key(|&(_, stamp)| stamp)
                    .expect("the ring has slots");
                if !offline || !clock.is_some_and(|clock| clock.wait(stamp)) {
                    return Err("Force lattice: every staging slot is still on the GPU".into());
                }
                index
            }
        };
        let capacity = buffers.capacity;
        let slot = &mut buffers.staging[index];
        if impulses_dirty && impulse_bytes > 0 {
            // SAFETY: shared storage laid out by `Capacity` (impulses, forces,
            // hit records, tick lengths), each region within its capacity;
            // its last GPU reader has retired (checked or waited above) and
            // the copy below is the next.
            unsafe { slot.buffer.write(0, bytemuck::cast_slice(impulses)) };
            gpu.native_enc.copy_buffer_range(&slot.buffer, 0, &buffers.impulses, 0, impulse_bytes);
        }
        if forces_dirty && force_bytes > 0 {
            let offset = capacity.impulse_bytes();
            // SAFETY: as above, after the impulse region; the capacity holds
            // every force lattice.
            unsafe { slot.buffer.write(offset, bytemuck::cast_slice(forces)) };
            gpu.native_enc.copy_buffer_range(&slot.buffer, offset, &buffers.forces, 0, force_bytes);
        }
        if hit_bytes > 0 {
            let offset = capacity.hits_offset();
            // SAFETY: the retired staging slot includes the hit records.
            unsafe { slot.buffer.write(offset, bytemuck::cast_slice(hits)) };
            gpu.native_enc.copy_buffer_range(&slot.buffer, offset, &buffers.hits, 0, hit_bytes);
        }
        if tick_bytes > 0 {
            let offset = capacity.ticks_offset();
            // SAFETY: the retired staging slot ends with room for every tick
            // length, after the hit records.
            unsafe { slot.buffer.write(offset, bytemuck::cast_slice(tick_seconds)) };
            gpu.native_enc.copy_buffer_range(&slot.buffer, offset, &buffers.tick_seconds, 0, tick_bytes);
        }
        // The far terms are laid with the lattices they sit beside.
        if forces_dirty && far_bytes > 0 {
            let offset = capacity.far_offset();
            // SAFETY: the retired staging slot ends with room for every far
            // term, after the tick lengths.
            unsafe { slot.buffer.write(offset, bytemuck::cast_slice(far)) };
            gpu.native_enc.copy_buffer_range(&slot.buffer, offset, &buffers.far, 0, far_bytes);
        }
        slot.read_stamp = clock.map_or(0, FrameClock::stamp);
        Ok(())
    }
}

/// The scene's force field on `acceleration_field` as this consumer feels it:
/// with the Physics World's still air (`air_resistance`, 1/s), the air felt at
/// the node's Air Drag (`air_drag`, 0 ignores it), one rule for every
/// consumer (D-114). Every node that lays or steps the field reads it here.
pub fn felt_field(ctx: &EffectNodeContext<'_, '_>, owner: &str) -> Result<Option<manifold_physics::FieldValue>, String> {
    let air_drag = ctx.scalar_or_param("air_drag", 1.0);
    let air_resistance = ctx.scalar_or_param("air_resistance", 0.0);
    let forces = ctx.inputs.cpu_value::<manifold_physics::FieldValue>("acceleration_field");
    manifold_physics::FieldValue::felt_in_still_air(forces.as_ref(), air_resistance, air_drag)
        .map_err(|error| format!("{owner}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A swarm uploads before its first hit with no impulse lattice at all.
    /// The reserve made then holds its first hit and a later live hit frame
    /// with more ticks, so neither reallocates a buffer or a staging slot.
    #[test]
    fn lattice_reserve_holds_live_hit_frames() {
        let lattice = ForceLattice::over_box([-2.0; 3], [4.0; 3], 64);
        let nodes = lattice.node_count();
        let frame = |ticks: usize, hit_lattices: usize| Capacity { lattice, forces: ticks, impulses: hit_lattices * nodes, hits: 0, ticks, far: 0 };
        let reserved = Capacity::reserve(frame(1, 0));
        for (ticks, hit_lattices) in [(1, 1), (2, 2), (3, 3)] {
            assert!(reserved.holds(&frame(ticks, hit_lattices)), "{ticks} ticks with hits");
        }
        // Offline catch-up past a live frame grows it; so does a new lattice.
        assert!(!reserved.holds(&frame(8, 8)));
        assert!(!reserved.holds(&Capacity { lattice: ForceLattice::over_box([-2.0; 3], [4.0; 3], 17), ..frame(1, 0) }));
        // The staging regions follow one another without overlap.
        assert!(reserved.impulse_bytes() >= 3 * lattice.impulse_bytes());
        assert_eq!(reserved.hits_offset(), reserved.impulse_bytes() + 3 * lattice.force_bytes());
        assert!(reserved.staging_bytes() >= reserved.ticks_offset() + 3 * 4);
    }
}
