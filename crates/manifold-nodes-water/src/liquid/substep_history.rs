//! Accepted liquid substeps on the solver-neutral MAC-face seam.
//! Four f32 words per schedule row: duration, elapsed endpoint, bitcast
//! timestamped impulse index/valid bit, reserved zero. Zero duration is inactive.
//! Face arrays concatenate ordinary seam grids in schedule order. Inactive
//! rows do not publish faces; consumers must skip them before reading a grid.
use super::grid::face_len;
use crate::primitives::face_sample_component::FaceSampleComponent;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

const ARGUMENT_BYTES: u64 = 3 * 3 * 4;
// Adapt the solver plan once, then let the existing component primitive
// gather only active grids. The GPU owns activity; no readback is needed.
const CAPTURE_SHADER: &str = r#"
@group(0) @binding(0) var<uniform> capture: vec4<u32>;
@group(0) @binding(1) var<storage, read> plan: array<u32>;
@group(0) @binding(2) var<storage, read_write> schedule: array<u32>;
@group(0) @binding(3) var<storage, read_write> arguments: array<u32>;

@compute @workgroup_size(1)
fn capture_schedule() {
    let row = capture.x * 4u;
    schedule[row] = plan[0];
    schedule[row + 1u] = plan[1];
    schedule[row + 2u] = plan[7];
    schedule[row + 3u] = 0u;
    let enabled = bitcast<f32>(plan[0]) > 0.0;
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        arguments[axis * 3u] = select(0u, capture[axis + 1u], enabled);
        arguments[axis * 3u + 1u] = 1u;
        arguments[axis * 3u + 2u] = 1u;
    }
}
"#;

pub(crate) fn history_bytes(cells: [u32; 3], slots: u32) -> u64 {
    ARGUMENT_BYTES + u64::from(slots) * (16 + (0..3).map(|a| face_len(cells, a) * 4).sum::<u64>())
}

#[derive(Default)]
pub(crate) struct SubstepHistory {
    pub schedule: Option<GpuBuffer>,
    pub faces: Option<[GpuBuffer; 3]>,
    component: Option<GpuComputePipeline>,
    capture_schedule: Option<GpuComputePipeline>,
    arguments: Option<GpuBuffer>,
    clear_faces: bool,
    shape: Option<([u32; 3], u32)>,
}
impl SubstepHistory {
    pub fn prepare(&mut self, device: &GpuDevice) {
        standalone_pipeline::<FaceSampleComponent>(&mut self.component, device);
        self.capture_schedule.get_or_insert_with(|| {
            device.create_compute_pipeline(CAPTURE_SHADER, "capture_schedule", "liquid.substep_history.schedule")
        });
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
        manifold_node_engine::load::expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            history_bytes(cells, slots),
        )
        .map_err(|e| e.to_string())?;
        let schedule = device.try_create_buffer_shared(u64::from(slots) * 16)?;
        schedule.zero_fill();
        let alloc = |a| device.try_create_buffer(u64::from(slots) * face_len(cells, a) * 4);
        let faces = [alloc(0)?, alloc(1)?, alloc(2)?];
        let arguments = device.try_create_buffer(ARGUMENT_BYTES)?;
        self.faces = Some(faces);
        self.schedule = Some(schedule);
        self.arguments = Some(arguments);
        self.clear_faces = true;
        self.shape = Some((cells, slots));
        Ok(())
    }
    /// GPU FLIP adapts its scheduler and native face records here. Consumers
    /// never see either solver-private layout.
    pub fn capture(&mut self, enc: &mut GpuEncoder, index: u32, plan: &GpuBuffer, faces: &GpuBuffer) {
        let (cells, slots) = self.shape.expect("history reserved");
        assert!(index < slots);
        let schedule = self.schedule.as_ref().expect("schedule reserved");
        let arguments = self.arguments.as_ref().expect("arguments reserved");
        if self.clear_faces {
            for out in self.faces.as_ref().expect("faces reserved") {
                enc.clear_buffer(out);
            }
            self.clear_faces = false;
        }
        let dispatch = [
            index,
            (face_len(cells, 0) as u32).div_ceil(256),
            (face_len(cells, 1) as u32).div_ceil(256),
            (face_len(cells, 2) as u32).div_ceil(256),
        ];
        enc.dispatch_compute(
            self.capture_schedule.as_ref().expect("capture prepared"),
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::cast_slice(&dispatch) },
                GpuBinding::Buffer { binding: 1, buffer: plan, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: schedule, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: arguments, offset: 0 },
            ],
            [1, 1, 1],
            "liquid.substep_history.schedule",
        );
        enc.compute_memory_barrier_buffers();
        for (axis, out) in self
            .faces
            .as_ref()
            .expect("faces reserved")
            .iter()
            .enumerate()
        {
            let count = face_len(cells, axis) as u32;
            // The component reads nodes − 4 cells: GPU FLIP's solver grid.
            let words = [
                axis as u32,
                ((cells[0] + 4) as f32).to_bits(),
                ((cells[1] + 4) as f32).to_bits(),
                ((cells[2] + 4) as f32).to_bits(),
                count,
                0,
                0,
                0,
            ];
            enc.dispatch_compute_indirect(
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
                arguments,
                axis as u64 * 12,
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
    fn liquid_substep_history_capture_shader_validates() {
        let module = naga::front::wgsl::parse_str(CAPTURE_SHADER).expect("history capture WGSL parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("history capture WGSL validates");
    }

    #[test]
    fn liquid_substep_history_extent_covers_all_schedule_and_face_dispatches() {
        for cells in [[8, 8, 8], [16, 12, 8], [64, 64, 64]] {
            for slots in [6, 7, 262] {
                let mut bytes = ARGUMENT_BYTES + u64::from(slots) * 16;
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
        assert_eq!(history_bytes([64; 3], 6), 19_169_412);
    }
}
