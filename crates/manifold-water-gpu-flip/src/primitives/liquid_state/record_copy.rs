//! The tick's live particle records, copied from the region's result into the
//! liquid state: one thread per slot in use, dispatched from the identity's
//! record. A whole-buffer blit would move the reserved pool, dead slots
//! included.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};
use manifold_water_liquid::primitives::particle_identity::COUNT_OFFSET;

const SHADER: &str = include_str!("../shaders/liquid_state_records.wgsl");

pub struct RecordCopy {
    pipeline: GpuComputePipeline,
}

impl RecordCopy {
    pub fn new(device: &GpuDevice) -> Self {
        Self {
            pipeline: device.create_compute_pipeline(
                SHADER,
                "copy_records",
                "node.liquid_state.copy_records",
            ),
        }
    }

    /// Copies slots `[0, n)` of `source` into `state`, `n` being word 7 of
    /// `identity`, whose words 4-6 are the dispatch.
    pub fn encode(
        &self,
        enc: &mut GpuEncoder,
        identity: &GpuBuffer,
        source: &GpuBuffer,
        state: &GpuBuffer,
    ) {
        let bindings = [buffer(0, identity), buffer(1, source), buffer(2, state)];
        enc.dispatch_compute_indirect(
            &self.pipeline,
            &bindings,
            identity,
            COUNT_OFFSET,
            "node.liquid_state.copy_records",
        );
        enc.compute_memory_barrier_buffers();
    }
}

fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;

    const SLOTS: usize = 1024;
    const SENTINEL: u32 = 0xdead_beef;

    fn shared(device: &GpuDevice, words: &[u32]) -> GpuBuffer {
        let buffer = device.create_buffer_shared((words.len() * 4) as u64);
        // SAFETY: a shared buffer of exactly these words; no GPU work in flight.
        unsafe { buffer.write(0, bytemuck::cast_slice(words)) };
        buffer
    }

    fn read(buffer: &GpuBuffer) -> Vec<u32> {
        let ptr = buffer.mapped_ptr().expect("a shared buffer");
        // SAFETY: a shared buffer of `size` bytes; the GPU work that wrote it has completed.
        bytemuck::cast_slice(unsafe { std::slice::from_raw_parts(ptr, buffer.size as usize) })
            .to_vec()
    }

    /// Eight words a record, each word naming its slot and lane.
    fn records(tag: u32) -> Vec<u32> {
        (0..SLOTS as u32 * 8).map(|word| tag + word).collect()
    }

    /// The identity words a record of `n` slots holds: next id, epoch,
    /// reserved base, reset request, then `[groups, 1, 1, n]`.
    fn identity(n: u32) -> [u32; 8] {
        [11, 4, 6, 0, n.div_ceil(256).max(1), 1, 1, n]
    }

    /// Slots in use copy; the state keeps its words from `n` on, whether `n`
    /// is empty, one group's edge or in the middle of a group.
    #[test]
    fn the_copy_moves_the_slots_in_use_and_leaves_the_rest() {
        let device = manifold_gpu::testkit::test_device();
        let copy = RecordCopy::new(&device);
        let source = records(0x1000_0000);
        let before = records(0x2000_0000);
        for n in [0u32, 1, 255, 256, 257, 300, SLOTS as u32] {
            let identity = shared(&device, &identity(n));
            let source_buffer = shared(&device, &source);
            let state = shared(&device, &before);
            let mut enc = device.create_encoder("liquid state record copy proof");
            copy.encode(&mut enc, &identity, &source_buffer, &state);
            enc.commit_and_wait_completed();
            let words = read(&state);
            let used = n as usize * 8;
            assert_eq!(words[..used], source[..used], "n {n}: the records copy");
            assert_eq!(
                words[used..],
                before[used..],
                "n {n}: the rest is untouched"
            );
        }
    }

    /// A count past the buffers copies what both hold and no more.
    #[test]
    fn a_count_past_the_buffers_is_clamped_to_them() {
        let device = manifold_gpu::testkit::test_device();
        let copy = RecordCopy::new(&device);
        let source = records(0x1000_0000);
        let identity = shared(&device, &identity(SLOTS as u32 * 4));
        let state = shared(&device, &vec![SENTINEL; 300 * 8]);
        let source_buffer = shared(&device, &source);
        let mut enc = device.create_encoder("liquid state record copy clamp proof");
        copy.encode(&mut enc, &identity, &source_buffer, &state);
        enc.commit_and_wait_completed();
        assert_eq!(read(&state), source[..300 * 8]);
    }
}
