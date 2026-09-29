//! The multi-level inclusive prefix sum shared by `node.sort_particles_into_cells`
//! (bin starts) and `node.running_total`. One storage buffer holds every level:
//! level 0 at offset 0 (the values to scan), each later level the 256-wide
//! block totals of the one before. Not a primitive — a scan is barriered and
//! multi-dispatch, so its atoms are fusion boundaries (ADDING_PRIMITIVES.md,
//! exclusion 1).

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

const SHADER: &str = include_str!("shaders/prefix_scan.wgsl");
const BLOCK: usize = 256;
/// 256⁴ values; every array in the graph is far below this.
const MAX_LEVELS: usize = 4;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScanParams {
    n: u32,
    offset: u32,
    parent: u32,
    has_parent: u32,
}

/// (offset, length) of each level for `n` values. The last level is one block.
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

/// Words of storage the scan needs for `n` values.
pub(crate) fn storage_words(n: usize) -> usize {
    let (levels, count) = levels(n);
    let (offset, length) = levels[count - 1];
    offset + length
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
        let blocks = self.blocks.as_ref().expect("scan pipelines prepared");
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
        let dispatch = |encoder: &mut manifold_gpu::GpuEncoder, pipeline, level: usize, label| {
            let uniforms = params(level);
            encoder.dispatch_compute(
                pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                    GpuBinding::Buffer { binding: 1, buffer, offset: 0 },
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
    }
}
