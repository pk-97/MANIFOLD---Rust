//! Ported from FLIP Fluids particlesheeter.cpp (MIT); see THIRD_PARTY_NOTICES.md.
//! GPU FLIP sheet detection, seed selection and births, a stage internal of
//! `node.gpu_flip_step` (DECOMPOSING_GENERATORS.md section 1.2 (Specialised
//! solvers are stage nodes); ADDING_PRIMITIVES.md exclusion 6). The contract is
//! `manifold_fluids::sheeter`, the CPU port proven against the native
//! sheeter; this stage is proven against that port.
//!
//! Output: the births the sheeter would seed before its fill-rate draw, each
//! a lattice-local position and its key (the engine's visiting order, see the
//! shader's `site_key`), and their true count. The grid is the held world
//! tiles of the step's table: a tile that holds no slot is outside it (zero
//! level set, no neighbour). Births are numbered by pool slot, then the cell
//! in the slot, then the offset, so a run is bitwise the same whatever order
//! the GPU's threads arrive in. A list of `capacity` keeps the first
//! `capacity` births.
//!
//! In the step, per substep before the move (fluidsimulation.cpp
//! `_updateSheetSeeding`): selection with the fill-rate draw and the slot
//! scan (`encode_draw`), the caller's identity reservation over
//! [`GpuSheeting::winners`], then `encode_write` puts each birth after the
//! live prefix in the saved velocity at its seed, as many as the reservation
//! granted: the record's `n` less the live end.
//!
//! Parity contract. Exact: cell and half-cell indexing (the f64 floor for
//! results inside +-2^30, see the shader's `exact_floor`; beyond that and for
//! non-finite positions it clamps outside every lattice), candidate centres, the
//! claim resolution by visiting order. Narrowed: interpolation and vector arithmetic run in f32 under
//! Metal fast math where the engine widens weights to f64 and contracts its
//! own way, so a decision within rounding of its threshold (band, depth walk,
//! radius, nearest-three, plane guard, mindot) can differ; and a marker's
//! cell for the density cap and first-four selection is the shared
//! ParticleSorter's f32 assignment, which can differ from the engine's f64
//! floor for a marker within an f32 rounding of a cell face.
//!
//! The fill-rate draw is not native's. Native draws a stateful MT19937
//! stream at 32 bits (fluidsimulation.h `_randomDouble`) in seed order and,
//! within 2h of a solid, multiplies the rate by that solid's sheeting
//! strength (fluidsimulation.cpp 7158-7170). Here the draw is a stateless
//! 24-bit PCG hash of (key, tick, substep) and there is no solid multiplier
//! (bodies carry no sheeting strength yet: BUG-g5nm1 (GPU FLIP sheeting solid
//! strength)). So selection parity gives birth parity only at rate 1 and only
//! while native's effective rate stays 1 (a solid sheeting strength under 1
//! breaks it). At a fractional rate the acceptance is a like Bernoulli draw
//! at the rate, not the same particles, and not the same distribution near a
//! solid with strength other than 1 or below the 24-bit draw's resolution.

use manifold_gpu::BufferUse;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};
use manifold_node_engine::scene::world_tiles::{READ_SIDE, lattice_origin, pool_len};
use manifold_water_liquid::world_tiles::{WorldTiles, allocated_args_offset};

/// The stage's gate (`GpuSheeting::encode_gate`): the step's clock plan,
/// made inactive (a live zero step, which every pass of the stage, the
/// sheet distance and the identity reservation skip) unless the particle
/// slots past the sort's live prefix cover the last active substep's
/// request (`stats` word 0, 0 before the first). The reservation grants a
/// request whole or not at all, so a closed gate refuses nothing it would
/// have granted. A gate an active substep closes refuses that request, so
/// it adds it to the tick's refused births (`SHEET_REFUSED_TAIL`), and a
/// full pool keeps reporting the births it turns away.
pub(super) const GATE_SHADER: &str = "
struct Gate { live_range: u32, slots: u32, pad0: u32, pad1: u32 };
@group(0) @binding(0) var<uniform> g: Gate;
@group(0) @binding(1) var<storage, read> ranges: array<vec2<u32>>;
@group(0) @binding(2) var<storage, read> plan: array<u32>;
@group(0) @binding(3) var<storage, read_write> gated: array<u32>;
@group(0) @binding(4) var<storage, read> stats: array<u32>;
@group(0) @binding(5) var<storage, read_write> capped: array<u32>;

