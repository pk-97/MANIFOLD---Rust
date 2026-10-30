//! Ported from FLIP Fluids rigidpressurecoupling.h, rigidboundaryvelocity.cpp and rigidfluidcoupling.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
//! GPU FLIP's dynamic bodies inside the pressure solve
//! Each
//! body that takes a reaction adds its six velocity unknowns to the solve as
//! the engine's mass-aware PCG does, so the operator is L + ρh·G M_c Gᵀ
//! (`scripts/mgpcg_reference.py --body`, `body_solve`), M_c the body's
//! mobility held on its supports (static contacts shape the prediction): M⁻¹ for a free body. The passes, in
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
//! active slots: the solver's level-0 buffers come in with every call (the
//! gate triples, the flags indexed by slot, the list of slots). The partials
//! are two a pool slot per body, so they grow with the pool and the body
//! count. The solid velocity change runs over every held slot of the step's
//! world tiles.
//!
//! Every fine-lattice buffer a call takes (the water, open fractions, solid
//! velocity, direction, `s`) is a pooled field of the world tiles in
//! [`Bodies::tiles`], and the kernels index through its table.
//!
//! The reaction holds 8 floats per body (linear then angular impulse, N·s and
//! N·m·s about the posed centre of mass), added up over a tick's steps.

use manifold_gpu::BufferUse;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use super::gpu_flip_pressure::Sink;
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, lattice_origin};
pub(crate) use manifold_water_liquid::coupling::REACTION_FLOATS;
use manifold_water_liquid::world_tiles::{WorldTiles, allocated_args_offset};

const SHADER_BODY: &str = include_str!("shaders/gpu_flip_bodies.wgsl");

/// The kernels with the world tile read side they index the lattice through.
fn shader() -> String {
    format!("{READ_SIDE}\n{SHADER_BODY}")
}

// The poison pass's workgroup size.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
const THREADS: u64 = 256;
const SUM_FLOATS: u64 = 16;
/// Floats one partial slot holds (six used).
const PARTIAL_FLOATS: u64 = 8;
// Owner codes hold a body index in a byte per axis.
const _: () = assert!(MAX_LIQUID_BODIES <= 255);
const SUM_BYTES: u64 = MAX_LIQUID_BODIES as u64 * SUM_FLOATS * 4;

/// The pressure solver's fine-level buffers the passes read their cells and
/// partial slots through: the gate triples, the flags (indexed by slot, zero
/// for every slot not in the list) and the list of active slots.
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
    /// The solve-live triple the finalize checks (gpu_flip_bodies.wgsl
    /// impulse_finalize), so a round run after the stop writes no sums.
    pub live: u32,
}

manifold_core::testkit_visible! {
/// The bodies a step couples into its solve, and what their passes read.
pub(crate) struct Bodies<'a> {
    /// The fine level's world tiles: the table the pooled buffers below are
    /// read through, the pool's size, and the held slots.
    pub tiles: &'a WorldTiles,
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
    /// Six vec4 per row of `bodies`: each body's packed response to an
    /// impulse while it stays on its supports (D16), as `pose_bodies` writes
    /// it.
    pub mobility: &'a GpuBuffer,
}
}

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    origin: [f32; 3],
    cell_size: f32,
    density: f32,
    tick_seconds: f32,
    slots: u32,
    first: u32,
    body_count: u32,
    accumulate: u32,
    /// impulse_finalize inside a round: the gate's solve-live triple; 0 runs always.
    live: u32,
    /// The tile pool's capacity P (gpu_flip_bodies.wgsl ts_pool_tiles); H
    /// is the table's own.
    pool_tiles: u32,
    _pad_h: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

struct Pipelines {
    partial: GpuComputePipeline,
    finalize: GpuComputePipeline,
    product: GpuComputePipeline,
    velocity: GpuComputePipeline,
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    poison: GpuComputePipeline,
}

manifold_core::testkit_visible! {
/// The passes' pipelines and their partial sums and per-body sums.
#[derive(Default)]
pub(crate) struct BodyPasses {
    pipelines: Option<Pipelines>,
    partials: Option<GpuBuffer>,
    sums: Option<GpuBuffer>,
    clock_plan: Option<GpuBuffer>,
}
}

