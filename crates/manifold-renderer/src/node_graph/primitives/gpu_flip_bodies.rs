//! Ported from FLIP Fluids rigidpressurecoupling.h, rigidboundaryvelocity.cpp and rigidfluidcoupling.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
//! GPU FLIP's dynamic bodies inside the pressure solve
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (solids in the water),
//! LIQUID_SOLVER_SEAM_DESIGN.md D7 (bodies inside the pressure solve)): each
//! body that takes a reaction adds its six velocity unknowns to the solve as
//! the engine's mass-aware PCG does, so the operator is L + ρh·G M⁻¹ Gᵀ
//! (`scripts/mgpcg_reference.py --body`, `body_solve`). The passes, in
//! `shaders/gpu_flip_bodies.wgsl`:
//!
//! - inside every conjugate gradient iteration, the bodies' share of the
//!   operator on the search direction: its pressure impulse per body, then
//!   (1/h)·G·M⁻¹·impulse added to s;
//! - after the projection, the pressure's impulse into the reaction and its
//!   velocity change into the solid velocity.
//!
//! The pressure is a body's only reaction, as in the engine
//! (rigidfluidcoupling.cpp): the constraint's friction acts on the water
//! alone. An explicit friction reaction diverges once ρ·h·f·A_wet/m passes 2.
//!
//! The impulse and the body product run over the pressure solve's fine
//! active tiles (docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md D-9): the solver's
//! level-0 tile buffers come in with every call. The partials are two a
//! tile per body, so they grow with the lattice and the body count.
//!
//! The reaction holds 8 floats per body (linear then angular impulse, N·s and
//! N·m·s about the posed centre of mass), added up over a tick's steps.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
pub(crate) use crate::node_graph::liquid::coupling::REACTION_FLOATS;

const SHADER: &str = include_str!("shaders/gpu_flip_bodies.wgsl");

const THREADS: u64 = 256;
const SUM_FLOATS: u64 = 16;
/// Floats one partial slot holds (six used).
const PARTIAL_FLOATS: u64 = 8;
// Owner codes hold a body index in a byte per axis.
const _: () = assert!(MAX_FLUID_ROLES <= 255);
const SUM_BYTES: u64 = MAX_FLUID_ROLES as u64 * SUM_FLOATS * 4;

/// The pressure solver's fine-level tile buffers the passes read their
/// cells and partial slots through: the gate triples, the flags, the lists.
pub(crate) type Tiles<'a> = [&'a GpuBuffer; 3];

/// Where a solve's gate holds the body product's group counts
/// (gpu_flip_pressure.rs Slots): byte offsets of the impulse partial's
/// triple (the fine level's live workgroups by the bodies), the finalize's
/// (the bodies) and the product's (the fine level's live workgroups). A
/// stopped solve, or an inactive clock slot, zeroes all three.
pub(crate) struct BodyGate<'a> {
    pub buffer: &'a GpuBuffer,
    pub partial: u64,
    pub finalize: u64,
    pub product: u64,
}

/// The bodies a step couples into its solve, and what their passes read.
pub(crate) struct Bodies<'a> {
    pub lattice: [u32; 3],
    pub lattice_min: [f32; 3],
    pub cell_size: f32,
    pub density: f32,
    pub tick_seconds: f32,
    /// This tick's first row in `bodies`, and the bodies from it.
    pub first: u32,
    pub count: u32,
    pub water: &'a GpuBuffer,
    /// Open fractions per face and open volume per cell (the step's `s`).
    pub open: &'a GpuBuffer,
    /// The solid face velocity, friction and owner codes (the step's `v`).
    pub solid: &'a GpuBuffer,
    pub bodies: &'a GpuBuffer,
}

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    n: [u32; 3],
    slots: u32,
    lattice_min: [f32; 3],
    cell_size: f32,
    density: f32,
    tick_seconds: f32,
    first: u32,
    body_count: u32,
    accumulate: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

struct Pipelines {
    partial: GpuComputePipeline,
    finalize: GpuComputePipeline,
    product: GpuComputePipeline,
    velocity: GpuComputePipeline,
    #[cfg(all(test, feature = "gpu-proofs"))]
    poison: GpuComputePipeline,
}

/// The passes' pipelines and their partial sums and per-body sums.
#[derive(Default)]
pub(crate) struct BodyPasses {
    pipelines: Option<Pipelines>,
    partials: Option<GpuBuffer>,
    sums: Option<GpuBuffer>,
    clock_plan: Option<GpuBuffer>,
}

