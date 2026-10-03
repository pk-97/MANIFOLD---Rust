//! The publication copy behind `node.liquid_frame`: live records sorted by
//! birth id, ties in source order, then a zeroed tail. A stable LSD radix
//! sort of (key, source index) pairs, 4 bits a pass: per pass a tile digit
//! histogram, the shared PrefixScan over the digit-major counts, and a
//! scatter ranked in workgroup memory; then one gather moves the records.
//! Section 2.5 audit: no multi-bit sort exists in portable WGSL, PrefixScan is
//! the reuse point. Barriered and multi-pass, it stays a hand-written helper
//! inside liquid_frame's cross-frame boundary (ADDING_PRIMITIVES.md
//! exclusion 2).
use super::liquid_stats::with_stats_layout;
use super::prefix_scan::{PrefixScan, ScanLabels, storage_words};
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

const SHADER: &str = include_str!("shaders/particle_publication.wgsl");
/// Elements a workgroup sorts per pass (the shader's `TILE`).
const TILE: u32 = 256;
const DIGITS: u32 = 16;
/// Four key bits a pass. An even count leaves the sorted pairs in `pairs[0]`.
const PASSES: u32 = 8;
const _: () = assert!(PASSES * 4 == 32 && PASSES.is_multiple_of(2));
const SCAN: ScanLabels = ScanLabels { blocks: "particle_publication.scan", add: "particle_publication.scan_add" };

/// Sort tiles over `records`; one when there are none, so the live count is
/// still written.
fn tiles(records: u32) -> u32 {
    records.div_ceil(TILE).max(1)
}

/// One pair array: `slots` keys, then `slots` source indices.
fn pair_bytes(slots: u32) -> u64 {
    u64::from(slots) * 8
}

/// Everything the publisher allocates for a target of `slots` records: two
/// pair arrays, the live-count word and the digit scan's storage.
pub(crate) fn scratch_bytes(slots: u32) -> u64 {
    let slots = slots.max(1);
    2 * pair_bytes(slots) + 16 + storage_words((DIGITS * tiles(slots)) as usize) as u64 * 4
}

