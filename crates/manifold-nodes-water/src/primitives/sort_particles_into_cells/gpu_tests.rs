//! The counting sort against its semantics computed on the CPU, word for
//! word: each live record among the first `count` (never more than every
//! wired output holds) in bin order, each bin in input order, the slots past
//! the live total cleared, nothing past the slots the outputs hold, and
//! nothing at all through a shut clock gate.

use manifold_gpu::BufferUse;
use manifold_gpu::{GpuBuffer, GpuDevice};
use manifold_node_engine::ports::ArrayType;
use manifold_node_engine::scene::world_tiles::{
    lattice_origin, local_index, pool_len, table_words, tile_of_cell,
};
use manifold_water_gpu_mpm::matter::MatterPoint;
use manifold_water_liquid::fluid_particles::{CellRange, bin_total};
use manifold_water_liquid::primitives::particle_identity::COUNT_OFFSET;
use manifold_water_liquid::primitives::prefix_scan::ScanLabels;
use manifold_water_liquid::primitives::sort_particles_into_cells::*;
use manifold_water_liquid::testkit::world_box::table_words_of;

const LABELS: SortLabels = SortLabels {
    clear: "sort proof clear",
    count: "sort proof count",
    scan: ScanLabels {
        blocks: "sort proof scan blocks",
        add: "sort proof scan add",
    },
    ranges: "sort proof ranges",
    tail: "sort proof tail",
    scatter: "sort proof scatter",
    stabilise: "sort proof stabilise",
};

/// Bins filled to exactly these sizes: one, two, the low thirties, a crowded
/// cell before the crowding removal, and past a thousand.
const CROWDED: [usize; 9] = [1, 2, 31, 32, 33, 34, 250, 1000, 4097];

/// xorshift64*, seeded per fixture, so every run sorts the same records.
struct Rng(u64);

impl Rng {
    fn word(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u32
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f32 {
        (self.word() >> 8) as f32 / 16_777_216.0
    }

    fn below(&mut self, n: usize) -> usize {
        ((u64::from(self.word()) * n as u64) >> 32) as usize
    }
}

/// `bins` bins of `cell` metres from the corner `min`.
#[derive(Clone, Copy)]
struct Grid {
    min: [f32; 3],
    cell: f32,
    bins: [u32; 3],
}

/// Records in `read`'s layout, as the raw words the sort reads.
struct Input {
    words: Vec<u32>,
    read: RecordRead,
    grid: Grid,
    /// (bin, size) of each bin the fixture fills to an exact size.
    crowded: Vec<(usize, usize)>,
}

/// Hands out the shuffled record slots and writes each record's position and
/// liveness word; every other word keeps its random fill.
struct Writer<'a> {
    words: &'a mut [u32],
    read: RecordRead,
    slots: std::vec::IntoIter<usize>,
}

impl Writer<'_> {
    fn put(&mut self, rng: &mut Rng, mut p: [f32; 3], live: bool) {
        let i = self.slots.next().expect("a slot for every record");
        let stride = self.read.stride_words as usize;
        let live_word = i * stride + self.read.live_word as usize;
        let non_finite = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
        match self.read.live_rule {
            LIVE_BY_RADIUS => {
                self.words[live_word] = if live {
                    if rng.below(100) == 0 {
                        f32::INFINITY.to_bits()
                    } else {
                        (0.01 + 0.04 * rng.unit()).to_bits()
                    }
                } else {
                    [
                        0.0_f32.to_bits(),
                        (-0.0_f32).to_bits(),
                        (-0.01_f32).to_bits(),
                        f32::NAN.to_bits(),
                        0xffc0_0000,
                        f32::NEG_INFINITY.to_bits(),
                    ][rng.below(6)]
                };
                // A dead record's position is never read.
                if !live && rng.below(4) == 0 {
                    p[rng.below(3)] = non_finite[rng.below(3)];
                }
            }
            // Dead by the word, or by a non-finite axis with a live word.
            LIVE_BY_ID => {
                let live_id = live || rng.below(2) == 0;
                self.words[live_word] = if live_id {
                    1 + rng.below(u32::MAX as usize - 1) as u32
                } else {
                    0
                };
                if !live && live_id {
                    p[rng.below(3)] = non_finite[rng.below(3)];
                }
            }
            _ => {
                let live_kind = live || rng.below(2) == 0;
                self.words[live_word] = if live_kind {
                    [0, 1, 2, 4][rng.below(4)]
                } else {
                    [3, 5, 6, u32::MAX][rng.below(4)]
                };
                if !live && live_kind {
                    p[rng.below(3)] = non_finite[rng.below(3)];
                }
            }
        }
        let position = i * stride + self.read.position_word as usize;
        for (axis, value) in p.into_iter().enumerate() {
            self.words[position + axis] = value.to_bits();
        }
    }
}