fn records(n: [u32; 3]) -> u64 {
    n.iter().map(|&side| u64::from(side) + 1).product()
}

/// Partial slots per body: two a fine tile (gpu_flip_pressure.rs
/// `partial_count`).
fn partial_slots(n: [u32; 3]) -> u64 {
    2 * n.iter().map(|&side| u64::from(side.div_ceil(super::gpu_flip_step::TILE))).product::<u64>()
}

/// Bytes the partials hold for `count` bodies on lattice `n`.
fn partial_bytes(n: [u32; 3], count: u32) -> u64 {
    u64::from(count.max(1)) * partial_slots(n) * PARTIAL_FLOATS * 4
}

/// Device bytes the passes hold once a step has `count` dynamic bodies on
/// lattice `n`, for the extent proof (gated as `liquid::extent` is).
#[cfg(any(test, feature = "gpu-proofs"))]
pub(crate) fn held_bytes(n: [u32; 3], count: u32) -> u64 {
    partial_bytes(n, count) + SUM_BYTES
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer { binding, buffer, offset: 0 }
}

fn groups(threads: u64) -> [u32; 3] {
    [threads.div_ceil(THREADS).max(1) as u32, 1, 1]
}

/// Every fine tile's two workgroups: what a listed dispatch is launched
/// with; the kernels return whole workgroups past the live count.
fn tile_groups(n: [u32; 3]) -> u32 {
    partial_slots(n) as u32
}

impl BodyPasses {
    fn pipelines(device: &GpuDevice) -> Pipelines {
        let pipe = |entry: &str, label: &str| device.create_compute_pipeline(SHADER, entry, label);
        Pipelines {
            partial: pipe("impulse_partial", "gpu_flip.bodies.partial"),
            finalize: pipe("impulse_finalize", "gpu_flip.bodies.finalize"),
            product: pipe("body_product", "gpu_flip.bodies.product"),
            velocity: pipe("velocity_change", "gpu_flip.bodies.velocity_change"),
            #[cfg(all(test, feature = "gpu-proofs"))]
            poison: pipe("poison_partials", "gpu_flip.bodies.poison"),
        }
    }

    /// Build the passes' pipelines; the owning node calls this at install.
    pub(crate) fn prepare_pipelines(&mut self, device: &GpuDevice) {
        if self.pipelines.is_none() {
            self.pipelines = Some(Self::pipelines(device));
        }
        if self.clock_plan.is_none() {
            let plan = device.create_buffer_shared(48);
            plan.zero_fill();
            self.clock_plan = Some(plan);
        }
    }

    pub(crate) fn set_clock_plan(&mut self, plan: &GpuBuffer) {
        self.clock_plan = Some(plan.clone());
    }

