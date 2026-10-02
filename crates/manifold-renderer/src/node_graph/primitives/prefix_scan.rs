//! The multi-level inclusive prefix sum shared by `node.sort_particles_into_cells`
//! (bin starts) and `node.running_total`. Level 0 is the values to scan; each
//! later level holds the 256-wide block totals of the one before. Level 0
//! lives either in this scan's own storage ([`PrefixScan::encode`]) or in the
//! caller's input and output buffers ([`PrefixScan::encode_into`], no copies
//! either side); the later levels always live in the scan's storage. Not a
//! primitive — a scan is barriered and multi-dispatch, so its atoms are fusion
//! boundaries (ADDING_PRIMITIVES.md, exclusion 1).

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

const SHADER: &str = include_str!("shaders/prefix_scan.wgsl");
const BLOCK: usize = 256;
/// 256⁴ values; every array in the graph is far below this.
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
/// from offset 0. The last level is one block.
fn levels(n: usize) -> ([(usize, usize); MAX_LEVELS], usize) {
    let mut levels = [(0, 0); MAX_LEVELS];
    let mut count = 0;
    let (mut offset, mut length) = (0, n.max(1));
    loop {
        levels[count] = (offset, length);
        count += 1;
        if length <= BLOCK || count == MAX_LEVELS {
            break;
        }
        offset += length;
        length = length.div_ceil(BLOCK);
    }
    (levels, count)
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
    add: Option<GpuComputePipeline>,
    buffer: Option<GpuBuffer>,
    words: usize,
}

impl PrefixScan {
    /// Create the pipelines. Call before any early return (compile contract).
    pub(crate) fn prepare(&mut self, device: &GpuDevice) {
        if self.blocks.is_none() {
            self.blocks = Some(device.create_compute_pipeline(SHADER, "scan_blocks", "prefix_scan.blocks"));
        }
        if self.add.is_none() {
            self.add = Some(device.create_compute_pipeline(SHADER, "add_block_totals", "prefix_scan.add"));
        }
    }

    /// The storage buffer sized for every level of `n` values, level 0 at
    /// offset 0. Pairs with [`Self::encode`].
    pub(crate) fn buffer(&mut self, device: &GpuDevice, n: usize) -> Result<&GpuBuffer, String> {
        self.storage(device, storage_words(n))
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

    /// Scan level 0 `[0, n)` of the storage in place. `prepare` and `buffer` first.
    pub(crate) fn encode(&self, encoder: &mut manifold_gpu::GpuEncoder, n: usize) {
        let buffer = self.buffer.as_ref().expect("scan storage prepared");
        self.encode_levels(encoder, n, buffer, buffer, 0);
    }

    /// Scan `src[0, n)` into `dst[0, n)`, the later levels in the storage.
    /// `prepare` and `parents` first. `src` and `dst` are distinct buffers.
    pub(crate) fn encode_into(
        &self,
        encoder: &mut manifold_gpu::GpuEncoder,
        n: usize,
        src: &GpuBuffer,
        dst: &GpuBuffer,
    ) {
        self.encode_levels(encoder, n, src, dst, n.max(1));
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
    ) {
        let blocks = self.blocks.as_ref().expect("scan pipelines prepared");
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
        let dispatch = |encoder: &mut manifold_gpu::GpuEncoder, pipeline, level: usize, label| {
            let uniforms = params(level);
            let (src, dst) = if level == 0 { (src, dst) } else { (parents, parents) };
            encoder.dispatch_compute(
                pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                    GpuBinding::Buffer { binding: 1, buffer: src, offset: 0 },
                    GpuBinding::Buffer { binding: 2, buffer: dst, offset: 0 },
                    GpuBinding::Buffer { binding: 3, buffer: parents, offset: 0 },
                ],
                [(uniforms.n as usize).div_ceil(BLOCK) as u32, 1, 1],
                label,
            );
            encoder.compute_memory_barrier_buffers();
        };
        for level in 0..count {
            dispatch(encoder, blocks, level, "prefix_scan.blocks");
        }
        for level in (0..count.saturating_sub(1)).rev() {
            dispatch(encoder, add, level, "prefix_scan.add");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_scan_levels_cover_every_block() {
        assert_eq!(levels(1).1, 1);
        assert_eq!(levels(256).1, 1);
        let (l, count) = levels(257);
        assert_eq!(count, 2);
        assert_eq!(l[1], (257, 2));
        let (l, count) = levels((1 << 20) + 3);
        assert_eq!(count, 3);
        assert_eq!(l[1], ((1 << 20) + 3, 4097));
        assert_eq!(l[2], ((1 << 20) + 3 + 4097, 17));
        assert_eq!(storage_words((1 << 20) + 3), (1 << 20) + 3 + 4097 + 17);
        assert_eq!(parent_words((1 << 20) + 3), 4097 + 17);
        assert_eq!(parent_words(256), 1);
        assert_eq!(parent_words(0), 1);
    }
}