#[derive(Default)]
pub struct ParticlePublication {
    pipelines: Option<[GpuComputePipeline; 4]>,
    pairs: Option<[GpuBuffer; 2]>,
    live: Option<GpuBuffer>,
    scan: PrefixScan,
}
pub(crate) struct Publication<'a> {
    pub source: &'a GpuBuffer,
    pub target: &'a GpuBuffer,
    pub identity: &'a GpuBuffer,
    pub stats: &'a GpuBuffer,
    pub metadata: &'a GpuBuffer,
    pub count: u32,
}
impl ParticlePublication {
    pub(crate) fn prepare(&mut self, device: &GpuDevice) {
        if self.pipelines.is_none() {
            let shader = with_stats_layout(SHADER);
            self.pipelines = Some(["first_upsweep", "upsweep", "downsweep", "gather"].map(
                |entry| device.create_compute_pipeline(&shader, entry, "particle_publication"),
            ));
        }
        self.scan.prepare(device);
    }
    pub(crate) fn encode(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        job: Publication<'_>,
    ) -> Result<(), String> {
        let slots = (job.target.size / 32) as u32;
        if slots == 0
            || u64::from(job.count) * 32 > job.source.size
            || job.count > slots
            || job.identity.size < 16
            || job.metadata.size < 16
            || job.stats.size < u64::from(super::liquid_stats::LIQUID_STATS_WORDS) * 4
        {
            return Err("particle publication buffers do not cover dispatch extent".into());
        }
        // Grows only with the target, all or nothing.
        if self.live.is_none() || self.pairs.as_ref().is_none_or(|pairs| pairs[0].size < pair_bytes(slots)) {
            crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
                device.modifier_memory_snapshot(),
                scratch_bytes(slots),
            )
            .map_err(|error| error.to_string())?;
            let pairs = [
                device.try_create_buffer_shared(pair_bytes(slots))?,
                device.try_create_buffer_shared(pair_bytes(slots))?,
            ];
            let live = device.try_create_buffer_shared(16)?;
            self.pairs = Some(pairs);
            self.live = Some(live);
        }
        let scan = self.scan.buffer(device, (DIGITS * tiles(slots)) as usize)?.clone();
        let pairs = self.pairs.as_ref().expect("publication reserved");
        let live = self.live.as_ref().expect("publication reserved");
        let pipes = self.pipelines.as_ref().expect("publication prepared");
        let sort_tiles = tiles(job.count);
        let groups = [sort_tiles, 1, 1];
        let cap = (pairs[0].size / 8) as u32;
        let mut params = [job.count, slots, 0, sort_tiles, cap, 1, 0, 0];
        enc.dispatch_compute(
            &pipes[0],
            &[uniform(&params), binding(1, job.source), binding(3, &scan), binding(8, &pairs[0])],
            groups,
            "particle_publication.upsweep",
        );
        enc.compute_memory_barrier_buffers();
        for pass in 0..PASSES {
            params[2] = pass * 4;
            params[5] = u32::from(pass == 0);
            let input = &pairs[pass as usize % 2];
            let output = &pairs[(pass as usize + 1) % 2];
            if pass > 0 {
                enc.dispatch_compute(
                    &pipes[1],
                    &[uniform(&params), binding(3, &scan), binding(7, input), binding(9, live)],
                    groups,
                    "particle_publication.upsweep",
                );
                enc.compute_memory_barrier_buffers();
            }
            self.scan.encode_labelled(enc, (DIGITS * sort_tiles) as usize, SCAN);
            enc.dispatch_compute(
                &pipes[2],
                &[uniform(&params), binding(3, &scan), binding(7, input), binding(8, output), binding(9, live)],
                groups,
                "particle_publication.downsweep",
            );
            enc.compute_memory_barrier_buffers();
        }
        enc.dispatch_compute(
            &pipes[3],
            &[
                uniform(&params),
                binding(1, job.source),
                binding(2, job.target),
                binding(4, job.identity),
                binding(5, job.stats),
                binding(6, job.metadata),
                binding(7, &pairs[0]),
                binding(9, live),
            ],
            [slots.div_ceil(TILE), 1, 1],
            "particle_publication.gather",
        );
        enc.compute_memory_barrier_buffers();
        Ok(())
    }
    /// Bytes held now: `scratch_bytes` of the largest target published.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(super) fn held_bytes(&mut self, device: &GpuDevice) -> u64 {
        let pairs = self.pairs.as_ref().map_or(0, |pairs| pairs[0].size + pairs[1].size);
        let live = self.live.as_ref().map_or(0, |live| live.size);
        pairs + live + self.scan.buffer(device, 1).map_or(0, |scan| scan.size)
    }
}
fn uniform(params: &[u32; 8]) -> GpuBinding<'_> {
    GpuBinding::Bytes {
        binding: 0,
        data: bytemuck::cast_slice(params),
    }
}
fn binding(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}
/// The 1-bit publisher this module replaced, transcribed line by line from its
/// shader: the oracle every publication proof compares bytes against.
#[cfg(test)]
pub(super) mod reference {
    use super::super::liquid_stats::NARROW_BAND_SHORTAGE_WORD;
    use crate::node_graph::fluid_particles::FluidParticle;

    /// `radius > 0.0` as that shader's GPU compare evaluated it: subnormals
    /// flush to zero (measured on the 1-bit publisher, 2026-10-04).
    pub(crate) fn live(radius: f32) -> bool {
        radius >= f32::MIN_POSITIVE
    }