    fn clock_binding(&self) -> GpuBinding<'_> {
        buffer(15, self.clock_plan.as_ref().expect("body clock plan prepared"))
    }

    /// Allocate the sums once and the partials for `count` bodies on
    /// lattice `n`, growing them when a larger lattice or more bodies
    /// arrive. The pipelines come from `prepare_pipelines` at install.
    pub(crate) fn prepare(&mut self, device: &GpuDevice, n: [u32; 3], count: u32) -> Result<(), String> {
        assert!(self.pipelines.is_some(), "body pipelines built by prepare_pipelines at install");
        let need = partial_bytes(n, count);
        if self.partials.as_ref().is_none_or(|p| p.size < need) {
            self.partials = Some(device.try_create_buffer(need)?);
        }
        if self.sums.is_none() {
            // Shared so the value proofs can read the last impulse's sums.
            self.sums = Some(device.try_create_buffer_shared(SUM_BYTES)?);
        }
        Ok(())
    }

    /// The last impulse's sums record per body, for the value proofs.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn sums(&self) -> Option<&GpuBuffer> {
        self.sums.as_ref()
    }

    /// Test-only: NaN into every partial slot of `bodies` after a prepare,
    /// so a finalize that reads a slot the partial pass did not write shows
    /// in the sums.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn poison(&self, enc: &mut GpuEncoder, bodies: &Bodies<'_>) {
        let (pipes, partials, _) = self.parts().expect("the body passes were prepared");
        let params = Self::params(bodies, false);
        enc.dispatch_compute(
            &pipes.poison,
            &[GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) }, buffer(7, partials)],
            groups(u64::from(bodies.count) * partial_slots(bodies.lattice) * PARTIAL_FLOATS),
            "gpu_flip.bodies.poison",
        );
    }

    fn params(bodies: &Bodies<'_>, accumulate: bool) -> Params {
        Params {
            n: bodies.lattice,
            slots: partial_slots(bodies.lattice) as u32,
            lattice_min: bodies.lattice_min,
            cell_size: bodies.cell_size,
            density: bodies.density,
            tick_seconds: bodies.tick_seconds,
            first: bodies.first,
            body_count: bodies.count,
            accumulate: u32::from(accumulate),
            ..Params::default()
        }
    }

    fn parts(&self) -> Result<(&Pipelines, &GpuBuffer, &GpuBuffer), String> {
        match (&self.pipelines, &self.partials, &self.sums) {
            (Some(pipes), Some(partials), Some(sums)) => {
                Ok((pipes, partials, sums))
            }
            _ => Err("the body passes were not prepared".into()),
        }
    }

    /// Each body's pressure impulse from `pressure` into the sums; with
    /// `reaction`, also added into it. With `gate`, on the solve's gate.
    fn impulse(
        &self,
        enc: &mut GpuEncoder,
        bodies: &Bodies<'_>,
        tiles: Tiles<'_>,
        pressure: &GpuBuffer,
        reaction: Option<&GpuBuffer>,
        gate: Option<&BodyGate<'_>>,
    ) -> Result<(), String> {
        let (pipes, partials, sums) = self.parts()?;
        if partials.size < partial_bytes(bodies.lattice, bodies.count) {
            return Err(format!("the body partials were prepared for fewer than {} bodies on {:?}", bodies.count, bodies.lattice));
        }
        let params = Self::params(bodies, reaction.is_some());
        let data = bytemuck::bytes_of(&params);
        let partial = [
            GpuBinding::Bytes { binding: 0, data },
            buffer(1, bodies.water),
            buffer(2, bodies.open),
            buffer(3, bodies.solid),
            buffer(4, bodies.bodies),
            buffer(5, pressure),
            buffer(7, partials),
            buffer(12, tiles[0]),
            buffer(14, tiles[2]),
            self.clock_binding(),
        ];
        let groups = [tile_groups(bodies.lattice), bodies.count.max(1), 1];
        match gate {
            Some(gate) => enc.dispatch_compute_gated(&pipes.partial, &partial, groups, gate.buffer, gate.partial, "gpu_flip.bodies.partial"),
            None => enc.dispatch_compute(&pipes.partial, &partial, groups, "gpu_flip.bodies.partial"),
        }
        // A fixed array, not a Vec: this runs every step. Without a reaction
        // binding 9 is left off.
        let finalize = [
            GpuBinding::Bytes { binding: 0, data },
            buffer(4, bodies.bodies),
            buffer(7, partials),
            buffer(8, sums),
            buffer(13, tiles[1]),
            self.clock_binding(),
            buffer(9, reaction.unwrap_or(sums)),
        ];
        let bound = if reaction.is_some() { finalize.len() } else { finalize.len() - 1 };
        let groups = [bodies.count.max(1), 1, 1];
        match gate {
            Some(gate) => enc.dispatch_compute_gated(&pipes.finalize, &finalize[..bound], groups, gate.buffer, gate.finalize, "gpu_flip.bodies.finalize"),
            None => enc.dispatch_compute(&pipes.finalize, &finalize[..bound], groups, "gpu_flip.bodies.finalize"),
        }
        Ok(())
    }

    /// Inside a conjugate gradient iteration: the bodies' share of the
    /// operator on the search direction `direction`, added to `s`, over the
    /// solver's fine active `tiles`.
    pub(crate) fn apply(
        &self,
        enc: &mut GpuEncoder,
        bodies: &Bodies<'_>,
        tiles: Tiles<'_>,
        direction: &GpuBuffer,
        s: &GpuBuffer,
    ) -> Result<(), String> {
        self.apply_on(enc, bodies, tiles, direction, s, None)
    }

    /// [`Self::apply`] on the solve's gate: a round the stop switched off,
    /// or an inactive clock slot, runs none of its three passes.
    pub(crate) fn apply_gated(
        &self,
        enc: &mut GpuEncoder,
        bodies: &Bodies<'_>,
        tiles: Tiles<'_>,
        direction: &GpuBuffer,
        s: &GpuBuffer,
        gate: &BodyGate<'_>,
    ) -> Result<(), String> {
        self.apply_on(enc, bodies, tiles, direction, s, Some(gate))
    }

    fn apply_on(
        &self,
        enc: &mut GpuEncoder,
        bodies: &Bodies<'_>,
        tiles: Tiles<'_>,
        direction: &GpuBuffer,
        s: &GpuBuffer,
        gate: Option<&BodyGate<'_>>,
    ) -> Result<(), String> {
        self.impulse(enc, bodies, tiles, direction, None, gate)?;
        let (pipes, _, sums) = self.parts()?;
        let params = Self::params(bodies, false);
        let product = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
            buffer(1, bodies.water),
            buffer(2, bodies.open),
            buffer(3, bodies.solid),
            buffer(4, bodies.bodies),
            buffer(8, sums),
            buffer(10, s),
            buffer(12, tiles[0]),
            buffer(14, tiles[2]),
            self.clock_binding(),
        ];
        let groups = [tile_groups(bodies.lattice), 1, 1];
        match gate {
            Some(gate) => enc.dispatch_compute_gated(&pipes.product, &product, groups, gate.buffer, gate.product, "gpu_flip.bodies.product"),
            None => enc.dispatch_compute(&pipes.product, &product, groups, "gpu_flip.bodies.product"),
        }
        Ok(())
    }

    /// After the projection, as the engine finishes its pressure stage: the
    /// pressure's impulse into `reaction` and its velocity change into the
    /// solid velocity (`solid_rw`, the same buffer as `bodies.solid`).
    pub(crate) fn react(
        &self,
        enc: &mut GpuEncoder,
        bodies: &Bodies<'_>,
        tiles: Tiles<'_>,
        pressure: &GpuBuffer,
        reaction: &GpuBuffer,
    ) -> Result<(), String> {
        self.impulse(enc, bodies, tiles, pressure, Some(reaction), None)?;
        let (pipes, _, sums) = self.parts()?;
        let params = Self::params(bodies, false);
        enc.dispatch_compute(
            &pipes.velocity,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                buffer(4, bodies.bodies),
                buffer(8, sums),
                buffer(11, bodies.solid),
                self.clock_binding(),
            ],
            groups(records(bodies.lattice)),
            "gpu_flip.bodies.velocity_change",
        );
        Ok(())
    }
}

