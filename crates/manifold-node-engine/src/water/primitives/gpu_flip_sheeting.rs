//! Ported from FLIP Fluids particlesheeter.cpp (MIT); see THIRD_PARTY_NOTICES.md.
//! GPU FLIP sheet detection, seed selection and births, a stage internal of
//! `node.gpu_flip_step` (DECOMPOSING_GENERATORS.md section 1.2 (Specialised
//! solvers are stage nodes); ADDING_PRIMITIVES.md exclusion 6). The contract is
//! `manifold_fluids::sheeter`, the CPU port proven against the native
//! sheeter; this stage is proven against that port.
//!
//! Output: the births the sheeter would seed before its fill-rate draw, in
//! the engine's order (ascending candidate rank), each a grid-local position
//! and its rank, and their true count. A list of `capacity` keeps the engine's
//! first `capacity` births.
//!
//! In the step, per substep before the move (fluidsimulation.cpp
//! `_updateSheetSeeding`): selection with the fill-rate draw and the rank
//! scan (`encode_draw`), the caller's identity reservation over
//! [`GpuSheeting::winners`], then `encode_write` puts each birth after the
//! live prefix in the saved velocity at its seed.
//!
//! Parity contract. Exact: cell and half-cell indexing (the f64 floor for
//! results inside +-2^30, see the shader's `exact_floor`; beyond that and for
//! non-finite positions it clamps outside every lattice), candidate centres, visiting order, the claim
//! resolution. Narrowed: interpolation and vector arithmetic run in f32 under
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
//! 24-bit PCG hash of (rank, tick, substep) and there is no solid multiplier
//! (bodies carry no sheeting strength yet: BUG-g5nm1 (GPU FLIP sheeting solid
//! strength)). So selection parity gives birth parity only at rate 1 and only
//! while native's effective rate stays 1 (a solid sheeting strength under 1
//! breaks it). At a fractional rate the acceptance is a like Bernoulli draw
//! at the rate, not the same particles, and not the same distribution near a
//! solid with strength other than 1 or below the 24-bit draw's resolution.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

const SHADER: &str = include_str!("shaders/gpu_flip_sheeting.wgsl");
const ENTRIES: [&str; 10] = [
    "clear", "detect", "feather", "feather_border", "select_markers", "build_buckets", "candidates", "resolve", "place", "write",
];
/// gpu_flip_step.wgsl `ClockPlan`.
const PLAN_BYTES: u64 = 48;
/// The engine's `sheetFillThreshold` default; no user control yet.
pub(crate) const FILL_THRESHOLD: f32 = -0.95;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    nx: u32,
    ny: u32,
    nz: u32,
    count: u32,
    bx: u32,
    by: u32,
    bz: u32,
    capacity: u32,
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
    steps: u32,
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
    slots: u32,
    pad0: u32,
    pad1: u32,
}

/// Bytes per cell excluding padded bucket rows: two sheet flags (the second
/// becomes row lengths), mask, cell count, flags and eight claims.
const BYTES_PER_CELL: u64 = 5 * 4 + 8 * 4;

fn bucket_bytes(cells: [u32; 3]) -> u64 {
    // 32 vec4 markers per bucket, including padded cells on odd sides.
    8 * ranks(cells)
}

/// Everything the stage holds for `cells` and a birth list of `capacity`,
/// the rank scan included.
pub(crate) fn scratch_bytes(cells: [u32; 3], capacity: u32) -> u64 {
    let n = cells.into_iter().map(u64::from).product::<u64>();
    let scan = 4 * super::prefix_scan::storage_words(ranks(cells) as usize) as u64;
    BYTES_PER_CELL * n + bucket_bytes(cells) + 16 * u64::from(capacity.max(1)) + 16 + PLAN_BYTES + scan + 16
}

/// Candidate ranks: eight offsets of eight cells of every 2-cell bucket.
fn ranks(cells: [u32; 3]) -> u64 {
    64 * cells.into_iter().map(|c| u64::from(c.div_ceil(2))).product::<u64>()
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
    /// One range per cell of the sort, its bins being the solver cells.
    pub ranges: &'a GpuBuffer,
    /// Slots the marker pass visits; a slot at radius 0 is skipped.
    pub count: u32,
    /// The surface level set at the cell centres, x fastest.
    pub phi: &'a GpuBuffer,
    /// The grid's minimum corner; positions are made grid-local by it.
    pub origin: [f32; 3],
    pub h: f32,
    pub threshold: f32,
}