impl Input {
    /// `n` records over `grid`, shuffled so each bin's members are spread
    /// over the input. Bin rows from `slab` up hold only the crowded bins,
    /// so those have exactly their sizes and the rest of the slab is empty.
    /// Below it: records on bin faces (exactly on them when the cell and the
    /// corner are binary fractions), records outside the box that clamp into
    /// the border bins, dead records of every kind the layout's rule names,
    /// and the rest uniform.
    fn build(
        read: RecordRead,
        grid: Grid,
        n: usize,
        slab: u32,
        crowded: &[usize],
        seed: u64,
    ) -> Self {
        let Grid { min, cell, bins } = grid;
        let mut rng = Rng(seed);
        let stride = read.stride_words as usize;
        let mut words: Vec<u32> = (0..n * stride).map(|_| rng.word()).collect();
        let mut slots: Vec<usize> = (0..n).collect();
        for k in (1..n).rev() {
            slots.swap(k, rng.below(k + 1));
        }
        let max: [f32; 3] = std::array::from_fn(|a| min[a] + bins[a] as f32 * cell);
        // Half a bin short of the slab, so no rounding lands in it.
        let low = |rng: &mut Rng| -> [f32; 3] {
            [
                min[0] + rng.unit() * bins[0] as f32 * cell,
                min[1] + rng.unit() * (slab as f32 - 0.5) * cell,
                min[2] + rng.unit() * bins[2] as f32 * cell,
            ]
        };
        let mut writer = Writer {
            words: &mut words,
            read,
            slots: slots.into_iter(),
        };
        let mut filled: Vec<(usize, usize)> = Vec::new();
        for &size in crowded {
            let (at, bin) = loop {
                let at = [
                    rng.below(bins[0] as usize),
                    (slab + 1) as usize + rng.below((bins[1] - slab - 1) as usize),
                    rng.below(bins[2] as usize),
                ];
                let bin = at[0] + bins[0] as usize * (at[1] + bins[1] as usize * at[2]);
                if filled.iter().all(|&(other, _)| other != bin) {
                    break (at, bin);
                }
            };
            for _ in 0..size {
                let p = std::array::from_fn(|a| {
                    min[a] + (at[a] as f32 + 0.05 + 0.9 * rng.unit()) * cell
                });
                writer.put(&mut rng, p, true);
            }
            filled.push((bin, size));
        }
        for _ in 0..n / 50 {
            let mut p = low(&mut rng);
            let axes = 1 + rng.below(7);
            for (a, value) in p.iter_mut().enumerate() {
                if axes & (1 << a) != 0 {
                    let last_face = if a == 1 { slab - 1 } else { bins[a] };
                    *value = min[a] + rng.below(last_face as usize + 1) as f32 * cell;
                }
            }
            writer.put(&mut rng, p, true);
        }
        for _ in 0..n / 100 {
            let mut p = low(&mut rng);
            let past = (0.01 + 2.0 * rng.unit()) * cell;
            match rng.below(5) {
                0 => p[0] = min[0] - past,
                1 => p[0] = max[0] + past,
                2 => p[2] = min[2] - past,
                3 => p[2] = max[2] + past,
                _ => p[1] = min[1] - past,
            }
            writer.put(&mut rng, p, true);
        }
        for _ in 0..n / 8 {
            let p = low(&mut rng);
            writer.put(&mut rng, p, false);
        }
        for _ in 0..writer.slots.len() {
            let p = low(&mut rng);
            writer.put(&mut rng, p, true);
        }
        Self {
            words,
            read,
            grid,
            crowded: filled,
        }
    }