/// Partial slots per body: two a pool slot (gpu_flip_pressure.rs
/// `partial_count`).
fn partial_slots(pool_tiles: u32) -> u64 {
    2 * u64::from(pool_tiles)
}

/// Bytes the partials hold for `count` bodies on a pool of `pool_tiles`.
fn partial_bytes(pool_tiles: u32, count: u32) -> u64 {
    u64::from(count.max(1)) * partial_slots(pool_tiles) * PARTIAL_FLOATS * 4
}

/// Device bytes the passes hold once a step has `count` dynamic bodies on a
/// pool of `pool_tiles`, for the extent proof (gated as `liquid::extent` is).
#[cfg(any(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub(crate) fn held_bytes(pool_tiles: u32, count: u32) -> u64 {
    partial_bytes(pool_tiles, count) + SUM_BYTES
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
fn groups(threads: u64) -> [u32; 3] {
    [threads.div_ceil(THREADS).max(1) as u32, 1, 1]
}

/// Every pool slot's two workgroups: what a listed dispatch is launched
/// with; the kernels return whole workgroups past the live count.
fn slot_groups(pool_tiles: u32) -> u32 {
    partial_slots(pool_tiles) as u32
}

impl BodyPasses {
    fn pipelines(device: &GpuDevice) -> Pipelines {
        let source = shader();
        let pipe = |entry: &str, label: &str| device.create_compute_pipeline(&source, entry, label);
        Pipelines {
            partial: pipe("impulse_partial", "gpu_flip.bodies.partial"),
            finalize: pipe("impulse_finalize", "gpu_flip.bodies.finalize"),
            product: pipe("body_product", "gpu_flip.bodies.product"),
            velocity: pipe("velocity_change", "gpu_flip.bodies.velocity_change"),
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            poison: pipe("poison_partials", "gpu_flip.bodies.poison"),
        }
    }

    manifold_core::testkit_visible! {
        /// Build the passes' pipelines; the owning node calls this at install.
        pub(crate) fn prepare_pipelines(&mut self, device: &GpuDevice) {
            if self.pipelines.is_none() {
                self.pipelines = Some(Self::pipelines(device));
            }
            if self.clock_plan.is_none() {
                let plan = device.create_buffer(48, BufferUse::Upload);
                plan.zero_fill();
                self.clock_plan = Some(plan);
            }
        }
    }

    manifold_core::testkit_visible! {
        pub(crate) fn set_clock_plan(&mut self, plan: &GpuBuffer) {
            self.clock_plan = Some(plan.clone());
        }
    }

