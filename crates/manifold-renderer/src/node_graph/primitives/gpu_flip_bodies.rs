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
//! The reaction holds 8 floats per body (linear then angular impulse, N·s and
//! N·m·s about the posed centre of mass), added up over a tick's steps.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
pub(crate) use crate::node_graph::liquid::coupling::REACTION_FLOATS;

const SHADER: &str = include_str!("shaders/gpu_flip_bodies.wgsl");

/// Workgroups per body in a partial sum, at most.
const MAX_GROUPS: u32 = 64;
/// Face records one partial thread covers before another group is added.
const RECORDS_PER_THREAD: u64 = 16;
const THREADS: u64 = 256;
const SUM_FLOATS: u64 = 16;
// Owner codes hold a body index in a byte per axis, and the finalize pass
// covers every body in one workgroup of 64.
const _: () = assert!(MAX_FLUID_ROLES <= 64);
const PARTIAL_BYTES: u64 = MAX_FLUID_ROLES as u64 * MAX_GROUPS as u64 * 8 * 4;
const SUM_BYTES: u64 = MAX_FLUID_ROLES as u64 * SUM_FLOATS * 4;
/// Device bytes the passes hold once a step has dynamic bodies, for the
/// extent proofs.
#[cfg(any(test, feature = "gpu-proofs"))]
pub(crate) const HELD_BYTES: u64 = PARTIAL_BYTES + SUM_BYTES;

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
    groups: u32,
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
}

/// The passes' pipelines and their partial sums and per-body sums.
#[derive(Default)]
pub(crate) struct BodyPasses {
    pipelines: Option<Pipelines>,
    partials: Option<GpuBuffer>,
    sums: Option<GpuBuffer>,
}

fn records(n: [u32; 3]) -> u64 {
    n.iter().map(|&side| u64::from(side) + 1).product()
}

fn cells(n: [u32; 3]) -> u64 {
    n.iter().map(|&side| u64::from(side)).product()
}

/// Workgroups per body for a face grid of `records`.
fn partial_groups(records: u64) -> u32 {
    records.div_ceil(THREADS * RECORDS_PER_THREAD).clamp(1, u64::from(MAX_GROUPS)) as u32
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer { binding, buffer, offset: 0 }
}

fn groups(threads: u64) -> [u32; 3] {
    [threads.div_ceil(THREADS).max(1) as u32, 1, 1]
}

impl BodyPasses {
    fn pipelines(device: &GpuDevice) -> Pipelines {
        let pipe = |entry: &str, label: &str| device.create_compute_pipeline(SHADER, entry, label);
        Pipelines {
            partial: pipe("impulse_partial", "gpu_flip.bodies.partial"),
            finalize: pipe("impulse_finalize", "gpu_flip.bodies.finalize"),
            product: pipe("body_product", "gpu_flip.bodies.product"),
            velocity: pipe("velocity_change", "gpu_flip.bodies.velocity_change"),
        }
    }

    /// Build the passes' pipelines; the owning node calls this at install.
    pub(crate) fn prepare_pipelines(&mut self, device: &GpuDevice) {
        if self.pipelines.is_none() {
            self.pipelines = Some(Self::pipelines(device));
        }
    }

