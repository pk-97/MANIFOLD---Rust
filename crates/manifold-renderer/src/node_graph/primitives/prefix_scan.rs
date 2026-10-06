//! The multi-level inclusive prefix sum shared by `node.sort_particles_into_cells`
//! (bin starts), `node.running_total` and `node.whitewater_step`. Level 0 is
//! the values to scan; each later level holds the 256-wide block totals of the
//! one before. Level 0 lives either in this scan's own storage
//! ([`PrefixScan::encode_labelled`]) or in the caller's input and output buffers
//! ([`PrefixScan::encode_into`], no copies either side); the later levels
//! always live in the scan's storage. The last level is one workgroup — one
//! block, or a tail of up to [`TAIL`] values — so any length up to
//! 256 × [`TAIL`] scans in three dispatches (blocks, tail, add). Not a
//! primitive — a scan is barriered and multi-dispatch, so its atoms are fusion
//! boundaries (ADDING_PRIMITIVES.md, exclusion 1).

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
    src_offset: u32,
    dst_offset: u32,
    parent: u32,
    has_parent: u32,
    _pad: [u32; 3],
}

/// (offset, length) of each level for `n` values, laid out one after another
/// from offset 0. Level 0 is scanned in blocks whatever its length; a later
/// level ends the chain once one workgroup can scan it.
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

/// Dispatches one scan over `n` values makes.
#[cfg(test)]
fn dispatches(n: usize) -> usize {
    let (_, count) = levels(n);
    2 * count - 1
}

/// A zeroed clock plan: the gate that never switches a pass off.
pub(crate) fn open_gate(device: &GpuDevice) -> GpuBuffer {
    let gate = device.create_buffer_shared(48);
    gate.zero_fill();
    gate
}

/// Words of storage every level of a scan over `n` values needs.
pub(crate) fn storage_words(n: usize) -> usize {
    let (levels, count) = levels(n);
    let (offset, length) = levels[count - 1];
    offset + length
}

/// Words of storage the levels past level 0 need (never zero: a scan with no
/// parent level still binds the storage).
fn parent_words(n: usize) -> usize {
    (storage_words(n) - n.max(1)).max(1)
}

#[derive(Default)]
pub(crate) struct PrefixScan {
    blocks: Option<GpuComputePipeline>,
    tail: Option<GpuComputePipeline>,
    add: Option<GpuComputePipeline>,
    buffer: Option<GpuBuffer>,
    words: usize,
    /// Zeros, the gate an ungated scan binds (prefix_scan.wgsl `gate`).
    open: Option<GpuBuffer>,
}