@compute @workgroup_size(1)
fn gate() {
    for (var i = 0u; i < 12u; i = i + 1u) {
        gated[i] = plan[i];
    }
    // The plan's own clock_active: a live plan with no step to take.
    let stepping = plan[11] == 0u || bitcast<f32>(plan[0]) > 0.0;
    let last = ranges[g.live_range];
    let live = last.x + last.y;
    if stepping && (live >= g.slots || g.slots - live < stats[0]) {
        gated[0] = 0u;
        gated[11] = 1u;
        capped[SHEET_REFUSED_TAIL] += stats[0];
    }
}
";

/// The gate with the stats layout constants (`SHEET_REFUSED_TAIL`) ahead of it.
pub(super) fn gate_shader() -> String {
    manifold_water_liquid::primitives::liquid_stats::with_stats_layout(GATE_SHADER)
}

/// `GATE_SHADER`'s uniform.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Gate {
    /// The canonical element's range, whose end is the live prefix's.
    live_range: u32,
    slots: u32,
    _pad: [u32; 2],
}

/// The kernel with the world tile read side it indexes the lattice through.
pub(super) fn source() -> String {
    format!(
        "{READ_SIDE}\n{}",
        include_str!("shaders/gpu_flip_sheeting.wgsl")
    )
}

/// The kernel with the stats layout constants (`SHEET_REFUSED_TAIL`) ahead of it.
fn shader() -> String {
    manifold_water_liquid::primitives::liquid_stats::with_stats_layout(&source())
}

/// The kernels, in the order [`CLEAR_TOTALS`] and the rest name them.
const ENTRIES: [&str; 12] = [
    "clear_totals",
    "clear",
    "detect",
    "feather",
    "feather_again",
    "select_markers",
    "build_buckets",
    "candidates",
    "resolve",
    "place",
    "write",
    "tally",
];
const CLEAR_TOTALS: usize = 0;
const CLEAR: usize = 1;
const DETECT: usize = 2;
const FEATHER: usize = 3;
const FEATHER_AGAIN: usize = 4;
const SELECT: usize = 5;
const BUCKETS: usize = 6;
const CANDIDATES: usize = 7;
const RESOLVE: usize = 8;
#[cfg(all(test, feature = "gpu-proofs"))]
const PLACE: usize = 9;
const WRITE: usize = 10;
const TALLY: usize = 11;
const LABELS: [&str; 12] = [
    "gpu_flip.sheeting.clear_totals",
    "gpu_flip.sheeting.clear",
    "gpu_flip.sheeting.detect",
    "gpu_flip.sheeting.feather",
    "gpu_flip.sheeting.feather_again",
    "gpu_flip.sheeting.select",
    "gpu_flip.sheeting.buckets",
    "gpu_flip.sheeting.candidates",
    "gpu_flip.sheeting.resolve",
    "gpu_flip.sheeting.place",
    "gpu_flip.sheeting.write",
    "gpu_flip.sheeting.tally",
];