    fn clock_binding(&self) -> GpuBinding<'_> {
        buffer(
            15,
            self.clock_plan.as_ref().expect("body clock plan prepared"),
        )
    }

    manifold_core::testkit_visible! {
        /// Allocate the sums once and the partials for `count` bodies on a
        /// pool of `pool_tiles`, growing them when a larger pool or more
        /// bodies arrive. The pipelines come from `prepare_pipelines` at
        /// install.
        pub(crate) fn prepare(&mut self, device: &GpuDevice, pool_tiles: u32, count: u32) -> Result<(), String> {
            assert!(self.pipelines.is_some(), "body pipelines built by prepare_pipelines at install");
            let need = partial_bytes(pool_tiles, count);
            if self.partials.as_ref().is_none_or(|p| p.size < need) {
                self.partials = Some(device.try_create_buffer(need, BufferUse::Gpu)?);
            }
            if self.sums.is_none() {
                // Read back so the value proofs can read the last impulse's sums.
                self.sums = Some(device.try_create_buffer(SUM_BYTES, BufferUse::Readback)?);
            }
            Ok(())
        }
    }

    /// The last impulse's sums record per body, for the value proofs.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn sums(&self) -> Option<&GpuBuffer> {
        self.sums.as_ref()
    }

    /// Test-only: NaN into every partial slot of `bodies` after a prepare,
    /// so a finalize that reads a slot the partial pass did not write shows
    /// in the sums.
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    pub fn poison(&self, enc: &mut GpuEncoder, bodies: &Bodies<'_>) {
        let (pipes, partials, _) = self.parts().expect("the body passes were prepared");
        let params = Self::params(bodies, false);
        enc.dispatch_compute(
            &pipes.poison,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                buffer(7, partials),
            ],
            groups(
                u64::from(bodies.count) * partial_slots(bodies.tiles.pool_tiles()) * PARTIAL_FLOATS,
            ),
            "gpu_flip.bodies.poison",
        );
    }

    fn params(bodies: &Bodies<'_>, accumulate: bool) -> Params {
        Params {
            origin: lattice_origin(bodies.cell_size),
            cell_size: bodies.cell_size,
            density: bodies.density,
            tick_seconds: bodies.tick_seconds,
            slots: partial_slots(bodies.tiles.pool_tiles()) as u32,
            first: bodies.first,
            body_count: bodies.count,
            accumulate: u32::from(accumulate),
            pool_tiles: bodies.tiles.pool_tiles(),
            _pad_h: 0,
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
    /// `reaction`, also added into it. With `gate`, on the solve's gate.
    fn impulse<S: Sink>(
        &self,
        enc: &mut S,
        bodies: &Bodies<'_>,
        tiles: Tiles<'_>,
        pressure: &GpuBuffer,
        reaction: Option<&GpuBuffer>,
        gate: Option<&BodyGate<'_>>,
    ) -> Result<(), String> {
        let (pipes, partials, sums) = self.parts()?;
        let pool_tiles = bodies.tiles.pool_tiles();
        if partials.size < partial_bytes(pool_tiles, bodies.count) {
            return Err(format!(
                "the body partials were prepared for fewer than {} bodies on a pool of {pool_tiles}",
                bodies.count
            ));
        }
        let params = Params {
            live: gate.map_or(0, |g| g.live),
            ..Self::params(bodies, reaction.is_some())
        };
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
            buffer(17, bodies.tiles.table()),
        ];
        let groups = [slot_groups(pool_tiles), bodies.count.max(1), 1];
        match gate {
            Some(gate) => enc.gated(
                &pipes.partial,
                &partial,
                groups,
                gate.buffer,
                gate.partial,
                "gpu_flip.bodies.partial",
            ),
            None => enc.plain(&pipes.partial, &partial, groups, "gpu_flip.bodies.partial"),
        }
        // A fixed array, not a Vec: this runs every step. Without a reaction
        // binding 9 holds the sums, which mode 0 never writes through it: a
        // template command inherits no binding, so every one the entry point
        // references is bound.
        let finalize = [
            GpuBinding::Bytes { binding: 0, data },
            buffer(4, bodies.bodies),
            buffer(7, partials),
            buffer(8, sums),
            buffer(13, tiles[1]),
            buffer(12, tiles[0]),
            self.clock_binding(),
            buffer(9, reaction.unwrap_or(sums)),
            buffer(16, bodies.mobility),
        ];
        let groups = [bodies.count.max(1), 1, 1];
        match gate {
            Some(gate) => enc.gated(
                &pipes.finalize,
                &finalize,
                groups,
                gate.buffer,
                gate.finalize,
                "gpu_flip.bodies.finalize",
            ),
            None => enc.plain(
                &pipes.finalize,
                &finalize,
                groups,
                "gpu_flip.bodies.finalize",
            ),
        }
        Ok(())
    }

    /// Inside a conjugate gradient iteration: the bodies' share of the
    /// operator on the search direction `direction`, added to `s`, over the
    /// solver's fine active `tiles`, ungated (the proofs run it outside a solve).
    #[cfg(any(test, feature = "testkit"))]
    pub fn apply<S: Sink>(
        &self,
        enc: &mut S,
        bodies: &Bodies<'_>,
        tiles: Tiles<'_>,
        direction: &GpuBuffer,
        s: &GpuBuffer,
    ) -> Result<(), String> {
        self.apply_on(enc, bodies, tiles, direction, s, None)
    }

    /// [`Self::apply`] on the solve's gate: a round the stop switched off,
    /// or an inactive clock slot, runs none of its three passes.
    pub(crate) fn apply_gated<S: Sink>(
        &self,
        enc: &mut S,
        bodies: &Bodies<'_>,
        tiles: Tiles<'_>,
        direction: &GpuBuffer,
        s: &GpuBuffer,
        gate: &BodyGate<'_>,
    ) -> Result<(), String> {
        self.apply_on(enc, bodies, tiles, direction, s, Some(gate))
    }

    fn apply_on<S: Sink>(
        &self,
        enc: &mut S,
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
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&params),
            },
            buffer(1, bodies.water),
            buffer(2, bodies.open),
            buffer(3, bodies.solid),
            buffer(4, bodies.bodies),
            buffer(8, sums),
            buffer(10, s),
            buffer(12, tiles[0]),
            buffer(14, tiles[2]),
            self.clock_binding(),
            buffer(17, bodies.tiles.table()),
        ];
        let groups = [slot_groups(bodies.tiles.pool_tiles()), 1, 1];
        match gate {
            Some(gate) => enc.gated(
                &pipes.product,
                &product,
                groups,
                gate.buffer,
                gate.product,
                "gpu_flip.bodies.product",
            ),
            None => enc.plain(&pipes.product, &product, groups, "gpu_flip.bodies.product"),
        }
        Ok(())
    }

    manifold_core::testkit_visible! {
        /// After the projection, as the engine finishes its pressure stage: the
        /// pressure's impulse into `reaction` and its velocity change into the
        /// solid velocity (`solid_rw`, the same buffer as `bodies.solid`) over
        /// every held slot.
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
            enc.dispatch_compute_indirect(
                &pipes.velocity,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                    buffer(4, bodies.bodies),
                    buffer(8, sums),
                    buffer(11, bodies.solid),
                    self.clock_binding(),
                    buffer(17, bodies.tiles.table()),
                    buffer(18, &bodies.tiles.lists().by_rank),
                ],
                bodies.tiles.args(),
                allocated_args_offset(bodies.tiles.reach().ring_max),
                "gpu_flip.bodies.velocity_change",
            );
            Ok(())
        }
    }
}