    /// The target's `slots` records and the four metadata words.
    pub(crate) fn publish(
        source: &[FluidParticle],
        count: u32,
        slots: u32,
        identity: [u32; 4],
        stats: &[u32],
    ) -> (Vec<FluidParticle>, [u32; 4]) {
        let slots = slots as usize;
        // initialize: live records in place, everything else zeroed.
        let mut src: Vec<FluidParticle> = (0..slots)
            .map(|i| {
                let keep = (i as u32) < count && live(source[i].position_radius[3]);
                if keep { source[i] } else { FluidParticle::default() }
            })
            .collect();
        let mut dst = vec![FluidParticle::default(); slots];
        let mut scan = vec![0u32; slots];
        for bit in 0..=32u32 {
            let zero_bit = |p: &FluidParticle| {
                if bit == 32 { live(p.position_radius[3]) } else { p.id & (1 << bit) == 0 }
            };
            let mut sum = 0u32;
            for (word, particle) in scan.iter_mut().zip(&src) {
                sum += u32::from(zero_bit(particle));
                *word = sum;
            }
            let total = scan[slots - 1];
            for (i, particle) in src.iter().enumerate() {
                let mut destination = total.wrapping_add(i as u32).wrapping_sub(scan[i]);
                if zero_bit(particle) {
                    destination = scan[i] - 1;
                }
                dst[destination as usize] = *particle;
            }
            std::mem::swap(&mut src, &mut dst);
        }
        let accepted = stats[0] == 0 && stats[NARROW_BAND_SHORTAGE_WORD as usize] == 0 && identity[3] == 0;
        (src, [scan[slots - 1], identity[1], u32::from(accepted), 0])
    }

    /// The same contract stated plainly: live records stably sorted by id,
    /// then zeroed records.
    pub(crate) fn stable_id_sort(source: &[FluidParticle], count: u32, slots: u32) -> Vec<FluidParticle> {
        let mut records: Vec<FluidParticle> =
            source[..count as usize].iter().copied().filter(|p| live(p.position_radius[3])).collect();
        records.sort_by_key(|p| p.id);
        records.resize(slots as usize, FluidParticle::default());
        records
    }
}

#[cfg(test)]
mod tests {
    use super::super::liquid_stats::LIQUID_STATS_WORDS;
    use super::{DIGITS, PASSES, TILE, reference, tiles};
    use crate::node_graph::fluid_particles::FluidParticle;

    /// Finite records, a fifth of them dead by each kind of non-positive or
    /// subnormal radius, ids under `mask`.
    fn records(seed: u64, count: u32, mask: u32) -> Vec<FluidParticle> {
        let mut seed = seed;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        (0..count)
            .map(|i| {
                let r = next();
                let radius = match r % 10 {
                    0 => 0.0,
                    1 => -0.0,
                    2 => -0.25,
                    3 => f32::from_bits(0x0000_0400),
                    _ => 0.05,
                };
                FluidParticle {
                    position_radius: [i as f32, 1.0, 2.0, radius],
                    velocity: [3.0, 4.0, 5.0],
                    id: (r >> 32) as u32 & mask,
                }
            })
            .collect()
    }

    const SIZES: [(u32, u32, u32); 9] = [
        (1, 1, u32::MAX),
        (7, 3, 3),
        (300, 0, u32::MAX),
        (300, 300, 15),
        (513, 400, u32::MAX),
        (2000, 1999, 0x8000_00ff),
        (4097, 4097, 0xf000_000f),
        (20_000, 17_000, u32::MAX),
        (70_001, 70_001, 0x00ff_ffff),
    ];

    #[test]
    fn particle_publication_reference_is_a_stable_live_id_sort() {
        for (slots, count, mask) in SIZES {
            let source = records(0x2545_f491_4f6c_dd1d ^ u64::from(slots), count, mask);
            let identity = [9, 0xdead_beef, 0, 0];
            let (frame, metadata) = reference::publish(&source, count, slots, identity, &[0; LIQUID_STATS_WORDS as usize]);
            let want = reference::stable_id_sort(&source, count, slots);
            assert_eq!(bytemuck::cast_slice::<_, u32>(&frame), bytemuck::cast_slice::<_, u32>(&want), "slots {slots}");
            let live = source.iter().filter(|p| reference::live(p.position_radius[3])).count() as u32;
            assert_eq!(metadata, [live, 0xdead_beef, 1, 0]);
        }
    }

