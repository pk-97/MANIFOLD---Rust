//! The face grid a liquid domain publishes (`docs/LIQUID_SOLVER_SEAM_DESIGN.md`
//! section 3.2 (Grid outputs)): MAC faces in the FLIP engine's layout over
//! the domain's cells, one f32 array per axis in m/s, scene space. Every
//! solver resamples its own lattice into this layout; no consumer sees a
//! native one.
//!
//! The cell-centred narrow-band distance and its disabled positive sentinel
//! follow the reseeding seam described by Ferstl et al. (2016).

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};
use crate::fluid_particles::FaceSample;

use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use crate::primitives::liquid_stats::{with_stats_layout, LIQUID_STATS_WORDS, NARROW_BAND_SHORTAGE_WORD};

const PUBLISH_SHADER_SOURCE: &str = include_str!("primitives/shaders/liquid_frame_faces.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FaceParams {
    len: u32,
    _pad0: u32,
    _pad1: u32,
    has_shortage: u32,
}

/// The frame ports that carry the grid: one array per axis, the cells per
/// axis, and how many face layers past the liquid carry velocity.
pub const FACE_GRID_PORTS: [&str; 7] =
    ["face_u", "face_v", "face_w", "face_cells_x", "face_cells_y", "face_cells_z", "face_valid_layers"];

/// FLIP's MAC trilinear on the face arrays, for bodies that sample velocity
/// at a point.
pub const LIQUID_FACES: &str = include_str!("primitives/shaders/liquid_faces.wgsl");

/// A frame node's inputs for the grid it publishes, one array per axis
/// (`FACE_GRID_PORTS[0..3]`).
pub const FACE_INPUT_PORTS: [&str; 3] = ["face_u_in", "face_v_in", "face_w_in"];

/// Faces per axis of `axis`'s array: one more than the cells along `axis`,
/// the cells on the other two.
pub fn face_dims(cells: [u32; 3], axis: usize) -> [u32; 3] {
    let mut dims = cells;
    dims[axis] += 1;
    dims
}

/// Records in `axis`'s array, in u64 so no size wraps.
pub fn face_len(cells: [u32; 3], axis: usize) -> u64 {
    face_dims(cells, axis).iter().map(|&n| u64::from(n)).product()
}

manifold_core::testkit_visible! {
/// Bytes of a staggered face grid at `cells`: one record per padded cell.
pub fn face_bytes(cells: [u32; 3]) -> u64 {
    cells.iter().map(|&n| u64::from(n) + 1).product::<u64>() * size_of::<FaceSample>() as u64
}
}

/// Number of cell-centred interior distance records. The field has one f32
/// per physical cell; its storage extent is exact, with no metadata record.
pub fn interior_len(cells: [u32; 3]) -> u64 {
    cells.iter().map(|&n| u64::from(n)).product()
}

/// Bytes of a cell-centred interior distance field.
pub fn interior_bytes(cells: [u32; 3]) -> u64 {
    interior_len(cells) * 4
}

fn stats_has_shortage(stats_bytes: u64) -> bool {
    stats_bytes >= u64::from(LIQUID_STATS_WORDS) * 4
}

/// Index of face `f` in `axis`'s array, x fastest.
pub fn face_index(cells: [u32; 3], axis: usize, f: [u32; 3]) -> usize {
    let d = face_dims(cells, axis).map(|n| n as usize);
    let f = f.map(|n| n as usize);
    f[0] + d[0] * (f[1] + d[1] * f[2])
}

/// Face `f` of `axis`'s array from its index.
pub fn face_coords(cells: [u32; 3], axis: usize, index: usize) -> [u32; 3] {
    let d = face_dims(cells, axis).map(|n| n as usize);
    [index % d[0], (index / d[0]) % d[1], index / (d[0] * d[1])].map(|n| n as u32)
}

/// Where face `f` of `axis` sits, in scene metres: on the cell boundary
/// along `axis`, at the cell centre on the other two.
pub fn face_position(min: [f32; 3], cell_size: f32, axis: usize, f: [u32; 3]) -> [f32; 3] {
    std::array::from_fn(|b| {
        let half = if b == axis { 0.0 } else { 0.5 };
        min[b] + (f[b] as f32 + half) * cell_size
    })
}

/// Installed copy and sentinel-clear kernels shared by the face and interior
/// publication paths.
#[derive(Default)]
pub struct InteriorOps {
    copy_pipeline: Option<GpuComputePipeline>,
    clear_pipeline: Option<GpuComputePipeline>,
}

