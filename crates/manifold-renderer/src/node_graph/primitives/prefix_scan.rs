//! The multi-level inclusive prefix sum shared by `node.sort_particles_into_cells`
//! (bin starts), `node.running_total` and `node.whitewater_step`. One storage
//! buffer holds every level: level 0 at offset 0 (the values to scan), each
//! later level the 256-wide block totals of the one before. The last level is
//! one workgroup — one block, or a tail of up to [`TAIL`] values — so any
//! length up to 256 × [`TAIL`] scans in three dispatches (blocks, tail, add).
//! Not a primitive — a scan is barriered and multi-dispatch, so its atoms are
//! fusion boundaries (ADDING_PRIMITIVES.md, exclusion 1).

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

const SHADER: &str = include_str!("shaders/prefix_scan.wgsl");
const BLOCK: usize = 256;
/// The most one workgroup scans alone: each of its 256 threads sums a run of
/// 64, so a last level this long needs no further level.
const TAIL: usize = BLOCK * 64;
/// 256³ × TAIL values; every array in the graph is far below this.
const MAX_LEVELS: usize = 4;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScanParams {
    n: u32,
    offset: u32,
    parent: u32,
    has_parent: u32,
}

/// (offset, length) of each level for `n` values. Level 0 is scanned in
/// blocks whatever its length; a later level ends the chain once one
/// workgroup can scan it.
fn levels(n: usize) -> ([(usize, usize); MAX_LEVELS], usize) {
    let mut levels = [(0, 0); MAX_LEVELS];
    let mut count = 0;
    let (mut offset, mut length) = (0, n.max(1));
    loop {
        levels[count] = (offset, length);
        count += 1;
        let last = if count == 1 { length <= BLOCK } else { length <= TAIL };
        if last || count == MAX_LEVELS {
            break;
        }
        offset += length;
        length = length.div_ceil(BLOCK);
    }
    (levels, count)
}

/// Dispatches one `encode(n)` makes.
#[cfg(test)]
fn dispatches(n: usize) -> usize {
    let (_, count) = levels(n);
    2 * count - 1
}

/// Words of storage the scan needs for `n` values.
pub(crate) fn storage_words(n: usize) -> usize {
    let (levels, count) = levels(n);
    let (offset, length) = levels[count - 1];
    offset + length
}

#[derive(Default)]
pub(crate) struct PrefixScan {
    blocks: Option<GpuComputePipeline>,
    tail: Option<GpuComputePipeline>,
    add: Option<GpuComputePipeline>,
    buffer: Option<GpuBuffer>,
    words: usize,
}

/// Dispatch labels: the block passes and the add passes.
#[derive(Clone, Copy)]
pub(crate) struct ScanLabels {
    pub blocks: &'static str,
    pub add: &'static str,
}

impl ScanLabels {
    pub(crate) const DEFAULT: Self = Self { blocks: "prefix_scan.blocks", add: "prefix_scan.add" };
}

impl PrefixScan {
    /// Create the pipelines. Call before any early return (compile contract).
    pub(crate) fn prepare(&mut self, device: &GpuDevice) {
        if self.blocks.is_none() {
            self.blocks = Some(device.create_compute_pipeline(SHADER, "scan_blocks", "prefix_scan.blocks"));
        }
        if self.tail.is_none() {
            self.tail = Some(device.create_compute_pipeline(SHADER, "scan_tail", "prefix_scan.tail"));
        }
        if self.add.is_none() {
            self.add = Some(device.create_compute_pipeline(SHADER, "add_block_totals", "prefix_scan.add"));
        }
    }

