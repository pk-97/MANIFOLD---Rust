//! The GPU side of the shared force lattice: the stable buffers a domain
//! publishes, the staging ring that fills them, the field scalars an atom
//! binds and the trilinear WGSL include. The CPU lattice is
//! `manifold_physics::force_lattice::ForceLattice`.

use manifold_gpu::{FrameClock, GpuBuffer};
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_physics::force_lattice::ForceLattice;

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
        let force_lattices =
            if forces.is_some() { ctx.scalar_or_param("force_lattices", 0.0).round().max(0.0) as i32 } else { 0 };
        let impulse_tick = if impulses.is_some() { ctx.scalar_or_param("impulse_tick", -1.0).round().max(-1.0) as i32 } else { -1 };
        let first_tick = ctx.scalar_or_param("first_tick", 0.0).round().max(0.0) as i32;
        let lattice_bytes = nodes.iter().map(|&n| n as u64).product::<u64>() * 16;
        for (name, buffer, lattices) in
            [("forces", forces, force_lattices.max(0) as u64), ("impulses", impulses, u64::from(impulse_tick >= 0))]
        {
            if lattices > 0 && buffer.is_some_and(|buffer| buffer.size < lattices * lattice_bytes) {
                return Err(format!(
                    "{atom}: the {name} buffer holds fewer than {lattices} lattice(s) of {} × {} × {} field nodes; wire the domain's {name} and field scalars",
                    nodes[0], nodes[1], nodes[2]
                ));
            }
        }
        if (force_lattices > 0 || impulse_tick >= 0) && !(spacing.is_finite() && spacing > 0.0) {
            return Err(format!("{atom}: field_spacing must be positive"));
        }
        Ok(Self { nodes, spacing, force_lattices, impulse_tick, first_tick, forces, impulses })
    }
}

/// What one upload copies: the lattices back to back, each with its own
/// dirty flag, and the hit records (copied whenever there are any).
pub struct LatticeUpload<'a> {
    pub lattice: ForceLattice,
    /// Force lattices the buffer keeps room for even when fewer are live.
    pub reserve_lattices: usize,
    pub forces: &'a [[f32; 4]],
    pub forces_dirty: bool,
    pub impulses: &'a [[f32; 4]],
    pub impulses_dirty: bool,
    pub hits: &'a [[u32; 4]],
}

/// One staging slot: the impulse lattice, the force lattices then the hit
/// records, written by the CPU, copied into the stable buffers by the GPU.
struct StagingSlot {
    buffer: GpuBuffer,
    /// Frame-clock stamp of the frame that copied from it; 0 never read.
    read_stamp: u64,
}

struct Buffers {
    lattice: ForceLattice,
    force_capacity: usize,
    impulse_capacity: usize,
    hit_capacity: usize,
    hits: GpuBuffer,
    forces: GpuBuffer,
    impulses: GpuBuffer,
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

    /// Copy what changed into the stable buffers, in encoder order. Live never
    /// waits for a staging slot; offline waits for the oldest. A resize
    /// rewrites everything.
    pub fn upload(&mut self, gpu: &mut GpuEncoder<'_>, offline: bool, data: LatticeUpload<'_>) -> Result<(), String> {
        let LatticeUpload { lattice, reserve_lattices, forces, impulses, hits, .. } = data;
        let (mut forces_dirty, mut impulses_dirty) = (data.forces_dirty, data.impulses_dirty);
        if self.clock.is_none() {
            self.clock = gpu.device.frame_clock();
        }
        let bytes = lattice.bytes();
        let force_lattices = forces.len() / lattice.node_count().max(1);
        let force_bytes = (forces.len() * 16) as u64;
        let impulse_bytes = (impulses.len() * 16) as u64;
        let hit_bytes = (hits.len() * 16) as u64;
        if self.gpu.as_ref().is_none_or(|buffers| {
            buffers.lattice != lattice
                || buffers.force_capacity < force_lattices
                || buffers.impulse_capacity < impulses.len()
                || buffers.hit_capacity < hits.len()
        }) {
            // Room for a live frame's ticks up front; offline catch-up grows it.
            let force_capacity = force_lattices.max(reserve_lattices);
            let create = |what: &str, size: u64| {
                gpu.device
                    .try_create_buffer_shared(size)
                    .map_err(|error| format!("Force lattice: the {what} need {size} bytes: {error}"))
            };
            let forces_buffer = create("forces", force_capacity as u64 * bytes)?;
            let impulses_buffer = create("impulses", impulse_bytes.max(16))?;
            let hits_buffer = create("hits", hit_bytes.max(16))?;
            forces_buffer.zero_fill();
            impulses_buffer.zero_fill();
            hits_buffer.zero_fill();
            let staging = (0..STAGING_SLOTS)
                .map(|_| {
                    create("staging lattices", impulse_bytes + force_capacity as u64 * bytes + hit_bytes.max(16))
                        .map(|buffer| StagingSlot { buffer, read_stamp: 0 })
                })
                .collect::<Result<_, _>>()?;
            self.gpu = Some(Buffers {
                lattice,
                force_capacity,
                impulse_capacity: impulses.len(),
                hit_capacity: hits.len(),
                hits: hits_buffer,
                forces: forces_buffer,
                impulses: impulses_buffer,
                staging,
            });
            forces_dirty = true;
            impulses_dirty = true;
        }
        if !forces_dirty && !impulses_dirty && hits.is_empty() {
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
        let slot = &mut buffers.staging[index];
        if impulses_dirty && impulse_bytes > 0 {
            // SAFETY: shared storage of (1 + capacity) × `bytes` plus the hit
            // records; its last GPU reader has retired (checked or waited
            // above) and the copy below is the next.
            unsafe { slot.buffer.write(0, bytemuck::cast_slice(impulses)) };
            gpu.native_enc.copy_buffer_range(&slot.buffer, 0, &buffers.impulses, 0, impulse_bytes);
        }
        if forces_dirty && force_bytes > 0 {
            // SAFETY: as above, after the impulse lattice; the capacity holds
            // every force lattice.
            unsafe { slot.buffer.write(impulse_bytes, bytemuck::cast_slice(forces)) };
            gpu.native_enc.copy_buffer_range(&slot.buffer, impulse_bytes, &buffers.forces, 0, force_bytes);
        }
        if hit_bytes > 0 {
            let offset = impulse_bytes + buffers.force_capacity as u64 * bytes;
            // SAFETY: the retired staging slot includes the hit records.
            unsafe { slot.buffer.write(offset, bytemuck::cast_slice(hits)) };
            gpu.native_enc.copy_buffer_range(&slot.buffer, offset, &buffers.hits, 0, hit_bytes);
        }
        slot.read_stamp = clock.map_or(0, FrameClock::stamp);
        Ok(())
    }
}