/// Why a step's dynamic bodies are refused, or None: every body's owner code
/// must fit a byte, and the reaction must hold every body.
pub(crate) fn body_refusal(count: u32, reaction: Option<&GpuBuffer>) -> Option<String> {
    if count as usize > MAX_LIQUID_BODIES {
        return Some(format!(
            "{count} bodies exceed the {MAX_LIQUID_BODIES} a liquid holds"
        ));
    }
    let need = u64::from(count) * REACTION_FLOATS as u64 * 4;
    match reaction {
        None => Some("dynamic bodies need the domain's reaction wired".into()),
        Some(buffer) if buffer.size < need => Some(format!(
            "the reaction holds {} bytes, under the {need} {count} bodies need",
            buffer.size
        )),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_shader_validates_with_every_entry() {
        let source = shader();
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
        let entries: Vec<&str> = module
            .entry_points
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        for entry in [
            "impulse_partial",
            "impulse_finalize",
            "body_product",
            "velocity_change",
            "poison_partials",
        ] {
            assert!(entries.contains(&entry), "missing entry {entry}");
        }
    }

    #[test]
    fn params_match_the_shader_uniform() {
        assert_eq!(size_of::<Params>(), 64);
    }

    /// Two slots a pool slot, as the solver counts its fine partials.
    #[test]
    fn partial_slots_are_two_a_pool_slot() {
        assert_eq!(partial_slots(1), 2);
        assert_eq!(partial_slots(12), 24);
        assert_eq!(partial_slots(512), 2 * 512);
        assert_eq!(partial_bytes(512, 0), partial_bytes(512, 1));
        assert_eq!(partial_bytes(512, 3), 3 * 1024 * 32);
        assert_eq!(slot_groups(12), 24);
    }
}