    /// The storage buffer, sized for `n` values. Level 0 starts at offset 0.
    pub(crate) fn buffer(&mut self, device: &GpuDevice, n: usize) -> Result<&GpuBuffer, String> {
        let words = storage_words(n);
        if self.buffer.is_none() || self.words < words {
            let bytes = (words * 4) as u64;
            crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
                device.modifier_memory_snapshot(),
                bytes,
            )
            .map_err(|error| error.to_string())?;
            self.buffer = Some(device.try_create_buffer_shared(bytes)?);
            self.words = words;
        }
        Ok(self.buffer.as_ref().expect("scan storage allocated"))
    }

    /// Scan level 0 `[0, n)` in place. `prepare` and `buffer` first.
    pub(crate) fn encode(&self, encoder: &mut manifold_gpu::GpuEncoder, n: usize) {
        self.encode_labelled(encoder, n, ScanLabels::DEFAULT);
    }

    /// [`encode`](Self::encode) with the caller's dispatch labels, so a
    /// profile tells one scan site from another.
    pub(crate) fn encode_labelled(&self, encoder: &mut manifold_gpu::GpuEncoder, n: usize, labels: ScanLabels) {
        let blocks = self.blocks.as_ref().expect("scan pipelines prepared");
        let tail = self.tail.as_ref().expect("scan pipelines prepared");
        let add = self.add.as_ref().expect("scan pipelines prepared");
        let buffer = self.buffer.as_ref().expect("scan storage prepared");
        let (levels, count) = levels(n);
        let params = |level: usize| {
            let (offset, length) = levels[level];
            let parent = if level + 1 < count { Some(levels[level + 1].0) } else { None };
            ScanParams {
                n: length as u32,
                offset: offset as u32,
                parent: parent.unwrap_or(0) as u32,
                has_parent: u32::from(parent.is_some()),
            }
        };
        let dispatch = |encoder: &mut manifold_gpu::GpuEncoder, pipeline, level: usize, groups: u32, label| {
            let uniforms = params(level);
            encoder.dispatch_compute(
                pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                    GpuBinding::Buffer { binding: 1, buffer, offset: 0 },
                ],
                [groups, 1, 1],
                label,
            );
            encoder.compute_memory_barrier_buffers();
        };
        let groups = |level: usize| levels[level].1.div_ceil(BLOCK) as u32;
        for level in 0..count - 1 {
            dispatch(encoder, blocks, level, groups(level), labels.blocks);
        }
        let last = count - 1;
        if levels[last].1 <= BLOCK {
            dispatch(encoder, blocks, last, 1, labels.blocks);
        } else {
            dispatch(encoder, tail, last, 1, labels.blocks);
        }
        for level in (0..count - 1).rev() {
            dispatch(encoder, add, level, groups(level), labels.add);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_scan_levels_cover_every_block() {
        assert_eq!(levels(1).1, 1);
        assert_eq!(levels(255).1, 1);
        assert_eq!(levels(256).1, 1);
        let (l, count) = levels(257);
        assert_eq!(count, 2);
        assert_eq!(l[1], (257, 2));
        let (l, count) = levels((1 << 20) + 3);
        assert_eq!(count, 2);
        assert_eq!(l[1], ((1 << 20) + 3, 4097));
        assert_eq!(storage_words((1 << 20) + 3), (1 << 20) + 3 + 4097);
        let (l, count) = levels(BLOCK * TAIL + 1);
        assert_eq!(count, 3);
        assert_eq!(l[1], (BLOCK * TAIL + 1, TAIL + 1));
        assert_eq!(l[2], (BLOCK * TAIL + 1 + TAIL + 1, 65));
    }

    #[test]
    fn prefix_scan_is_three_dispatches_up_to_the_tail() {
        for n in [1, 255, 256] {
            assert_eq!(dispatches(n), 1, "{n}");
        }
        for n in [257, 65_536, 65_537, (1 << 20) + 3, 343_000, BLOCK * TAIL] {
            assert_eq!(dispatches(n), 3, "{n}");
        }
        assert_eq!(dispatches(BLOCK * TAIL + 1), 5);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;

    /// The GPU scan equals the CPU inclusive scan word for word, at every
    /// level shape: one block, blocks plus a one-block last level, blocks
    /// plus a tail (the three-dispatch case up to 256 × TAIL), and past it.
    #[test]
    fn prefix_scan_matches_cpu_at_every_level_shape() {
        let device = crate::test_device();
        let mut scan = PrefixScan::default();
        scan.prepare(&device);
        let mut seed = 0x9e37_79b9_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % 7
        };
        for n in [1usize, 255, 256, 257, 65_536, 65_537, (1 << 20) + 3, BLOCK * TAIL, BLOCK * TAIL + 1] {
            let values: Vec<u32> = (0..n).map(|_| next()).collect();
            let want: Vec<u32> = values
                .iter()
                .scan(0u32, |total, &v| {
                    *total += v;
                    Some(*total)
                })
                .collect();
            let buffer = scan.buffer(&device, n).expect("scan storage");
            // SAFETY: a shared buffer of at least n words; no GPU work in flight.
            unsafe { buffer.write(0, bytemuck::cast_slice(&values)) };
            let mut encoder = device.create_encoder("prefix scan proof");
            scan.encode(&mut encoder, n);
            encoder.commit_and_wait_completed();
            let buffer = scan.buffer.as_ref().expect("scan storage");
            let ptr = buffer.mapped_ptr().expect("shared buffer");
            // SAFETY: shared buffer holding at least n words; GPU work done.
            let got: &[u32] = bytemuck::cast_slice(unsafe { std::slice::from_raw_parts(ptr, n * 4) });
            let first_bad = got.iter().zip(&want).position(|(g, w)| g != w);
            assert_eq!(first_bad, None, "n = {n} ({} dispatches): word {:?} differs", dispatches(n), first_bad);
        }
    }
}