/// The passes before the birth numbering: totals and cells cleared, detection
/// through the draw. The scan over the slots follows [`RESOLVE`].
const DRAW: [usize; 9] = [
    CLEAR_TOTALS,
    CLEAR,
    DETECT,
    FEATHER,
    FEATHER_AGAIN,
    SELECT,
    BUCKETS,
    CANDIDATES,
    RESOLVE,
];
/// The numbered births into the birth list, then the true total.
#[cfg(all(test, feature = "gpu-proofs"))]
const NUMBER: [usize; 2] = [PLACE, TALLY];
/// The births into the particles after the reservation, then the bookkeeping.
const WRITE_BIRTHS: [usize; 2] = [WRITE, TALLY];
/// gpu_flip_step/common.wgsl `ClockPlan`.
const PLAN_BYTES: u64 = 48;
/// The stats words (`stats`).
const STATS_BYTES: u64 = 32;
/// A particle is a sheet candidate only within this many cells of the
/// surface (particlesheeter.h:131-133 `_maxDepth`).
const MAX_SHEET_DEPTH_CELLS: f64 = 2.0;
/// How far inward the depth test walks from a candidate, in cells
/// (particlesheeter.h:131-133 `_testDistance`).
const DEPTH_TEST_CELLS: f64 = 3.0;
/// The walk's step, in cells (particlesheeter.h:131-133 `_stepDistance`).
const DEPTH_TEST_STEP_CELLS: f64 = 0.5;
/// How deep the sheeter's distance must be true, in cells: a particle the
/// test starts from is at most MAX_SHEET_DEPTH_CELLS deep, walks
/// DEPTH_TEST_CELLS further, and the trilinear sample at the end of the walk
/// reads the cells one beyond.
pub(crate) const SHEET_DISTANCE_CELLS: f64 = MAX_SHEET_DEPTH_CELLS + DEPTH_TEST_CELLS + 1.0;
/// The engine's `sheetFillThreshold` default; no user control yet.
pub(crate) const FILL_THRESHOLD: f32 = -0.95;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    /// The tile pool's capacity P and the table's entries H
    /// (gpu_flip_sheeting.wgsl ts_pool_tiles, ts_table_entries).
    pool_tiles: u32,
    entries: u32,
    capacity: u32,
    steps: u32,
    ox: f32,
    oy: f32,
    oz: f32,
    h: f32,
    inv_h: f32,
    inv_sub: f32,
    half_h: f32,
    sub_dx: f32,
    max_depth: f32,
    step_distance: f32,
    max_seed_depth: f32,
    max_radius: f32,
    threshold: f32,
    /// The f64 reciprocals' bits, low word first, for the exact indexing.
    inv_h_bits_lo: u32,
    inv_h_bits_hi: u32,
    inv_sub_bits_lo: u32,
    inv_sub_bits_hi: u32,
    rate: f32,
    tick: u32,
    substep: u32,
    /// 1 inside the step: `tally` also does the tick's bookkeeping.
    in_step: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

/// Bytes per cell of the per-cell arrays: two sheet flags (the second becomes
/// row lengths), mask, cell count, flags and eight claims.
const CELL_BYTES: u64 = 5 * 4 + 8 * 4;

/// Bytes per cell of the bucket rows: 32 markers of 16 bytes for every eight
/// cells of a bucket.
const BUCKET_BYTES_PER_CELL: u64 = 64;

/// Bytes per pooled cell the stage holds.
pub(crate) const POOLED_BYTES_PER_CELL: u64 = CELL_BYTES + BUCKET_BYTES_PER_CELL;

/// Bytes of bucket rows a pool of `pool_tiles` holds: 64 buckets a slot, 32
/// vec4 markers a bucket.
fn bucket_bytes(pool_tiles: u32) -> u64 {
    BUCKET_BYTES_PER_CELL * 512 * u64::from(pool_tiles)
}

/// Everything the stage holds for a pool of `pool_tiles` and a birth list of
/// `capacity`, the slot scan included; the per-cell arrays and bucket rows
/// are pooled fields.
pub(crate) fn scratch_bytes(capacity: u32, pool_tiles: u32) -> u64 {
    let scan =
        4 * manifold_water_liquid::primitives::prefix_scan::storage_words(pool_tiles as usize)
            as u64;
    CELL_BYTES * pool_len(pool_tiles)
        + bucket_bytes(pool_tiles)
        + 16 * u64::from(capacity.max(1))
        + 16
        + PLAN_BYTES
        + scan
        + STATS_BYTES
}

fn words(x: f64) -> [u32; 2] {
    let bits = x.to_bits();
    [bits as u32, (bits >> 32) as u32]
}