impl InteriorOps {
    /// Install both kernels with the node, following Ferstl et al. (2016)'s
    /// narrow-band storage rule: the disabled field is a positive sentinel.
    pub fn prepare(&mut self, device: &manifold_gpu::GpuDevice) {
        let shader = with_stats_layout(PUBLISH_SHADER_SOURCE);
        self.prepare_copy(device, &shader);
        if self.clear_pipeline.is_none() {
            self.prepare_clear_with_shader(device, &shader);
        }
    }

    pub fn prepare_clear(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.clear_pipeline.is_none() {
            let shader = with_stats_layout(PUBLISH_SHADER_SOURCE);
            self.prepare_clear_with_shader(device, &shader);
        }
    }

    fn prepare_clear_with_shader(&mut self, device: &manifold_gpu::GpuDevice, shader: &str) {
        self.clear_pipeline = Some(device.create_compute_pipeline(shader, "interior_clear", "liquid.clear_interior"));
    }

    fn prepare_copy(&mut self, device: &manifold_gpu::GpuDevice, shader: &str) {
        if self.copy_pipeline.is_none() {
            self.copy_pipeline = Some(device.create_compute_pipeline(shader, "cs_main", "liquid.publish_faces"));
        }
    }

    pub fn clear(&self, gpu: &mut GpuEncoder<'_>, target: &GpuBuffer) -> Result<(), String> {
        if target.size == 0 || !target.size.is_multiple_of(4) {
            return Err(format!("interior distance buffer has invalid extent {} bytes", target.size));
        }
        let len = u32::try_from(target.size / 4)
            .map_err(|_| format!("interior distance buffer has {} records, past 32-bit GPU indexing", target.size / 4))?;
        let pipeline = self.clear_pipeline.as_ref().expect("liquid interior pipeline prepared at install");
        let params = FaceParams { len, _pad0: 0, _pad1: 0, has_shortage: 0 };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                GpuBinding::Buffer { binding: 3, buffer: target, offset: 0 },
            ],
            [len.div_ceil(256), 1, 1],
            "liquid.clear_interior",
        );
        Ok(())
    }

}

/// A frame node's published face grid, frame B's only: storage per wired
/// axis over the domain's cells, written when the frame publishes a tick and
/// never from a tick whose stats flag a non-finite record. Every liquid frame
/// publishes through this, so the grid holds while paused and I8 covers it
/// for every solver.
#[derive(Default)]
pub struct PublishedFaces {
    faces: [Option<GpuBuffer>; 3],
    ops: InteriorOps,
}

/// A tick cannot be published when its particle reduction found non-finite
/// records or when narrow-band reseeding ran out of particle slots.
pub fn stats_failed(stats: &[u32]) -> bool {
    stats.first().copied().unwrap_or(1) != 0 || stats.get(NARROW_BAND_SHORTAGE_WORD as usize).copied().unwrap_or(1) != 0
}

impl PublishedFaces {
    pub fn prepare(&mut self, device: &manifold_gpu::GpuDevice) {
        self.ops.prepare(device);
    }

    /// Whether `port` is one of the grid's arrays.
    pub fn provides(port: &str) -> bool {
        FACE_GRID_PORTS[..3].contains(&port)
    }

    /// The array published on `port`.
    pub fn buffer(&self, port: &str) -> Option<&GpuBuffer> {
        FACE_GRID_PORTS[..3].iter().position(|&p| p == port).and_then(|axis| self.faces[axis].as_ref())
    }

    /// Every axis has storage.
    pub fn complete(&self) -> bool {
        self.faces.iter().all(Option::is_some)
    }