    /// Allocate the sums once; the partials hold every body at the most
    /// groups, so no lattice reallocates them. The pipelines come from
    /// `prepare_pipelines` at install.
    pub(crate) fn prepare(&mut self, device: &GpuDevice) -> Result<(), String> {
        assert!(self.pipelines.is_some(), "body pipelines built by prepare_pipelines at install");
        if self.partials.is_none() {
            self.partials = Some(device.try_create_buffer(PARTIAL_BYTES)?);
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

    fn params(bodies: &Bodies<'_>, accumulate: bool) -> Params {
        Params {
            n: bodies.lattice,
            groups: partial_groups(records(bodies.lattice)),
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
            (Some(pipes), Some(partials), Some(sums)) => Ok((pipes, partials, sums)),
            _ => Err("the body passes were not prepared".into()),
        }
    }

    /// Each body's pressure impulse from `pressure` into the sums; with
    /// `reaction`, also added into it.
    fn impulse(
        &self,
        enc: &mut GpuEncoder,
        bodies: &Bodies<'_>,
        pressure: &GpuBuffer,
        reaction: Option<&GpuBuffer>,
    ) -> Result<(), String> {
        let (pipes, partials, sums) = self.parts()?;
        let params = Self::params(bodies, reaction.is_some());
        let data = bytemuck::bytes_of(&params);
        enc.dispatch_compute(
            &pipes.partial,
            &[
                GpuBinding::Bytes { binding: 0, data },
                buffer(1, bodies.water),
                buffer(2, bodies.open),
                buffer(3, bodies.solid),
                buffer(4, bodies.bodies),
                buffer(5, pressure),
                buffer(7, partials),
            ],
            [params.groups, bodies.count.max(1), 1],
            "gpu_flip.bodies.partial",
        );
        // A fixed array, not a Vec: this runs every step. Without a reaction
        // binding 9 is left off.
        let finalize = [
            GpuBinding::Bytes { binding: 0, data },
            buffer(4, bodies.bodies),
            buffer(7, partials),
            buffer(8, sums),
            buffer(9, reaction.unwrap_or(sums)),
        ];
        let bound = if reaction.is_some() { finalize.len() } else { finalize.len() - 1 };
        enc.dispatch_compute(&pipes.finalize, &finalize[..bound], [1, 1, 1], "gpu_flip.bodies.finalize");
        Ok(())
    }

    /// Inside a conjugate gradient iteration: the bodies' share of the
    /// operator on the search direction `direction`, added to `s`.
    pub(crate) fn apply(
        &self,
        enc: &mut GpuEncoder,
        bodies: &Bodies<'_>,
        direction: &GpuBuffer,
        s: &GpuBuffer,
    ) -> Result<(), String> {
        self.impulse(enc, bodies, direction, None)?;
        let (pipes, _, sums) = self.parts()?;
        let params = Self::params(bodies, false);
        enc.dispatch_compute(
            &pipes.product,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                buffer(1, bodies.water),
                buffer(2, bodies.open),
                buffer(3, bodies.solid),
                buffer(4, bodies.bodies),
                buffer(8, sums),
                buffer(10, s),
            ],
            groups(cells(bodies.lattice)),
            "gpu_flip.bodies.product",
        );
        Ok(())
    }

    /// After the projection, as the engine finishes its pressure stage: the
    /// pressure's impulse into `reaction` and its velocity change into the
    /// solid velocity (`solid_rw`, the same buffer as `bodies.solid`).
    pub(crate) fn react(
        &self,
        enc: &mut GpuEncoder,
        bodies: &Bodies<'_>,
        pressure: &GpuBuffer,
        reaction: &GpuBuffer,
    ) -> Result<(), String> {
        self.impulse(enc, bodies, pressure, Some(reaction))?;
        let (pipes, _, sums) = self.parts()?;
        let params = Self::params(bodies, false);
        enc.dispatch_compute(
            &pipes.velocity,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                buffer(4, bodies.bodies),
                buffer(8, sums),
                buffer(11, bodies.solid),
            ],
            groups(records(bodies.lattice)),
            "gpu_flip.bodies.velocity_change",
        );
        Ok(())
    }
}

/// Why a step's dynamic bodies are refused, or None: every body's owner code
/// must fit a byte, the finalize pass runs one workgroup of 64, and the
/// reaction must hold every body.
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
        for entry in ["impulse_partial", "impulse_finalize", "body_product", "velocity_change"] {
            assert!(entries.contains(&entry), "missing entry {entry}");
        }
    }

    #[test]
    fn params_match_the_shader_uniform() {
        assert_eq!(size_of::<Params>(), 64);
    }

    #[test]
    fn partial_groups_grow_with_the_face_grid() {
        assert_eq!(partial_groups(1), 1);
        assert_eq!(partial_groups(25 * 25 * 25), 4);
        assert_eq!(partial_groups(65 * 65 * 65), MAX_GROUPS);
    }
}