/// What the in-step births read beyond the selection's inputs.
pub(crate) struct StepBirths<'a> {
    /// The constrained saved faces (the engine's `_savedVelocityField`).
    pub old: &'a GpuBuffer,
    /// liquid_state's birth identity, reserved by the caller between the
    /// draw and the write.
    pub identity: &'a GpuBuffer,
    /// The pool's slots: births past them are dropped and counted.
    pub slots: u32,
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
    /// Per rank: the winners, scanned in place into birth indices.
    scan: super::prefix_scan::PrefixScan,
    cells: [u32; 3],
    capacity: u32,
}

impl GpuSheeting {
    pub(crate) fn prepare(&mut self, device: &GpuDevice) {
        if self.pipelines.is_empty() {
            for entry in ENTRIES {
                self.pipelines.push(device.create_compute_pipeline(SHADER, entry, "gpu_flip.sheeting"));
            }
        }
        self.scan.prepare(device);
    }

    /// Frozen pre-row GPU evaluation, compiled only by the parity proofs.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn prepare_reference(&mut self, device: &GpuDevice) {
        assert!(self.pipelines.is_empty());
        let shader = super::gpu_flip_sheeting_cpu_tests::reference_shader();
        for entry in ENTRIES {
            self.pipelines.push(device.create_compute_pipeline(&shader, entry, "gpu_flip.sheeting.reference"));
        }
        self.scan.prepare(device);
    }

    /// Synthetic cell segments, without detection/feathering.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn encode_buckets(&self, enc: &mut GpuEncoder, inputs: &SheetInputs<'_>) {
        self.dispatch_with(enc, inputs, &self.buffers.as_ref().expect("sheeting reserved")[9], None, None, 5..6);
    }

    pub(crate) fn reserve(&mut self, device: &GpuDevice, cells: [u32; 3], capacity: u32) -> Result<(), String> {
        if self.buffers.is_some() && self.cells == cells && self.capacity == capacity {
            return Ok(());
        }
        crate::load::expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            scratch_bytes(cells, capacity),
        )
        .map_err(|e| e.to_string())?;
        let n = cells.into_iter().map(u64::from).product::<u64>();
        let plan = device.try_create_buffer_shared(PLAN_BYTES)?;
        plan.zero_fill();
        let stats = device.try_create_buffer_shared(16)?;
        stats.zero_fill();
        self.buffers = Some([
            device.try_create_buffer(4 * n)?,
            device.try_create_buffer(4 * n)?,
            device.try_create_buffer(4 * n)?,
            device.try_create_buffer(4 * n)?,
            device.try_create_buffer(bucket_bytes(cells))?,
            device.try_create_buffer(8 * 4 * n)?,
            device.try_create_buffer(4 * n)?,
            device.try_create_buffer(16 * u64::from(capacity.max(1)))?,
            device.try_create_buffer(16)?,
            plan,
            stats,
        ]);
        self.scan.buffer(device, ranks(cells) as usize)?;
        self.cells = cells;
        self.capacity = capacity;
        Ok(())
    }

    /// The birth list in the engine's order and its count buffer: word 0 the
    /// true count, word 1 claimants whose recomputed evaluation disagreed (always 0).
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn births(&self) -> (&GpuBuffer, &GpuBuffer) {
        let b = self.buffers.as_ref().expect("sheeting reserved");
        (&b[7], &b[8])
    }

    /// The draw's winners per rank, scanned: what the identity reservation reads.
    pub(crate) fn winners(&self) -> &GpuBuffer {
        self.scan.buffer_ref()
    }

    /// Candidate ranks, the scan's length.
    pub(crate) fn ranks(&self) -> u32 {
        ranks(self.cells) as u32
    }

    /// Four words: the last active substep's requested and written births,
    /// then both summed over every substep since the stage was reserved.
    #[cfg(all(test, feature = "gpu-proofs"))]
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
    pub(crate) fn encode(&self, enc: &mut GpuEncoder, inputs: &SheetInputs<'_>) {
        let zeros = &self.buffers.as_ref().expect("sheeting reserved")[9];
        self.encode_gated(enc, inputs, zeros);
    }

    /// Proof only: each particle's cell and half-cell into the birth list,
    /// two records a particle (`capacity` must cover `2 * count`).
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn encode_index_probe(&self, device: &GpuDevice, enc: &mut GpuEncoder, inputs: &SheetInputs<'_>) {
        let probe = device.create_compute_pipeline(SHADER, "index_probe", "gpu_flip.sheeting.probe");
        self.dispatch_with(enc, inputs, &self.buffers.as_ref().expect("sheeting reserved")[9], None, Some((&probe, inputs.count)), 0..0);
    }

    /// Proof only: clear and detect, the sheet flags before feathering.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn encode_detect(&self, enc: &mut GpuEncoder, inputs: &SheetInputs<'_>) {
        self.dispatch_with(enc, inputs, &self.buffers.as_ref().expect("sheeting reserved")[9], None, None, 0..2);
    }

    /// Gated by the FLIP clock plan: in an inactive slot every pass returns
    /// before writing.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn encode_gated(&self, enc: &mut GpuEncoder, inputs: &SheetInputs<'_>, plan: &GpuBuffer) {
        self.dispatch_with(enc, inputs, plan, None, None, 0..9);
    }

    /// In the step: selection, the fill-rate draw and the rank scan. The
    /// caller reserves identities from [`Self::winners`] over [`Self::ranks`],
    /// then calls [`Self::encode_write`].
    pub(crate) fn encode_draw(&self, enc: &mut GpuEncoder, inputs: &SheetInputs<'_>, plan: &GpuBuffer, births: &StepBirths<'_>) {
        self.dispatch_with(enc, inputs, plan, Some(births), None, 0..8);
    }

    /// In the step, after the reservation: the births after the live prefix.
    pub(crate) fn encode_write(&self, enc: &mut GpuEncoder, inputs: &SheetInputs<'_>, plan: &GpuBuffer, births: &StepBirths<'_>) {
        self.dispatch_with(enc, inputs, plan, Some(births), None, 9..10);
    }

    fn dispatch_with(
        &self,
        enc: &mut GpuEncoder,
        inputs: &SheetInputs<'_>,
        plan: &GpuBuffer,
        step: Option<&StepBirths<'_>>,
        probe: Option<(&GpuComputePipeline, u32)>,
        entries: std::ops::Range<usize>,
    ) {
        let [sheet_a, sheet_b, mask, cell_counts, selected, claims, flags, births, birth_count, zeros, stats] =
            self.buffers.as_ref().expect("sheeting reserved");
        let n = self.cells;
        let h = inputs.h;
        let dx = f64::from(h);
        // The sheeter's constants, rounded as manifold_fluids::sheeter rounds them.
        let test_distance = (3.0 * dx) as f32;
        let step_distance = (0.5 * dx) as f32;
        let params = Params {
            nx: n[0],
            ny: n[1],
            nz: n[2],
            count: inputs.count,
            bx: n[0].div_ceil(2),
            by: n[1].div_ceil(2),
            bz: n[2].div_ceil(2),
            capacity: self.capacity,
            ox: inputs.origin[0],
            oy: inputs.origin[1],
            oz: inputs.origin[2],
            h,
            inv_h: (1.0 / dx) as f32,
            inv_sub: (1.0 / (0.5 * dx)) as f32,
            half_h: (0.5 * dx) as f32,
            sub_dx: (0.5 * dx) as f32,
            max_depth: (2.0 * dx) as f32,
            step_distance,
            steps: (test_distance / step_distance).ceil() as u32,
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
            slots: step.map_or(0, |s| s.slots),
            pad0: 0,
            pad1: 0,
        };
        fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
            GpuBinding::Buffer { binding, buffer, offset: 0 }
        }
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
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
        ];
        let cells = n[0] * n[1] * n[2];
        let groups = |work: u32| [work.div_ceil(256).max(1), 1, 1];
        if let Some((pipeline, work)) = probe {
            enc.dispatch_compute(pipeline, &bindings, groups(work), "gpu_flip.sheeting.probe");
            return;
        }
        let ranks = ranks(n) as u32;
        let work = [cells, inputs.count, cells, cells, cells, ranks / 64, 8 * cells, ranks, ranks, ranks];
        let labels = [
            "gpu_flip.sheeting.clear",
            "gpu_flip.sheeting.detect",
            "gpu_flip.sheeting.feather",
            "gpu_flip.sheeting.feather_border",
            "gpu_flip.sheeting.select",
            "gpu_flip.sheeting.buckets",
            "gpu_flip.sheeting.candidates",
            "gpu_flip.sheeting.resolve",
            "gpu_flip.sheeting.place",
            "gpu_flip.sheeting.write",
        ];
        for i in entries {
            enc.dispatch_compute(&self.pipelines[i], &bindings, groups(work[i]), labels[i]);
            enc.compute_memory_barrier_buffers();
            if i == 7 {
                self.scan.encode_labelled_gated(
                    enc,
                    ranks as usize,
                    super::prefix_scan::ScanLabels { blocks: "gpu_flip.sheeting.scan.blocks", add: "gpu_flip.sheeting.scan.add" },
                    plan,
                );
                enc.compute_memory_barrier_buffers();
            }
        }
    }
}