/// What one sheeting pass reads.
pub(crate) struct SheetInputs<'a> {
    /// Liquid particles sorted by cell, stable within a cell. In the step the
    /// births are written after its live prefix.
    pub particles: &'a GpuBuffer,
    /// Per sorted slot, the particle's input index (the engine's marker order).
    pub order: &'a GpuBuffer,
    /// One range per cell of the sort, its bins being the pool indices of the
    /// cells.
    pub ranges: &'a GpuBuffer,
    /// The slots in use, on the GPU: a record `[groups, 1, 1, n]` at a byte
    /// offset of a buffer (a particle identity's at `COUNT_OFFSET`, or a
    /// step's own), `groups` at 256 threads. The marker pass dispatches from
    /// it and visits the first `n` slots, skipping one at radius 0; no pass
    /// reads a slot at or past `n`. In the step the reservation raises `n` by
    /// the births it grants, which `encode_write` reads.
    pub count: (&'a GpuBuffer, u64),
    /// The signed distance to the surface at the cell centres, a pooled cell
    /// field, true to `SHEET_DISTANCE_CELLS` deep (gpu_flip_sheet_distance).
    pub phi: &'a GpuBuffer,
    pub h: f32,
    pub threshold: f32,
}

/// What the in-step births read beyond the selection's inputs.
pub(crate) struct StepBirths<'a> {
    /// The constrained saved faces (the engine's `_savedVelocityField`), a
    /// pooled record field.
    pub old: &'a GpuBuffer,
    /// liquid_state's birth identity, reserved by the caller between the
    /// draw and the write.
    pub identity: &'a GpuBuffer,
    /// The step's `capped` buffer and the byte offset of its solver words:
    /// the stage adds the births the pool refused to the tick's tally there.
    pub capped: &'a GpuBuffer,
    pub capped_offset: u64,
    /// The fill rate in (0, 1]; the caller encodes no pass at 0.
    pub rate: f32,
    pub tick: u32,
    pub substep: u32,
}

#[derive(Default)]
pub(crate) struct GpuSheeting {
    pipelines: Vec<GpuComputePipeline>,
    /// sheet a, sheet b / row lengths, mask, cell counts, bucket rows, claims, candidate
    /// flags, births, birth count, zero plan, birth stats.
    buffers: Option<[GpuBuffer; 11]>,
    /// Per pool slot: the births its cells keep, scanned in place into the
    /// births before each slot's end.
    scan: manifold_water_liquid::primitives::prefix_scan::PrefixScan,
    gate: Option<GpuComputePipeline>,
    /// The plan the stage last ran on ([`Self::encode_gate`]).
    gated_plan: Option<GpuBuffer>,
    capacity: u32,
    pool_tiles: u32,
}

impl GpuSheeting {
    pub(crate) fn prepare(&mut self, device: &GpuDevice) {
        if self.pipelines.is_empty() {
            let shader = shader();
            for entry in ENTRIES {
                self.pipelines.push(device.create_compute_pipeline(
                    &shader,
                    entry,
                    "gpu_flip.sheeting",
                ));
            }
        }
        if self.gate.is_none() {
            self.gate = Some(device.create_compute_pipeline(
                &gate_shader(),
                "gate",
                "gpu_flip.sheeting.gate",
            ));
        }
        self.scan.prepare(device);
    }