    fn records(&self) -> usize {
        self.words.len() / self.read.stride_words as usize
    }

    fn record(&self, i: usize) -> &[u32] {
        let stride = self.read.stride_words as usize;
        &self.words[i * stride..(i + 1) * stride]
    }

    fn position(&self, i: usize) -> [u32; 3] {
        let at = self.read.position_word as usize;
        std::array::from_fn(|a| self.record(i)[at + a])
    }

    /// sort_particles_into_cells.wgsl `is_live`.
    fn live(&self, i: usize) -> bool {
        let word = self.record(i)[self.read.live_word as usize];
        let finite = self
            .position(i)
            .iter()
            .all(|&w| w & 0x7f80_0000 != 0x7f80_0000);
        match self.read.live_rule {
            LIVE_BY_RADIUS => f32::from_bits(word) > 0.0,
            LIVE_BY_KIND => (word < 3 || word == 4) && finite,
            _ => word != 0 && finite,
        }
    }

    /// sort_particles_into_cells.wgsl `bin_of`, in the same f32 operations.
    /// Every live position the fixtures write is within a few bins of the
    /// box, so the conversion to i32 is exact.
    fn bin_of(&self, i: usize) -> usize {
        let Grid { min, cell, bins } = self.grid;
        let inv_cell = 1.0 / cell;
        let p = self.position(i).map(f32::from_bits);
        let b: [u32; 3] = std::array::from_fn(|a| {
            let at = ((p[a] - min[a]) * inv_cell).floor() as i32;
            at.clamp(0, bins[a] as i32 - 1) as u32
        });
        (b[0] + bins[0] * (b[1] + bins[1] * b[2])) as usize
    }

    /// The sort on the CPU: each live record among the first `count`, bins in
    /// index order, each bin in input order. The cell ranges, and each
    /// sorted slot's input index.
    fn cpu_sort(&self, count: usize) -> (Vec<CellRange>, Vec<u32>) {
        let binned: Vec<Option<usize>> = (0..count)
            .map(|i| self.live(i).then(|| self.bin_of(i)))
            .collect();
        let mut ranges = vec![CellRange::default(); bin_total(self.grid.bins) as usize];
        for &bin in binned.iter().flatten() {
            ranges[bin].count += 1;
        }
        let mut start = 0;
        for range in &mut ranges {
            range.start = start;
            start += range.count;
        }
        let mut next: Vec<u32> = ranges.iter().map(|range| range.start).collect();
        let mut members = vec![0; start as usize];
        for (i, bin) in binned.into_iter().enumerate() {
            if let Some(bin) = bin {
                members[next[bin] as usize] = i as u32;
                next[bin] += 1;
            }
        }
        (ranges, members)
    }

    /// The fixture holds what the proofs claim: every crowded bin at exactly
    /// its size, and empty bins.
    fn check_fixture(&self) {
        let (ranges, _) = self.cpu_sort(self.records());
        for &(bin, size) in &self.crowded {
            assert_eq!(ranges[bin].count as usize, size, "crowded bin {bin}");
        }
        assert!(
            ranges.iter().any(|range| range.count == 0),
            "the fixture has no empty bin"
        );
    }