    /// The shader's passes on the CPU, word for word, over buffers of exactly
    /// the sizes `encode` allocates (an index past one panics): pairs of
    /// `slots` capacity, `DIGITS × tiles` scanned counts.
    fn radix_model(source: &[FluidParticle], count: u32, slots: u32) -> Vec<FluidParticle> {
        const DEAD: u32 = u32::MAX;
        let cap = slots as usize;
        let tile = TILE as usize;
        let tiles = tiles(count) as usize;
        let mut pairs = [vec![0u32; 2 * cap], vec![0u32; 2 * cap]];
        let mut scan = vec![0u32; DIGITS as usize * tiles];
        let mut live = 0usize;
        let one_hot = |valid: bool, digit: u32| {
            let mut words = [0u32; 8];
            if valid {
                words[digit as usize >> 1] = 1 << ((digit & 1) * 16);
            }
            words
        };
        let add = |into: &mut [u32; 8], from: [u32; 8]| into.iter_mut().zip(from).for_each(|(a, b)| *a += b);
        let field = |words: &[u32; 8], digit: u32| (words[digit as usize >> 1] >> ((digit & 1) * 16)) & 0xffff;
        for pass in 0..PASSES {
            let (shift, first) = (pass * 4, pass == 0);
            for t in 0..tiles {
                let mut total = [0u32; 8];
                for i in t * tile..(t + 1) * tile {
                    let (valid, key) = if first && i < count as usize {
                        let words: &[u32] = bytemuck::cast_slice(std::slice::from_ref(&source[i]));
                        let valid = words[3].wrapping_sub(0x0080_0000) <= 0x7f00_0000;
                        pairs[0][i] = words[7];
                        pairs[0][cap + i] = if valid { i as u32 } else { DEAD };
                        (valid, words[7])
                    } else if !first && i < live {
                        (true, pairs[pass as usize % 2][i])
                    } else {
                        (false, 0)
                    };
                    add(&mut total, one_hot(valid, (key >> shift) & 15));
                }
                for digit in 0..DIGITS {
                    scan[digit as usize * tiles + t] = field(&total, digit);
                }
            }
            let mut sum = 0;
            for word in &mut scan {
                sum += *word;
                *word = sum;
            }
            let input = pairs[pass as usize % 2].clone();
            let output = &mut pairs[(pass as usize + 1) % 2];
            for t in 0..tiles {
                let elements: Vec<(bool, u32, u32)> = (t * tile..(t + 1) * tile)
                    .map(|i| {
                        let (valid, index) = if first && i < count as usize {
                            (input[cap + i] != DEAD, input[cap + i])
                        } else if !first && i < live {
                            (true, input[cap + i])
                        } else {
                            (false, 0)
                        };
                        (valid, if valid { input[i] } else { 0 }, index)
                    })
                    .collect();
                let mut totals = [0u32; 8];
                for &(valid, key, _) in &elements {
                    add(&mut totals, one_hot(valid, (key >> shift) & 15));
                }
                let mut through = [0u32; 8];
                for &(valid, key, index) in &elements {
                    let digit = (key >> shift) & 15;
                    add(&mut through, one_hot(valid, digit));
                    if valid {
                        let base = scan[digit as usize * tiles + t] - field(&totals, digit);
                        let destination = (base + field(&through, digit) - 1) as usize;
                        output[destination] = key;
                        output[cap + destination] = index;
                    }
                }
                if first && t == 0 {
                    live = scan[DIGITS as usize * tiles - 1] as usize;
                }
            }
        }
        (0..cap)
            .map(|k| if k < live { source[pairs[0][cap + k] as usize] } else { FluidParticle::default() })
            .collect()
    }

    #[test]
    fn particle_publication_radix_passes_match_the_reference() {
        for (slots, count, mask) in SIZES {
            let source = records(0x9e37_79b9_7f4a_7c15 ^ u64::from(count), count, mask);
            let (want, _) = reference::publish(&source, count, slots, [0; 4], &[0; LIQUID_STATS_WORDS as usize]);
            let got = radix_model(&source, count, slots);
            assert_eq!(bytemuck::cast_slice::<_, u32>(&got), bytemuck::cast_slice::<_, u32>(&want), "{count} of {slots}");
        }
    }

    #[test]
    fn particle_publication_shader_validates() {
        let shader = super::with_stats_layout(super::SHADER);
        let module = naga::front::wgsl::parse_str(&shader).unwrap();
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
    }
}