    /// Frozen pre-row GPU evaluation, compiled only by the parity proofs.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn prepare_reference(&mut self, device: &GpuDevice) {
        assert!(self.pipelines.is_empty());
        let shader = super::gpu_flip_sheeting_cpu_tests::reference_shader();
        for entry in ENTRIES {
            self.pipelines.push(device.create_compute_pipeline(
                &shader,
                entry,
                "gpu_flip.sheeting.reference",
            ));
        }
        self.scan.prepare(device);
    }

    /// Synthetic cell segments, without detection/feathering.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn encode_buckets(
        &self,
        enc: &mut GpuEncoder,
        inputs: &SheetInputs<'_>,
        tiles: &WorldTiles,
    ) {
        self.dispatch_with(
            enc,
            inputs,
            &self.buffers.as_ref().expect("sheeting reserved")[9],
            None,
            None,
            &[BUCKETS],
            tiles,
        );
    }

    pub(crate) fn reserve(
        &mut self,
        device: &GpuDevice,
        capacity: u32,
        pool_tiles: u32,
    ) -> Result<(), String> {
        if self.buffers.is_some() && self.capacity == capacity && self.pool_tiles == pool_tiles {
            return Ok(());
        }
        manifold_node_engine::load::expand::admit_world_bytes(
            device.modifier_memory_snapshot(),
            scratch_bytes(capacity, pool_tiles),
        )
        .map_err(|e| e.to_string())?;
        let n = pool_len(pool_tiles);
        let plan = device.try_create_buffer(PLAN_BYTES, BufferUse::Upload)?;
        plan.zero_fill();
        // The stats carry over a resize: the water still owes its births.
        let stats = match self.buffers.take() {
            Some([.., stats]) => stats,
            None => {
                let stats = device.try_create_buffer(STATS_BYTES, BufferUse::Upload)?;
                stats.zero_fill();
                stats
            }
        };
        self.buffers = Some([
            device.try_create_buffer(4 * n, BufferUse::Gpu)?,
            device.try_create_buffer(4 * n, BufferUse::Gpu)?,
            device.try_create_buffer(4 * n, BufferUse::Gpu)?,
            device.try_create_buffer(4 * n, BufferUse::Gpu)?,
            device.try_create_buffer(bucket_bytes(pool_tiles), BufferUse::Gpu)?,
            device.try_create_buffer(8 * 4 * n, BufferUse::Gpu)?,
            device.try_create_buffer(4 * n, BufferUse::Gpu)?,
            device.try_create_buffer(16 * u64::from(capacity.max(1)), BufferUse::Gpu)?,
            device.try_create_buffer(16, BufferUse::Gpu)?,
            plan,
            stats,
        ]);
        self.scan.buffer(device, pool_tiles as usize)?;
        if self.gated_plan.is_none() {
            self.gated_plan = Some(device.try_create_buffer(PLAN_BYTES, BufferUse::Gpu)?);
        }
        self.capacity = capacity;
        self.pool_tiles = pool_tiles;
        Ok(())
    }

    /// In the step, first: the plan the stage runs on, `plan` while the
    /// particle slots of `slots` past the sort's live prefix in `ranges`
    /// cover the last request, else inactive (`GATE_SHADER`), its request
    /// added to the refused births in `capped` from `capped_offset`. Water
    /// that fills its pool spends nothing on sheeting.
    pub(crate) fn encode_gate(
        &self,
        enc: &mut GpuEncoder,
        plan: &GpuBuffer,
        ranges: &GpuBuffer,
        slots: u32,
        (capped, capped_offset): (&GpuBuffer, u64),
    ) -> &GpuBuffer {
        let gated = self.gated_plan.as_ref().expect("sheeting reserved");
        let stats = &self.buffers.as_ref().expect("sheeting reserved")[10];
        let gate = Gate {
            live_range: (pool_len(self.pool_tiles) - 1) as u32,
            slots,
            _pad: [0; 2],
        };
        let buffer = |binding, buffer| GpuBinding::Buffer {
            binding,
            buffer,
            offset: 0,
        };
        enc.dispatch_compute(
            self.gate.as_ref().expect("sheeting prepared"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&gate),
                },
                buffer(1, ranges),
                buffer(2, plan),
                buffer(3, gated),
                buffer(4, stats),
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: capped,
                    offset: capped_offset,
                },
            ],
            [1, 1, 1],
            "gpu_flip.sheeting.gate",
        );
        gated
    }

    /// The birth list in birth order and its count buffer: word 0 the true
    /// count, word 1 claimants whose recomputed evaluation disagreed (always 0).
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn births(&self) -> (&GpuBuffer, &GpuBuffer) {
        let b = self.buffers.as_ref().expect("sheeting reserved");
        (&b[7], &b[8])
    }

    /// The draw's births per pool slot, scanned: what the identity reservation
    /// reads (its last word over [`Self::ranks`] is the total).
    pub(crate) fn winners(&self) -> &GpuBuffer {
        self.scan.buffer_ref()
    }

    /// The scan's length: one per pool slot.
    pub(crate) fn ranks(&self) -> u32 {
        self.pool_tiles
    }

    /// Eight words: the last active substep's requested and written births,
    /// then both summed over every substep since the stage was first
    /// reserved, then
    /// the water budget the step's `remove_owed_markers` keeps (the births
    /// owed, the cells owing this substep and the particles given back).
    pub(crate) fn stats(&self) -> &GpuBuffer {
        &self.buffers.as_ref().expect("sheeting reserved")[10]
    }

    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn reserved(&self) -> bool {
        self.buffers.is_some()
    }

    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn scratch(&self) -> &[GpuBuffer; 11] {
        self.buffers.as_ref().expect("sheeting reserved")
    }

    /// Always active: the zero clock plan.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn encode(
        &self,
        enc: &mut GpuEncoder,
        inputs: &SheetInputs<'_>,
        tiles: &WorldTiles,
    ) {
        let zeros = &self.buffers.as_ref().expect("sheeting reserved")[9];
        self.encode_gated(enc, inputs, zeros, tiles);
    }

    /// Proof only: each particle's cell and half-cell into the birth list,
    /// two records a particle (`capacity` must cover `2 * count`), the
    /// position taken as lattice-local already.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn encode_index_probe(
        &self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        inputs: &SheetInputs<'_>,
        tiles: &WorldTiles,
    ) {
        let probe =
            device.create_compute_pipeline(&shader(), "index_probe", "gpu_flip.sheeting.probe");
        self.dispatch_with(
            enc,
            inputs,
            &self.buffers.as_ref().expect("sheeting reserved")[9],
            None,
            Some(&probe),
            &[],
            tiles,
        );
    }

    /// Proof only: clear and detect, the sheet flags before feathering.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn encode_detect(
        &self,
        enc: &mut GpuEncoder,
        inputs: &SheetInputs<'_>,
        tiles: &WorldTiles,
    ) {
        self.dispatch_with(
            enc,
            inputs,
            &self.buffers.as_ref().expect("sheeting reserved")[9],
            None,
            None,
            &[CLEAR, DETECT],
            tiles,
        );
    }

    /// Gated by the FLIP clock plan: in an inactive slot every pass returns
    /// before writing.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn encode_gated(
        &self,
        enc: &mut GpuEncoder,
        inputs: &SheetInputs<'_>,
        plan: &GpuBuffer,
        tiles: &WorldTiles,
    ) {
        self.dispatch_with(enc, inputs, plan, None, None, &DRAW, tiles);
        self.dispatch_with(enc, inputs, plan, None, None, &NUMBER, tiles);
    }

    /// In the step: selection, the fill-rate draw and the scan over the pool's
    /// slots. The caller reserves identities from [`Self::winners`] over
    /// [`Self::ranks`], then calls [`Self::encode_write`].
    pub(crate) fn encode_draw(
        &self,
        enc: &mut GpuEncoder,
        inputs: &SheetInputs<'_>,
        plan: &GpuBuffer,
        births: &StepBirths<'_>,
        tiles: &WorldTiles,
    ) {
        self.dispatch_with(enc, inputs, plan, Some(births), None, &DRAW, tiles);
    }

    /// In the step, after the reservation: the births after the live prefix.
    pub(crate) fn encode_write(
        &self,
        enc: &mut GpuEncoder,
        inputs: &SheetInputs<'_>,
        plan: &GpuBuffer,
        births: &StepBirths<'_>,
        tiles: &WorldTiles,
    ) {
        self.dispatch_with(enc, inputs, plan, Some(births), None, &WRITE_BIRTHS, tiles);
    }

    fn dispatch_with(
        &self,
        enc: &mut GpuEncoder,
        inputs: &SheetInputs<'_>,
        plan: &GpuBuffer,
        step: Option<&StepBirths<'_>>,
        probe: Option<&GpuComputePipeline>,
        entries: &[usize],
        tiles: &WorldTiles,
    ) {
        assert_eq!(
            tiles.pool_tiles(),
            self.pool_tiles,
            "sheeting was reserved for another pool"
        );
        let [
            sheet_a,
            sheet_b,
            mask,
            cell_counts,
            selected,
            claims,
            flags,
            births,
            birth_count,
            zeros,
            stats,
        ] = self.buffers.as_ref().expect("sheeting reserved");
        let h = inputs.h;
        let dx = f64::from(h);
        let origin = lattice_origin(h);
        // The sheeter's constants, rounded as manifold_fluids::sheeter rounds them.
        let test_distance = (DEPTH_TEST_CELLS * dx) as f32;
        let step_distance = (DEPTH_TEST_STEP_CELLS * dx) as f32;
        let params = Params {
            pool_tiles: tiles.pool_tiles(),
            entries: tiles.table_entries(),
            capacity: self.capacity,
            steps: (test_distance / step_distance).ceil() as u32,
            ox: origin[0],
            oy: origin[1],
            oz: origin[2],
            h,
            inv_h: (1.0 / dx) as f32,
            inv_sub: (1.0 / (0.5 * dx)) as f32,
            half_h: (0.5 * dx) as f32,
            sub_dx: (0.5 * dx) as f32,
            max_depth: (MAX_SHEET_DEPTH_CELLS * dx) as f32,
            step_distance,
            max_seed_depth: dx as f32,
            max_radius: (2.0 * dx) as f32,
            threshold: inputs.threshold,
            inv_h_bits_lo: words(1.0 / dx)[0],
            inv_h_bits_hi: words(1.0 / dx)[1],
            inv_sub_bits_lo: words(1.0 / (0.5 * dx))[0],
            inv_sub_bits_hi: words(1.0 / (0.5 * dx))[1],
            // Standalone keeps every winner: the draw is under 1.
            rate: step.map_or(1.0, |s| s.rate),
            tick: step.map_or(0, |s| s.tick),
            substep: step.map_or(0, |s| s.substep),
            in_step: u32::from(step.is_some()),
            pad0: 0,
            pad1: 0,
            pad2: 0,
        };
        fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
            GpuBinding::Buffer {
                binding,
                buffer,
                offset: 0,
            }
        }
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&params),
            },
            buffer(1, inputs.particles),
            buffer(2, inputs.ranges),
            buffer(3, inputs.order),
            buffer(4, inputs.phi),
            buffer(5, sheet_a),
            buffer(6, sheet_b),
            buffer(7, mask),
            buffer(8, selected),
            buffer(9, cell_counts),
            buffer(10, claims),
            buffer(11, flags),
            buffer(12, births),
            buffer(13, birth_count),
            buffer(14, plan),
            buffer(15, self.scan.buffer_ref()),
            buffer(16, step.map_or(zeros, |s| s.old)),
            buffer(17, step.map_or(zeros, |s| s.identity)),
            buffer(18, stats),
            // Standalone never runs the in-step bookkeeping, the one place
            // that writes this.
            GpuBinding::Buffer {
                binding: 19,
                buffer: step.map_or(stats, |s| s.capped),
                offset: step.map_or(0, |s| s.capped_offset),
            },
            buffer(20, tiles.table()),
            GpuBinding::Buffer {
                binding: 21,
                buffer: inputs.count.0,
                offset: inputs.count.1,
            },
            buffer(22, &tiles.lists().by_rank),
        ];
        let held = allocated_args_offset(tiles.reach().ring_max);
        if let Some(pipeline) = probe {
            enc.dispatch_compute_indirect(
                pipeline,
                &bindings,
                inputs.count.0,
                inputs.count.1,
                "gpu_flip.sheeting.probe",
            );
            return;
        }
        for &i in entries {
            let pipeline = &self.pipelines[i];
            match i {
                // One thread per slot in use: dispatched from the record.
                DETECT => enc.dispatch_compute_indirect(
                    pipeline,
                    &bindings,
                    inputs.count.0,
                    inputs.count.1,
                    LABELS[i],
                ),
                // One thread per pool slot, held or not.
                CLEAR_TOTALS => enc.dispatch_compute(
                    pipeline,
                    &bindings,
                    [self.pool_tiles.div_ceil(256).max(1), 1, 1],
                    LABELS[i],
                ),
                TALLY => enc.dispatch_compute(pipeline, &bindings, [1, 1, 1], LABELS[i]),
                // The rest run over the cells of the held slots, 512 threads
                // a slot (`place` and `write` as two workgroups a slot).
                _ => enc.dispatch_compute_indirect(
                    pipeline,
                    &bindings,
                    tiles.args(),
                    held,
                    LABELS[i],
                ),
            }
            enc.compute_memory_barrier_buffers();
            if i == RESOLVE {
                self.scan.encode_labelled_gated(
                    enc,
                    self.pool_tiles as usize,
                    manifold_water_liquid::primitives::prefix_scan::ScanLabels {
                        blocks: "gpu_flip.sheeting.scan.blocks",
                        add: "gpu_flip.sheeting.scan.add",
                    },
                    plan,
                );
                enc.compute_memory_barrier_buffers();
            }
        }
    }
}