    fn upload(&self, device: &GpuDevice) -> GpuBuffer {
        let buffer = device.create_buffer(self.words.len() as u64 * 4, BufferUse::Upload);
        // SAFETY: a shared buffer of exactly these words; no GPU work in flight.
        unsafe { buffer.write(0, bytemuck::cast_slice(&self.words)) };
        buffer
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Gate {
    Unwired,
    /// A live clock plan with time to step.
    Open,
    /// A live clock plan with no time to step: every pass returns.
    Shut,
}

/// One sort: how many records, the slots each output holds (unwired when
/// `None`), and the gate.
struct Case {
    count: usize,
    sorted: Option<usize>,
    order: Option<usize>,
    gate: Gate,
}

/// What a word the sort must not write holds before and after it.
fn untouched(j: usize) -> u32 {
    0xa5a5_a5a5 ^ (j as u32).wrapping_mul(0x9e37_79b9)
}

fn untouched_buffer(device: &GpuDevice, words: usize) -> GpuBuffer {
    let buffer = device.create_buffer(words as u64 * 4, BufferUse::Upload);
    fill_untouched(&buffer);
    buffer
}

fn fill_untouched(buffer: &GpuBuffer) {
    let words: Vec<u32> = (0..(buffer.size / 4) as usize).map(untouched).collect();
    // SAFETY: a shared buffer of exactly these words; no GPU work in flight.
    unsafe { buffer.write(0, bytemuck::cast_slice(&words)) };
}

fn read_words(buffer: &GpuBuffer) -> Vec<u32> {
    let ptr = buffer.mapped_ptr().expect("a shared buffer");
    // SAFETY: a shared buffer of `size` bytes; the GPU work that wrote it has completed.
    bytemuck::cast_slice(unsafe { std::slice::from_raw_parts(ptr, buffer.size as usize) }).to_vec()
}

/// gpu_flip_clock.wgsl Plan, only the words the gate reads: live, with
/// `step_dt` seconds to step.
fn clock_plan(device: &GpuDevice, step_dt: f32) -> GpuBuffer {
    let mut plan = [0_u32; 12];
    plan[0] = step_dt.to_bits();
    plan[11] = 1;
    let buffer = device.create_buffer(48, BufferUse::Upload);
    // SAFETY: a shared buffer of 12 words; no GPU work in flight.
    unsafe { buffer.write(0, bytemuck::cast_slice(&plan)) };
    buffer
}

fn assert_words(what: &str, got: &[u32], want: &[u32], per_slot: usize) {
    assert_eq!(got.len(), want.len(), "{what}: length");
    if let Some(first) = got.iter().zip(want).position(|(g, w)| g != w) {
        let wrong = got.iter().zip(want).filter(|(g, w)| g != w).count();
        panic!(
            "{what}: {wrong} words differ, the first at word {first} (slot {}): {:#010x}, want {:#010x}",
            first / per_slot,
            got[first],
            want[first]
        );
    }
}

fn sort_and_check(
    device: &GpuDevice,
    sorter: &mut ParticleSorter,
    input: &Input,
    particles: &GpuBuffer,
    case: &Case,
) {
    let what = format!(
        "{} records (rule {}), count {}, sorted {:?}, order {:?}, gate {:?}",
        input.records(),
        input.read.live_rule,
        case.count,
        case.sorted,
        case.order,
        case.gate
    );
    let stride = input.read.stride_words as usize;
    let sorted = case
        .sorted
        .map(|slots| untouched_buffer(device, slots * stride));
    let order = case.order.map(|slots| untouched_buffer(device, slots));
    sorter
        .reserve_ranges(device, input.grid.bins)
        .expect("the cell ranges");
    let ranges = sorter
        .ranges()
        .expect("the cell ranges were reserved")
        .clone();
    fill_untouched(&ranges);
    let gate = match case.gate {
        Gate::Unwired => None,
        Gate::Open => Some(clock_plan(device, 1.0 / 60.0)),
        Gate::Shut => Some(clock_plan(device, 0.0)),
    };
    let mut encoder = device.create_encoder("sort oracle proof");
    sorter
        .encode(
            device,
            &mut encoder,
            &SortJob {
                particles,
                read: input.read,
                capacity: input.records() as u32,
                count: SortCount::Slots(case.count as u32),
                bin_min: input.grid.min,
                inv_cell: 1.0 / input.grid.cell,
                bins: input.grid.bins,
                sorted: sorted.as_ref(),
                order: order.as_ref(),
                gate: gate.as_ref(),
            },
            &LABELS,
        )
        .expect("the sort encodes");
    encoder.commit_and_wait_completed();

    let untouched_words = |buffer: &GpuBuffer| -> Vec<u32> {
        (0..(buffer.size / 4) as usize).map(untouched).collect()
    };
    let mut want_ranges = untouched_words(&ranges);
    let mut want_sorted = sorted.as_ref().map(untouched_words);
    let mut want_order = order.as_ref().map(untouched_words);
    if case.gate != Gate::Shut {
        let slots = [Some(input.records()), case.sorted, case.order]
            .into_iter()
            .flatten()
            .min()
            .expect("the input's slots");
        let (cells, members) = input.cpu_sort(case.count.min(slots));
        want_ranges[..2 * cells.len()].copy_from_slice(bytemuck::cast_slice(&cells));
        if let Some(want) = want_sorted.as_mut() {
            for (k, slot) in want[..slots * stride].chunks_exact_mut(stride).enumerate() {
                match members.get(k) {
                    Some(&i) => slot.copy_from_slice(input.record(i as usize)),
                    None => slot.fill(0),
                }
            }
        }
        if let Some(want) = want_order.as_mut() {
            for (k, slot) in want[..slots].iter_mut().enumerate() {
                *slot = members.get(k).copied().unwrap_or(u32::MAX);
            }
        }
    }
    assert_words(
        &format!("{what}: cell ranges"),
        &read_words(&ranges),
        &want_ranges,
        2,
    );
    if let (Some(buffer), Some(want)) = (&sorted, &want_sorted) {
        assert_words(
            &format!("{what}: sorted"),
            &read_words(buffer),
            want,
            stride,
        );
    }
    if let (Some(buffer), Some(want)) = (&order, &want_order) {
        assert_words(&format!("{what}: order"), &read_words(buffer), want, 1);
    }
}

fn prepared_sorter(device: &GpuDevice) -> ParticleSorter {
    let mut sorter = ParticleSorter::default();
    sorter.prepare(device);
    sorter
}

/// 2²⁰ liquid particle records on binary-fraction cells, where a record on a
/// face is exactly on it: every combination of wired outputs, and a count
/// short of the records.
#[test]
fn sort_particles_into_cells_matches_the_cpu_sort_at_a_million_records() {
    let device = manifold_gpu::testkit::test_device();
    let mut sorter = prepared_sorter(&device);
    let n = 1 << 20;
    let grid = Grid {
        min: [-2.0, -0.5, -1.25],
        cell: 0.0625,
        bins: [64, 48, 40],
    };
    let input = Input::build(LIQUID_PARTICLE_READ, grid, n, 34, &CROWDED, 0x5eed_0001);
    input.check_fixture();
    let particles = input.upload(&device);
    let short = n - 777;
    for (count, sorted, order) in [
        (n, Some(n), Some(n)),
        (short, Some(n), Some(n)),
        (short, Some(n), None),
        (short, None, Some(n)),
        (short, None, None),
    ] {
        sort_and_check(
            &device,
            &mut sorter,
            &input,
            &particles,
            &Case {
                count,
                sorted,
                order,
                gate: Gate::Unwired,
            },
        );
    }
}

/// 10⁵ liquid particle records where the cell and the corner have no exact
/// binary form, so a record on a face bins to whichever side the f32
/// arithmetic rounds it; outputs shorter than the records (the slots they
/// hold cap the count) and longer (nothing past the records is written).
#[test]
fn sort_particles_into_cells_matches_the_cpu_sort_where_faces_round() {
    let device = manifold_gpu::testkit::test_device();
    let mut sorter = prepared_sorter(&device);
    let n = 100_000;
    let grid = Grid {
        min: [-1.3, 0.1, -0.7],
        cell: 0.07,
        bins: [37, 23, 29],
    };
    let input = Input::build(
        LIQUID_PARTICLE_READ,
        grid,
        n,
        16,
        &[1, 2, 32, 33, 250, 1000, 1500],
        0x5eed_0002,
    );
    input.check_fixture();
    let particles = input.upload(&device);
    for (count, sorted, order) in [
        (n, Some(n), Some(n)),
        (n - 123, Some(90_000), Some(n + 100)),
        (n, Some(n + 64), Some(80_000)),
    ] {
        sort_and_check(
            &device,
            &mut sorter,
            &input,
            &particles,
            &Case {
                count,
                sorted,
                order,
                gate: Gate::Unwired,
            },
        );
    }
}

/// A shut clock gate writes no output word, before and after open sorts
/// on the same sorter; an open one sorts as an unwired one does.
#[test]
fn sort_particles_into_cells_writes_nothing_through_a_shut_gate() {
    let device = manifold_gpu::testkit::test_device();
    let mut sorter = prepared_sorter(&device);
    let n = 100_000;
    let grid = Grid {
        min: [-1.0, 0.0, -1.0],
        cell: 0.125,
        bins: [16, 16, 16],
    };
    let input = Input::build(
        LIQUID_PARTICLE_READ,
        grid,
        n,
        11,
        &[1, 33, 250, 1000],
        0x5eed_0003,
    );
    input.check_fixture();
    let particles = input.upload(&device);
    for (sorted, order, gate) in [
        (Some(n), Some(n), Gate::Shut),
        (Some(n), Some(n), Gate::Open),
        (None, Some(n), Gate::Shut),
        (Some(n), None, Gate::Open),
        (Some(n), Some(n), Gate::Unwired),
        (Some(n), Some(n), Gate::Shut),
    ] {
        sort_and_check(
            &device,
            &mut sorter,
            &input,
            &particles,
            &Case {
                count: n - 5,
                sorted,
                order,
                gate,
            },
        );
    }
}

/// Matter points (live by id and a finite position) and whitewater slots
/// (live by kind and a finite position) sort through `order` alone, at
/// their own record strides, with dense bins on most of the grid.
#[test]
fn sort_particles_into_cells_matches_the_cpu_sort_for_matter_and_whitewater() {
    let device = manifold_gpu::testkit::test_device();
    let mut sorter = prepared_sorter(&device);
    let n = 100_000;
    let grid = Grid {
        min: [-1.0, 0.0, -1.0],
        cell: 0.125,
        bins: [16, 16, 16],
    };
    let matter = record_read(&ArrayType::of_known::<MatterPoint>())
        .expect("a matter point has a position and an id");
    for (read, seed) in [
        (matter, 0x5eed_0004),
        (whitewater_record_read(), 0x5eed_0005),
    ] {
        let input = Input::build(read, grid, n, 11, &[1, 33, 250, 1000], seed);
        input.check_fixture();
        let particles = input.upload(&device);
        for (count, gate) in [(n, Gate::Unwired), (n - 99, Gate::Open)] {
            sort_and_check(
                &device,
                &mut sorter,
                &input,
                &particles,
                &Case {
                    count,
                    sorted: None,
                    order: Some(n),
                    gate,
                },
            );
        }
    }
}

/// A count record bounds the sort. 300 records sit in a 1024-slot buffer; the
/// slots past them look live (a positive radius) with NaN positions, so a pass
/// that read one would bin it and move the ranges. The ranges and the sorted
/// slots up to the live end equal the 300-slot sort of those records alone, and
/// the outputs keep their words past it: the record skips the tail clear.
#[test]
fn sort_particles_into_cells_through_a_count_record_ignores_the_slots_past_it() {
    let device = manifold_gpu::testkit::test_device();
    let grid = Grid {
        min: [-1.0, 0.0, -1.0],
        cell: 0.125,
        bins: [16, 16, 16],
    };
    let (records, slots) = (300, 1024);
    let clean = Input::build(
        LIQUID_PARTICLE_READ,
        grid,
        records,
        11,
        &[1, 33, 100],
        0x5eed_0007,
    );
    clean.check_fixture();
    let stride = clean.read.stride_words as usize;
    let (cells, members) = clean.cpu_sort(records);
    let live = members.len();
    assert!(
        (201..records).contains(&live),
        "the fixture holds dead records: {live} of {records} live"
    );
    // The 300-slot sort of the clean records is the CPU sort, word for word.
    let clean_particles = clean.upload(&device);
    sort_and_check(
        &device,
        &mut prepared_sorter(&device),
        &clean,
        &clean_particles,
        &Case {
            count: records,
            sorted: Some(records),
            order: Some(records),
            gate: Gate::Unwired,
        },
    );

    let mut words = clean.words.clone();
    let nan = f32::NAN.to_bits();
    for _ in records..slots {
        words.extend_from_slice(&[nan, nan, nan, 0.05_f32.to_bits(), 1, 2, 3, 7]);
    }
    let particles = device.create_buffer(words.len() as u64 * 4, BufferUse::Upload);
    // SAFETY: a shared buffer of exactly these words; no GPU work in flight.
    unsafe { particles.write(0, bytemuck::cast_slice(&words)) };
    // The identity's record: the allocator words, then `[groups, 1, 1, n]`.
    let identity = device.create_buffer(32, BufferUse::Upload);
    let groups = (records as u32).div_ceil(256);
    // SAFETY: a shared buffer of eight words; no GPU work in flight.
    unsafe {
        identity.write(
            0,
            bytemuck::cast_slice(&[9_u32, 1, 0, 0, groups, 1, 1, records as u32]),
        )
    };
    let sorted = untouched_buffer(&device, slots * stride);
    let order = untouched_buffer(&device, slots);
    let mut sorter = prepared_sorter(&device);
    sorter
        .reserve_ranges(&device, grid.bins)
        .expect("the cell ranges");
    let ranges = sorter.ranges().expect("the cell ranges").clone();
    fill_untouched(&ranges);
    let mut encoder = device.create_encoder("sort count record proof");
    sorter
        .encode(
            &device,
            &mut encoder,
            &SortJob {
                particles: &particles,
                read: clean.read,
                capacity: slots as u32,
                count: SortCount::Record {
                    buffer: &identity,
                    offset: COUNT_OFFSET,
                },
                bin_min: grid.min,
                inv_cell: 1.0 / grid.cell,
                bins: grid.bins,
                sorted: Some(&sorted),
                order: Some(&order),
                gate: None,
            },
            &LABELS,
        )
        .expect("the sort encodes");
    encoder.commit_and_wait_completed();

    assert_words(
        "cell ranges",
        &read_words(&ranges),
        bytemuck::cast_slice(&cells),
        2,
    );
    let mut want_sorted: Vec<u32> = (0..slots * stride).map(untouched).collect();
    let mut want_order: Vec<u32> = (0..slots).map(untouched).collect();
    for (slot, &input) in members.iter().enumerate() {
        want_sorted[slot * stride..(slot + 1) * stride]
            .copy_from_slice(clean.record(input as usize));
        want_order[slot] = input;
    }
    assert_words("sorted", &read_words(&sorted), &want_sorted, stride);
    assert_words("order", &read_words(&order), &want_order, 1);
}

/// The pooled sort keys each live particle on the pool index of its world
/// cell, `floor((x - origin) * inv_cell)` with no clamp, through whatever
/// slot its tile holds; the particles of a tile the table does not hold, the
/// ones outside the fixture's box among them, share the canonical element's
/// range. Each range keeps input order. The box sits across the world origin
/// (cells -8..12 on x, 0..13 on y, -8..1 on z, so tiles -1, 0 and 1 on x and
/// negative cells on z), under three tables: slots in tile order, a
/// permutation, and a short pool with two of the box's tiles absent.
#[test]
fn sort_particles_into_cells_pooled_keys_world_cells_through_the_table() {
    let device = manifold_gpu::testkit::test_device();
    let n = 20_000;
    let cell = 0.125;
    let origin = lattice_origin(cell);
    // Corner of world cell (-8, 0, -8): a binary fraction, so the fixture's
    // faces fall exactly on world cell faces.
    let grid = Grid {
        min: [origin[0] - 8.0 * cell, origin[1], origin[2] - 8.0 * cell],
        cell,
        bins: [20, 13, 9],
    };
    let input = Input::build(LIQUID_PARTICLE_READ, grid, n, 6, &[1, 33, 250], 0x5eed_0006);
    input.check_fixture();
    let particles = input.upload(&device);
    let inv_cell = 1.0 / cell;
    let world_cell = |i: usize| -> [i32; 3] {
        let p = input.position(i).map(f32::from_bits);
        std::array::from_fn(|a| ((p[a] - origin[a]) * inv_cell).floor() as i32)
    };
    let tiles: Vec<[i32; 3]> = (0..12).map(|t| [t % 3 - 1, t / 3 % 2, t / 6 - 1]).collect();
    let identity: Vec<Option<u32>> = (0..12).map(Some).collect();
    let permuted: Vec<Option<u32>> = (0..12).map(|t| Some((5 * t + 3) % 12)).collect();
    let mut short = vec![None; 12];
    for (i, t) in (0..12).filter(|&t| t != 4 && t != 7).enumerate() {
        short[t] = Some(9 - i as u32);
    }
    for (name, slot_of, pool) in [
        ("tile order", identity, 12u32),
        ("permuted", permuted, 12),
        ("short pool", short, 10),
    ] {
        let held: Vec<([i32; 3], u32)> = tiles
            .iter()
            .zip(&slot_of)
            .filter_map(|(&tile, slot)| slot.map(|slot| (tile, slot)))
            .collect();
        let words = table_words_of(pool, &held);
        assert_eq!(words.len() as u64, table_words(pool));
        let table = device.create_buffer(4 * words.len() as u64, BufferUse::Upload);
        // SAFETY: a shared buffer of exactly these words; no GPU work in flight.
        unsafe { table.write(0, bytemuck::cast_slice(&words)) };
        let order = untouched_buffer(&device, n);
        let mut sorter = ParticleSorter::default();
        sorter.prepare_pooled(&device);
        sorter
            .reserve_keys(&device, pool_len(pool))
            .expect("the cell ranges");
        let mut encoder = device.create_encoder("pooled sort proof");
        sorter
            .encode_pooled(
                &device,
                &mut encoder,
                &SortJob {
                    particles: &particles,
                    read: input.read,
                    capacity: n as u32,
                    count: SortCount::Slots(n as u32),
                    bin_min: origin,
                    inv_cell,
                    bins: [0; 3],
                    sorted: None,
                    order: Some(&order),
                    gate: None,
                },
                &table,
                pool,
                &LABELS,
            )
            .expect("the pooled sort encodes");
        encoder.commit_and_wait_completed();

        // The CPU sort: the key of each live particle, ranges by key, members
        // in input order.
        let canonical = pool_len(pool) as usize - 1;
        let key_of = |i: usize| -> usize {
            let c = world_cell(i);
            let at = tiles.iter().position(|&t| t == tile_of_cell(c));
            match at.and_then(|t| slot_of[t]) {
                Some(slot) => slot as usize * 512 + local_index(c) as usize,
                None => canonical,
            }
        };
        let keyed: Vec<Option<usize>> = (0..n).map(|i| input.live(i).then(|| key_of(i))).collect();
        let mut want_ranges = vec![[0u32; 2]; pool_len(pool) as usize];
        for key in keyed.iter().flatten() {
            want_ranges[*key][1] += 1;
        }
        let mut start = 0;
        for range in &mut want_ranges {
            range[0] = start;
            start += range[1];
        }
        let live = start as usize;
        let mut next: Vec<u32> = want_ranges.iter().map(|range| range[0]).collect();
        let mut want_order = vec![u32::MAX; n];
        for (i, key) in keyed.iter().enumerate() {
            if let Some(key) = key {
                want_order[next[*key] as usize] = i as u32;
                next[*key] += 1;
            }
        }
        let canonical_count = want_ranges[canonical][1];
        assert_words(
            &format!("{name}: cell ranges"),
            &read_words(sorter.ranges().expect("the cell ranges")),
            bytemuck::cast_slice(&want_ranges),
            2,
        );
        assert_words(
            &format!("{name}: order"),
            &read_words(&order),
            &want_order,
            1,
        );
        // The fixture reaches every case it names.
        assert!(live > n / 2, "{name}: most particles are live");
        assert!(
            canonical_count > 0,
            "{name}: particles outside the held tiles"
        );
        if name == "short pool" {
            let held_cells: usize = want_ranges[..canonical].iter().map(|r| r[1] as usize).sum();
            assert!(held_cells > 0 && held_cells + (canonical_count as usize) == live);
        }
    }
}
