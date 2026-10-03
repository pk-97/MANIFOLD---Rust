//! Internal whitewater stage pass: copy a cell-centred liquid distance lattice
//! into the larger whitewater lattice and fill its exterior.
//!
//! This is deliberately a stage helper rather than a catalog primitive. The
//! pass is part of `node.whitewater_step`'s FLIP method and has no independent
//! graph consumer.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};

const PAD_DISTANCE_LATTICE_SHADER: &str = include_str!("shaders/pad_distance_lattice.wgsl");

/// The hand-kernel's uniform layout, proven against its shader by the custom
/// ABI cases (`dispatch_count` names only generated layouts). Cells are source
/// dimensions in x-fastest order; the output side is `cells + 2 * padding` on
/// every axis.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PadUniforms {
    cells_x: u32,
    cells_y: u32,
    cells_z: u32,
    padding: u32,
    exterior: f32,
    padded_cells: u32,
    _pad0: u32,
    _pad1: u32,
}

/// Return the padded side lengths, refusing arithmetic that cannot be
/// represented by the dispatch contract.
pub(crate) fn padded_extent(cells: [u32; 3], padding: u32) -> Option<[u32; 3]> {
    let twice_padding = padding.checked_mul(2)?;
    Some([
        cells[0].checked_add(twice_padding)?,
        cells[1].checked_add(twice_padding)?,
        cells[2].checked_add(twice_padding)?,
    ])
}

fn cell_count(extent: [u32; 3]) -> Option<u32> {
    extent.into_iter().try_fold(1u32, u32::checked_mul)
}

/// Prepare the internal padding pipeline at stage install time.
pub(crate) fn prepare_pad_distance_lattice(
    slot: &mut Option<GpuComputePipeline>,
    device: &GpuDevice,
) {
    slot.get_or_insert_with(|| {
        device.create_compute_pipeline(
            PAD_DISTANCE_LATTICE_SHADER,
            "pad_distance_lattice",
            "node.whitewater_step.particle_distance",
        )
    });
}

/// Encode one padding pass. `cells` are the unpadded source dimensions;
/// `source` and `output` are x-fastest f32 arrays.
pub(crate) fn encode_pad_distance_lattice(
    encoder: &mut manifold_gpu::GpuEncoder,
    pipeline: &GpuComputePipeline,
    source: &GpuBuffer,
    output: &GpuBuffer,
    cells: [u32; 3],
    padding: u32,
    exterior: f32,
) {
    let side = padded_extent(cells, padding).expect("validated whitewater lattice");
    let padded_cells = cell_count(side).expect("validated whitewater count");
    let source_count = cell_count(cells).expect("validated solver count");
    assert!(source.size >= u64::from(source_count) * 4);
    assert!(output.size >= u64::from(padded_cells) * 4);
    let uniforms = PadUniforms {
        cells_x: cells[0],
        cells_y: cells[1],
        cells_z: cells[2],
        padding,
        exterior,
        padded_cells,
        _pad0: 0,
        _pad1: 0,
    };
    encoder.dispatch_compute(
        pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: source,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: output,
                offset: 0,
            },
        ],
        [padded_cells.div_ceil(256), 1, 1],
        "node.whitewater_step.particle_distance",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn cpu_pad(
        source: &[f32],
        cells: [u32; 3],
        padding: u32,
        exterior: f32,
    ) -> Vec<f32> {
        let side = padded_extent(cells, padding).expect("test extent");
        let total = cell_count(side).expect("test count") as usize;
        (0..total)
            .map(|index| {
                let sx = side[0] as usize;
                let sy = side[1] as usize;
                let coord = [index % sx, (index / sx) % sy, index / (sx * sy)];
                let source_coord = coord.map(|axis| axis as i32 - padding as i32);
                if source_coord
                    .iter()
                    .enumerate()
                    .any(|(axis, &value)| value < 0 || value >= cells[axis] as i32)
                {
                    exterior
                } else {
                    let source_index = source_coord[0] as usize
                        + cells[0] as usize
                            * (source_coord[1] as usize
                                + cells[1] as usize * source_coord[2] as usize);
                    source[source_index]
                }
            })
            .collect()
    }

    #[test]
    fn whitewater_per_tick_padding_shader_validates_on_cpu() {
        let module =
            naga::front::wgsl::parse_str(PAD_DISTANCE_LATTICE_SHADER).expect("padding WGSL parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("padding WGSL validates");
        assert_eq!(module.entry_points[0].name, "pad_distance_lattice");
    }

    #[test]
    fn padded_extent_and_coordinates_cover_an_eight_cube() {
        let cells = [8; 3];
        let padding = 2;
        let side = padded_extent(cells, padding).expect("8³ padded extent");
        assert_eq!(side, [12; 3]);
        assert_eq!(cell_count(side), Some(12 * 12 * 12));

        let source: Vec<f32> = (0..512).map(|index| index as f32 + 0.5).collect();
        let output = cpu_pad(&source, cells, padding, -99.0);
        for z in 0..side[2] as usize {
            for y in 0..side[1] as usize {
                for x in 0..side[0] as usize {
                    let output_index = x + side[0] as usize * (y + side[1] as usize * z);
                    let sx = x as i32 - padding as i32;
                    let sy = y as i32 - padding as i32;
                    let sz = z as i32 - padding as i32;
                    let expected =
                        if (0..8).contains(&sx) && (0..8).contains(&sy) && (0..8).contains(&sz) {
                            source[sx as usize + 8 * (sy as usize + 8 * sz as usize)]
                        } else {
                            -99.0
                        };
                    assert_eq!(output[output_index].to_bits(), expected.to_bits());
                }
            }
        }
    }

    #[test]
    fn padded_extent_rejects_overflow() {
        assert_eq!(padded_extent([u32::MAX; 3], 1), None);
        assert_eq!(padded_extent([1; 3], u32::MAX), None);
        assert_eq!(cell_count([u32::MAX; 3]), None);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;

    #[test]
    fn whitewater_per_tick_padding_matches_cpu_on_a_small_lattice() {
        let cells = [3, 2, 4];
        let padding = 1;
        let exterior = 17.25;
        let source: Vec<f32> = (0..24).map(|index| index as f32 * 0.5 - 3.0).collect();
        let expected = super::tests::cpu_pad(&source, cells, padding, exterior);
        let count = expected.len() as u32;
        let device = crate::test_device();
        let input = device.create_buffer_shared(source.len() as u64 * 4);
        unsafe { input.write(0, bytemuck::cast_slice(&source)) };
        let output = device.create_buffer_shared(count as u64 * 4);
        let mut pipeline = None;
        prepare_pad_distance_lattice(&mut pipeline, &device);
        let mut encoder = device.create_encoder("pad-distance-lattice-proof");
        encode_pad_distance_lattice(
            &mut encoder,
            pipeline.as_ref().unwrap(),
            &input,
            &output,
            cells,
            padding,
            exterior,
        );
        encoder.commit_and_wait_completed();
        let ptr = output.mapped_ptr().expect("shared output");
        let actual = unsafe { std::slice::from_raw_parts(ptr as *const f32, count as usize) };
        for (index, (&got, &want)) in actual.iter().zip(expected.iter()).enumerate() {
            assert_eq!(got.to_bits(), want.to_bits(), "destination {index}");
        }
    }
}