    /// Publish `inputs` (one per axis, `None` unwired) over `cells`. Each
    /// wired axis gets storage, zero until written and fresh per lattice (the
    /// old one retires with its fence), and is written when `ticked` or when
    /// its storage is new, since a new lattice's old faces never stand in for
    /// it. `stats` word 0 and the named shortage word are the tick's failure
    /// counts; the kernel skips a tick that has either. Returns a refusal
    /// naming what the device could not
    /// give.
    pub fn publish(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        cells: [u32; 3],
        inputs: [Option<&GpuBuffer>; 3],
        stats: Option<&GpuBuffer>,
        ticked: bool,
        node: &str,
    ) -> Option<String> {
        let device = gpu.device;
        let mut refused = None;
        for (axis, input) in inputs.into_iter().enumerate() {
            let bytes = face_len(cells, axis) * 4;
            let Some(input) = input else {
                self.faces[axis] = None;
                continue;
            };
            let mut fresh = false;
            if self.faces[axis].as_ref().is_none_or(|b| b.size < bytes) {
                match device.try_create_buffer_shared(bytes.max(4)) {
                    Ok(buffer) => {
                        buffer.zero_fill();
                        self.faces[axis] = Some(buffer);
                        fresh = true;
                    }
                    Err(error) => {
                        self.faces[axis] = None;
                        refused = Some(format!(
                            "{node}: the face grid needs 3 × {bytes} bytes the device cannot give: {error}. Lower Resolution."
                        ));
                        continue;
                    }
                }
            }
            let (Some(stats), Some(target)) = (stats.filter(|stats| stats.size >= 4), self.faces[axis].as_ref()) else { continue };
            if !(ticked || fresh) {
                continue;
            }
            let len = bytes.min(input.size).min(target.size) / 4;
            let Ok(len) = u32::try_from(len) else {
                refused = Some(format!("{node}: a face array of {len} faces is past 32-bit GPU indexing. Lower Resolution."));
                continue;
            };
            if len == 0 {
                continue;
            }
            let pipeline = self.ops.copy_pipeline.as_ref().expect("liquid face pipeline prepared at install");
            let has_shortage = u32::from(stats_has_shortage(stats.size));
            let params = FaceParams { len, _pad0: 0, _pad1: 0, has_shortage };
            gpu.native_enc.dispatch_compute(
                pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                    GpuBinding::Buffer { binding: 1, buffer: input, offset: 0 },
                    GpuBinding::Buffer { binding: 2, buffer: stats, offset: 0 },
                    GpuBinding::Buffer { binding: 3, buffer: target, offset: 0 },
                ],
                [len.div_ceil(256), 1, 1],
                "liquid.publish_faces",
            );
        }
        refused
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_publish_params_match_the_shader() {
        assert_eq!(std::mem::size_of::<FaceParams>(), 16);
        let shader = with_stats_layout(PUBLISH_SHADER_SOURCE);
        assert!(shader.contains("struct FaceParams"));
        let module = naga::front::wgsl::parse_str(&shader).expect("liquid_frame_faces.wgsl parses");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .expect("liquid_frame_faces.wgsl validates");
    }

    /// The seam table's lengths and index rules, on unequal sides so a
    /// swapped axis shows.
    #[test]
    fn face_grid_layout_matches_the_seam_table() {
        let n = [6, 5, 4];
        assert_eq!([0, 1, 2].map(|a| face_len(n, a)), [7 * 5 * 4, 6 * 6 * 4, 6 * 5 * 5]);
        assert_eq!(face_index(n, 0, [2, 3, 1]), 2 + 7 * (3 + 5));
        assert_eq!(face_index(n, 1, [2, 3, 1]), 2 + 6 * (3 + 6));
        assert_eq!(face_index(n, 2, [2, 3, 1]), 2 + 6 * (3 + 5));
        for a in 0..3 {
            for i in 0..face_len(n, a) as usize {
                assert_eq!(face_index(n, a, face_coords(n, a, i)), i);
            }
        }
        assert_eq!(face_position([-2.0, 0.0, 1.0], 0.5, 1, [1, 2, 3]), [-1.25, 1.0, 2.75]);
        assert_eq!(interior_len(n), 6 * 5 * 4);
        assert_eq!(interior_bytes(n), 6 * 5 * 4 * 4);
    }

    #[test]
    fn failed_stats_include_narrow_band_capacity_shortage() {
        assert!(!stats_failed(&[0; LIQUID_STATS_WORDS as usize]));
        let mut words = [0; LIQUID_STATS_WORDS as usize];
        words[NARROW_BAND_SHORTAGE_WORD as usize] = 1;
        assert!(stats_failed(&words));
        assert!(stats_failed(&[1]));
    }

    #[test]
    fn short_stats_never_enable_shortage_word_read() {
        assert!(!stats_has_shortage(4));
        assert!(!stats_has_shortage(u64::from(LIQUID_STATS_WORDS - 1) * 4));
        assert!(stats_has_shortage(u64::from(LIQUID_STATS_WORDS) * 4));
    }
}
