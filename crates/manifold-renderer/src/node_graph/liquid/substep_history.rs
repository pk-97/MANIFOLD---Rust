//! Accepted liquid substeps on the solver-neutral MAC-face seam.
//! Four f32 words per schedule row: duration, elapsed endpoint, bitcast
//! timestamped impulse index/valid bit, reserved zero. Zero duration is inactive.
//! Face arrays concatenate ordinary seam grids in schedule order.
use super::grid::face_len;
use crate::node_graph::primitives::face_sample_component::FaceSampleComponent;
use crate::node_graph::primitives::standalone_pipeline::standalone_pipeline;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

pub(crate) fn history_bytes(cells: [u32; 3], slots: u32) -> u64 {
    u64::from(slots) * (16 + (0..3).map(|a| face_len(cells, a) * 4).sum::<u64>())
}

#[derive(Default)]
pub(crate) struct SubstepHistory {
    pub schedule: Option<GpuBuffer>,
    pub faces: Option<[GpuBuffer; 3]>,
    component: Option<GpuComputePipeline>,
    shape: Option<([u32; 3], u32)>,
}
impl SubstepHistory {
    pub fn prepare(&mut self, device: &GpuDevice) {
        standalone_pipeline::<FaceSampleComponent>(&mut self.component, device);
    }
    pub fn reserve(
        &mut self,
        device: &GpuDevice,
        cells: [u32; 3],
        slots: u32,
    ) -> Result<(), String> {
        if self
            .shape
            .is_some_and(|(old, capacity)| old == cells && slots <= capacity)
        {
            return Ok(());
        }
        crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            history_bytes(cells, slots),
        )
        .map_err(|e| e.to_string())?;
        let schedule = device.try_create_buffer_shared(u64::from(slots) * 16)?;
        schedule.zero_fill();
        let alloc = |a| device.try_create_buffer(u64::from(slots) * face_len(cells, a) * 4);
        self.faces = Some([alloc(0)?, alloc(1)?, alloc(2)?]);
        self.schedule = Some(schedule);
        self.shape = Some((cells, slots));
        Ok(())
    }
    /// GPU FLIP adapts its scheduler and native face records here. Consumers
    /// never see either solver-private layout.
    pub fn capture(&self, enc: &mut GpuEncoder, index: u32, plan: &GpuBuffer, faces: &GpuBuffer) {
        let (cells, slots) = self.shape.expect("history reserved");
        assert!(index < slots);
        let schedule = self.schedule.as_ref().expect("schedule reserved");
        enc.copy_buffer_range(plan, 0, schedule, u64::from(index) * 16, 8);
        enc.copy_buffer_range(plan, 28, schedule, u64::from(index) * 16 + 8, 4);
        for (axis, out) in self
            .faces
            .as_ref()
            .expect("faces reserved")
            .iter()
            .enumerate()
        {
            let count = face_len(cells, axis) as u32;
            let words = [
                axis as u32,
                ((cells[0] + 7) as f32).to_bits(),
                ((cells[1] + 7) as f32).to_bits(),
                ((cells[2] + 7) as f32).to_bits(),
                count,
                0,
                0,
                0,
            ];
            enc.dispatch_compute(
                self.component.as_ref().expect("component prepared"),
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::cast_slice(&words),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: faces,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: out,
                        offset: u64::from(index) * u64::from(count) * 4,
                    },
                ],
                [count.div_ceil(256), 1, 1],
                "liquid.substep_history.faces",
            );
        }
        enc.compute_memory_barrier_buffers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn liquid_substep_history_extent_covers_all_schedule_and_face_dispatches() {
        for cells in [[8, 8, 8], [16, 12, 8], [64, 64, 64]] {
            for slots in [6, 7, 262] {
                let mut bytes = u64::from(slots) * 16;
                for axis in 0..3 {
                    let count = face_len(cells, axis);
                    let length = u64::from(slots) * count;
                    let mut dims = cells;
                    dims[axis] += 1;
                    let native = cells.map(|n| u64::from(n + 1));
                    for i in 0..count {
                        let c = [
                            i % u64::from(dims[0]),
                            i / u64::from(dims[0]) % u64::from(dims[1]),
                            i / (u64::from(dims[0]) * u64::from(dims[1])),
                        ];
                        assert!(
                            c[0] + native[0] * (c[1] + native[1] * c[2])
                                < native.into_iter().product::<u64>()
                        );
                    }
                    for step in 0..slots {
                        // Codegen guards the rounded workgroup tail at count.
                        assert!(u64::from(step) * count + count <= length);
                        assert!(u64::from(step) * 16 + 12 <= u64::from(slots) * 16);
                    }
                    bytes += length * 4;
                }
                assert_eq!(history_bytes(cells, slots), bytes);
            }
        }
        assert_eq!(history_bytes([64; 3], 6), 19_169_376);
    }
}