/// Why a step's dynamic bodies are refused, or None: every body's owner code
/// must fit a byte, and the reaction must hold every body.
pub(crate) fn body_refusal(count: u32, reaction: Option<&GpuBuffer>) -> Option<String> {
    if count as usize > MAX_FLUID_ROLES {
        return Some(format!("{count} bodies exceed the {MAX_FLUID_ROLES} a liquid holds"));
    }
    let need = u64::from(count) * REACTION_FLOATS as u64 * 4;
    match reaction {
        None => Some("dynamic bodies need the domain's reaction wired".into()),
        Some(buffer) if buffer.size < need => {
            Some(format!("the reaction holds {} bytes, under the {need} {count} bodies need", buffer.size))
        }
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_shader_uses_no_atomics() {
        let stray = super::super::gpu_flip_step::atomic_sites_outside(SHADER, &[]);
        assert!(stray.is_empty(), "atomics outside the allowlist (I8): {stray:#?}");
    }

    #[test]
    fn body_shader_validates_with_every_entry() {
        let module = naga::front::wgsl::parse_str(SHADER).unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let entries: Vec<&str> = module.entry_points.iter().map(|e| e.name.as_str()).collect();
        for entry in ["impulse_partial", "impulse_finalize", "body_product", "velocity_change", "poison_partials"] {
            assert!(entries.contains(&entry), "missing entry {entry}");
        }
    }

    #[test]
    fn params_match_the_shader_uniform() {
        assert_eq!(size_of::<Params>(), 64);
    }

    /// Two slots a tile, partial edge tiles counted, as the solver counts
    /// its fine partials.
    #[test]
    fn partial_slots_are_two_a_tile() {
        assert_eq!(partial_slots([8, 8, 8]), 2);
        assert_eq!(partial_slots([17, 16, 15]), 2 * 3 * 2 * 2);
        assert_eq!(partial_slots([64; 3]), 2 * 512);
        assert_eq!(partial_bytes([64; 3], 0), partial_bytes([64; 3], 1));
        assert_eq!(partial_bytes([64; 3], 3), 3 * 1024 * 32);
    }
}
