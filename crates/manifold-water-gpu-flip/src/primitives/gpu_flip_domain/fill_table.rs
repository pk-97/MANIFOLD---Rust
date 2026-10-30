//! The domain's provided `fill_boxes` array: one row per Water's Initial
//! Fill, the table `node.liquid_fill` seeds the boxes from, after the pool.
//! Waters that share one solver each bring their own fill.

use manifold_gpu::BufferUse;
use manifold_gpu::GpuBuffer;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_water_liquid::MAX_WATERS;
use manifold_water_liquid::body_buffers::LiquidBodyBuffers;

/// The rows a fill reads: one per Water a liquid holds, by Water.
pub const FILL_ROWS: usize = MAX_WATERS as usize;

/// One Water's Initial Fill in the grid box's half-cell sites: x0 x1 y0 y1
/// z0 z1 (empty without one), the Water, and its shaped fill's region row
/// plus one (0 for a box).
pub type FillRow = [u32; 8];

/// The rows, their buffer and the version a fill keys its refill on.
#[derive(Default)]
pub struct FillTable {
    rows: [FillRow; FILL_ROWS],
    version: u32,
    epoch: Option<u32>,
    dirty: bool,
    buffer: Option<GpuBuffer>,
}

impl FillTable {
    /// This frame's rows in epoch `epoch`: a change of rows, or a new epoch
    /// with a shaped fill (whose region is posed at the epoch's start), moves
    /// the version on, so the fill seeds again.
    pub fn set(&mut self, rows: [FillRow; FILL_ROWS], epoch: u32) {
        let shaped = rows.iter().any(|row| row[7] != 0);
        if rows != self.rows || (shaped && self.epoch != Some(epoch)) {
            self.rows = rows;
            // A version rides an f32 wire: it stays below 2^24.
            self.version = (self.version + 1) % (1 << 24);
            self.dirty = true;
        }
        self.epoch = Some(epoch);
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn buffer(&self) -> Option<&GpuBuffer> {
        self.buffer.as_ref()
    }

    /// Writes changed rows in encoder order through `uploader`, so the fill
    /// that follows the domain reads this frame's.
    pub fn upload(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        uploader: &mut LiquidBodyBuffers,
        label: &'static str,
    ) {
        if self.buffer.is_none() {
            let bytes = std::mem::size_of_val(&self.rows) as u64;
            self.buffer = Some(gpu.device.create_buffer(bytes, BufferUse::Gpu));
            self.dirty = true;
        }
        if self.dirty
            && let Some(buffer) = &self.buffer
        {
            uploader.write_rows(gpu, buffer, bytemuck::cast_slice(&self.rows), label);
            self.dirty = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The version moves on when a row changes and, with a shaped fill, on
    /// each new epoch; a box fill keeps it across epochs, so a reset reuses
    /// the fill.
    #[test]
    fn the_version_moves_when_the_fill_would_seed_differently() {
        let mut table = FillTable::default();
        let mut rows = [[0; 8]; FILL_ROWS];
        rows[2] = [4, 10, 0, 6, 4, 10, 2, 0];
        table.set(rows, 0);
        let boxed = table.version();
        table.set(rows, 1);
        assert_eq!(table.version(), boxed, "a box fill holds across epochs");
        rows[2][1] = 12;
        table.set(rows, 1);
        assert_eq!(table.version(), boxed + 1);
        rows[2][7] = 3;
        table.set(rows, 1);
        let shaped = table.version();
        table.set(rows, 1);
        assert_eq!(table.version(), shaped);
        table.set(rows, 2);
        assert_eq!(table.version(), shaped + 1, "a shaped fill reposes each epoch");
    }
}