/// Dispatch labels: the block and tail passes, and the add passes.
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
        if self.open.is_none() {
            self.open = Some(open_gate(device));
        }
    }

    /// The storage buffer sized for every level of `n` values, level 0 at
    /// offset 0. Pairs with [`Self::encode_labelled`].
    pub(crate) fn buffer(&mut self, device: &GpuDevice, n: usize) -> Result<&GpuBuffer, String> {
        self.storage(device, storage_words(n))
    }

    /// The storage `buffer` sized, for a caller holding `&self`.
    pub(crate) fn buffer_ref(&self) -> &GpuBuffer {
        self.buffer.as_ref().expect("scan storage prepared")
    }

    /// The storage buffer sized for the levels past level 0 of `n` values.
    /// Pairs with [`Self::encode_into`].
    pub(crate) fn parents(&mut self, device: &GpuDevice, n: usize) -> Result<&GpuBuffer, String> {
        self.storage(device, parent_words(n))
    }

    fn storage(&mut self, device: &GpuDevice, words: usize) -> Result<&GpuBuffer, String> {
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

    /// Scan level 0 `[0, n)` of the storage in place, under the caller's
    /// dispatch labels so a profile tells one scan site from another.
    /// `prepare` and `buffer` first.
    pub(crate) fn encode_labelled(&self, encoder: &mut manifold_gpu::GpuEncoder, n: usize, labels: ScanLabels) {
        let buffer = self.buffer.as_ref().expect("scan storage prepared");
        self.encode_levels(encoder, n, buffer, buffer, 0, labels, None);
    }

    /// [`Self::encode_labelled`] under a GPU FLIP clock plan: an inactive
    /// slot's (live, no time to step) scan runs no pass.
    pub(crate) fn encode_labelled_gated(&self, encoder: &mut manifold_gpu::GpuEncoder, n: usize, labels: ScanLabels, gate: &GpuBuffer) {
        let buffer = self.buffer.as_ref().expect("scan storage prepared");
        self.encode_levels(encoder, n, buffer, buffer, 0, labels, Some(gate));
    }

    /// Scan `src[0, n)` into `dst[0, n)`, the later levels in the storage.
    /// `prepare` and `parents` first. `src`, `dst`, and this scan's storage
    /// must all be distinct buffers: parent totals start at storage word 0.
    pub(crate) fn encode_into(
        &self,
        encoder: &mut manifold_gpu::GpuEncoder,
        n: usize,
        src: &GpuBuffer,
        dst: &GpuBuffer,
    ) {
        self.encode_levels(encoder, n, src, dst, n.max(1), ScanLabels::DEFAULT, None);
    }

    /// `level0_offset` is where level 0 would sit in the storage's layout:
    /// the later levels' storage offsets are the layout's minus it.
    fn encode_levels(
        &self,
        encoder: &mut manifold_gpu::GpuEncoder,
        n: usize,
        src: &GpuBuffer,
        dst: &GpuBuffer,
        level0_offset: usize,
        labels: ScanLabels,
        gate: Option<&GpuBuffer>,
    ) {
        let gate = gate.or(self.open.as_ref()).expect("scan pipelines prepared");
        let blocks = self.blocks.as_ref().expect("scan pipelines prepared");
        let tail = self.tail.as_ref().expect("scan pipelines prepared");
        let add = self.add.as_ref().expect("scan pipelines prepared");
        let parents = self.buffer.as_ref().expect("scan storage prepared");
        let (levels, count) = levels(n);
        let params = |level: usize| {
            let (offset, length) = levels[level];
            let own = if level == 0 { 0 } else { offset - level0_offset };
            let parent = (level + 1 < count).then(|| levels[level + 1].0 - level0_offset);
            ScanParams {
                n: length as u32,
                src_offset: own as u32,
                dst_offset: own as u32,
                parent: parent.unwrap_or(0) as u32,
                has_parent: u32::from(parent.is_some()),
                _pad: [0; 3],
            }
        };
        let dispatch = |encoder: &mut manifold_gpu::GpuEncoder, pipeline, level: usize, groups: u32, label| {
            let uniforms = params(level);
            let (src, dst) = if level == 0 { (src, dst) } else { (parents, parents) };
            encoder.dispatch_compute(
                pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                    GpuBinding::Buffer { binding: 1, buffer: src, offset: 0 },
                    GpuBinding::Buffer { binding: 2, buffer: dst, offset: 0 },
                    GpuBinding::Buffer { binding: 3, buffer: parents, offset: 0 },
                    GpuBinding::Buffer { binding: 4, buffer: gate, offset: 0 },
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
        assert_eq!(parent_words((1 << 20) + 3), 4097);
        assert_eq!(parent_words(256), 1);
        assert_eq!(parent_words(0), 1);
        let (l, count) = levels(BLOCK * TAIL + 1);
        assert_eq!(count, 3);
        assert_eq!(l[1], (BLOCK * TAIL + 1, TAIL + 1));
        assert_eq!(l[2], (BLOCK * TAIL + 1 + TAIL + 1, 65));
        assert_eq!(parent_words(BLOCK * TAIL + 1), TAIL + 1 + 65);
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

    /// Both encode paths equal the CPU inclusive scan word for word, at every
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
        let read = |buffer: &GpuBuffer, n: usize| -> Vec<u32> {
            let ptr = buffer.mapped_ptr().expect("shared buffer");
            // SAFETY: shared buffer holding at least n words; GPU work done.
            bytemuck::cast_slice(unsafe { std::slice::from_raw_parts(ptr, n * 4) }).to_vec()
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
            let check = |got: &[u32], path: &str| {
                let first_bad = got.iter().zip(&want).position(|(g, w)| g != w);
                assert_eq!(first_bad, None, "{path}: n = {n} ({} dispatches): word {first_bad:?} differs", dispatches(n));
            };

            let buffer = scan.buffer(&device, n).expect("scan storage");
            // SAFETY: a shared buffer of at least n words; no GPU work in flight.
            unsafe { buffer.write(0, bytemuck::cast_slice(&values)) };
            let mut encoder = device.create_encoder("prefix scan proof (in place)");
            scan.encode_labelled(&mut encoder, n, ScanLabels::DEFAULT);
            encoder.commit_and_wait_completed();
            check(&read(scan.buffer.as_ref().expect("scan storage"), n), "encode_labelled");

            let bytes = (n * 4) as u64;
            let src = device.try_create_buffer_shared(bytes).expect("src");
            let dst = device.try_create_buffer_shared(bytes).expect("dst");
            // SAFETY: a shared buffer of n words; no GPU work in flight.
            unsafe { src.write(0, bytemuck::cast_slice(&values)) };
            scan.parents(&device, n).expect("scan parents");
            let mut encoder = device.create_encoder("prefix scan proof (into)");
            scan.encode_into(&mut encoder, n, &src, &dst);
            encoder.commit_and_wait_completed();
            check(&read(&dst, n), "encode_into");
            assert_eq!(read(&src, n), values, "encode_into: n = {n}: src was written");
        }
    }
}
