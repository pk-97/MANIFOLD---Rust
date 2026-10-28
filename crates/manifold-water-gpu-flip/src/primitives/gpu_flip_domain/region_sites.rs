//! The half-cell sites each Inflow emits into: one box of world sites per
//! region row, published beside the region rows so emission enumerates an
//! inflow's own sites and never the whole domain.
//!
//! World half cell J sits at (J/2 - 1/4)·h along an axis: cell p, centred on
//! p·h, holds half cells 2p and 2p + 1. These are the fill's sites. A region
//! holds a point only inside its shape's distance lattice, so the box is the
//! world box round that lattice while the row's pose runs from the tick's
//! start to its end. The kernel still tests every site against the real
//! distance; the box only bounds which sites it visits.

use manifold_gpu::BufferUse;
use manifold_gpu::GpuBuffer;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::ports::EXACT_F32_COUNT;
use manifold_water_liquid::bodies::{LiquidBody, LiquidShape, REGION_INFLOW, body_pose_at, rotate};
use manifold_water_liquid::body_buffers::LiquidBodyBuffers;

/// One region row's site box, 32 bytes. An outflow, a disabled row or a row
/// whose shape is missing holds no sites (`count` 0).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RegionSites {
    /// World half cell of the box's first site, per axis (any sign).
    pub min: [i32; 3],
    /// Sites in the earlier rows of the same tick: the box's first index in
    /// the tick's enumeration, which runs row after row, x fastest in a box.
    pub first: u32,
    /// Sites per axis.
    pub size: [u32; 3],
    /// size.x · size.y · size.z.
    pub count: u32,
}

const _: () = assert!(std::mem::size_of::<RegionSites>() == 32);

/// The turn, in radians, one sample of a rotating pose may span. Between
/// samples a corner leaves its chord by at most r·(1 - cos(turn / 2)), which
/// the box adds back.
const MAX_SAMPLE_TURN: f64 = 0.5;
/// Most samples along one tick.
const MAX_SAMPLES: f64 = 64.0;

/// The region rows' site boxes of the last rows call, and the most sites any
/// tick enumerates.
#[derive(Debug, Default)]
pub struct RegionSiteRows {
    rows: Vec<RegionSites>,
    total: u32,
}

impl RegionSiteRows {
    /// The boxes, tick-major like the region rows they sit beside.
    pub fn rows(&self) -> &[RegionSites] {
        &self.rows
    }

    /// The most sites one tick's inflows enumerate: the dispatch size.
    pub fn total(&self) -> u32 {
        self.total
    }

    /// Rebuilds the boxes for `regions`, `region_count` rows a tick, tick
    /// after tick, over `shapes`. `seconds(tick)` bounds the pose time within
    /// that tick: the poses the solver reads run from the row's start to that
    /// long after it. Refused when one tick would enumerate more sites than a
    /// count carries exactly; the rows are then empty.
    pub fn rebuild(
        &mut self,
        regions: &[LiquidBody],
        region_count: usize,
        shapes: &[LiquidShape],
        cell_size: f32,
        seconds: impl Fn(usize) -> f32,
    ) -> Result<(), String> {
        let built = self.fill(regions, region_count, shapes, cell_size, seconds);
        if built.is_err() {
            self.rows.clear();
            self.total = 0;
        }
        built
    }

    fn fill(
        &mut self,
        regions: &[LiquidBody],
        region_count: usize,
        shapes: &[LiquidShape],
        cell_size: f32,
        seconds: impl Fn(usize) -> f32,
    ) -> Result<(), String> {
        self.rows.clear();
        self.total = 0;
        if region_count == 0 {
            return Ok(());
        }
        for (tick, rows) in regions.chunks(region_count).enumerate() {
            let mut in_tick = 0u64;
            for row in rows {
                let mut sites = row_sites(row, shapes, cell_size, seconds(tick))?;
                sites.first = in_tick as u32;
                in_tick += u64::from(sites.count);
                if in_tick > u64::from(EXACT_F32_COUNT) {
                    return Err(format!(
                        "GPU FLIP: the Inflows' site boxes pass {EXACT_F32_COUNT} sites in one tick, more than a count carries exactly. Make an Inflow smaller or raise Cell Size."
                    ));
                }
                self.rows.push(sites);
            }
            self.total = self.total.max(in_tick as u32);
        }
        Ok(())
    }
}

/// One row's box, with `first` left at 0. The sites of the world box round
/// the shape's lattice over the pose times 0..=`seconds`, a quarter cell of
/// slack each side for the kernel's f32 rounding.
fn row_sites(
    row: &LiquidBody,
    shapes: &[LiquidShape],
    cell_size: f32,
    seconds: f32,
) -> Result<RegionSites, String> {
    let shape = row.accel_shape[3];
    if row.angular_velocity[3] != REGION_INFLOW || shape < 0.0 {
        return Ok(RegionSites::default());
    }
    let Some(shape) = shapes.get(shape as usize) else {
        return Ok(RegionSites::default());
    };
    let [lo, hi] = swept_box(row, shape, seconds);
    let h = f64::from(cell_size);
    let mut sites = RegionSites::default();
    let mut count = 1u64;
    for a in 0..3 {
        let first = (2.0 * (lo[a] - 0.25 * h) / h + 0.5).ceil();
        let last = (2.0 * (hi[a] + 0.25 * h) / h + 0.5).floor();
        if !(first.abs() < 1.0e9 && last.abs() < 1.0e9) {
            return Err("GPU FLIP: an Inflow is out of range or not finite".into());
        }
        sites.min[a] = first as i32;
        sites.size[a] = (last - first + 1.0) as u32;
        count *= u64::from(sites.size[a]);
    }
    sites.count = u32::try_from(count).map_err(|_| {
        format!("GPU FLIP: an Inflow's site box holds {count} sites, more than a count carries exactly. Make it smaller or raise Cell Size.")
    })?;
    Ok(sites)
}

/// The lattice's eight corners as offsets from the body, scaled as the kernel
/// scales a lattice coordinate.
fn corners(shape: &LiquidShape) -> [[f32; 3]; 8] {
    let dims = [shape.dims_x, shape.dims_y, shape.dims_z];
    std::array::from_fn(|c| {
        std::array::from_fn(|a| {
            let node = if (c >> a) & 1 == 1 {
                dims[a].saturating_sub(1)
            } else {
                0
            };
            (shape.origin_spacing[a] + node as f32 * shape.origin_spacing[3]) * shape.scale_min[a]
        })
    })
}

/// The world box (min, max) round `shape`'s distance lattice posed by `row`
/// at every time from 0 to `seconds` after the row's start. The lattice's
/// corners are sampled along the pose; between samples a turning corner
/// leaves its chord by at most the sagitta, added to every side.
fn swept_box(row: &LiquidBody, shape: &LiquidShape, seconds: f32) -> [[f64; 3]; 2] {
    let corners = corners(shape);
    let reach = corners
        .iter()
        .map(|c| f64::from(c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt())
        .fold(0.0, f64::max);
    let w = row.angular_velocity;
    let turn = f64::from((w[0] * w[0] + w[1] * w[1] + w[2] * w[2]).sqrt()) * f64::from(seconds);
    let samples = (turn / MAX_SAMPLE_TURN).ceil().clamp(1.0, MAX_SAMPLES);
    let sagitta = reach * (1.0 - (0.5 * turn / samples).cos());
    let mut bounds = [[f64::INFINITY; 3], [f64::NEG_INFINITY; 3]];
    for i in 0..=samples as u32 {
        let t = seconds * i as f32 / samples as f32;
        let (position, rotation) = body_pose_at(row, t);
        for corner in corners {
            let offset = rotate(rotation, corner);
            for a in 0..3 {
                let x = f64::from(position[a] + offset[a]);
                bounds[0][a] = bounds[0][a].min(x);
                bounds[1][a] = bounds[1][a].max(x);
            }
        }
    }
    [
        bounds[0].map(|v| v - sagitta),
        bounds[1].map(|v| v + sagitta),
    ]
}

/// The domain's provided `region_sites` array: one [`RegionSites`] per region
/// row.
#[derive(Default)]
pub struct RegionSiteBuffer {
    buffer: Option<GpuBuffer>,
}

impl RegionSiteBuffer {
    pub fn buffer(&self) -> Option<&GpuBuffer> {
        self.buffer.as_ref()
    }

    /// Keeps the buffer at `rows`' size (at least one row) and, when the
    /// frame's rows are fresh, writes them in encoder order through
    /// `uploader`, so a box the previous frame's substeps may still read is
    /// never overwritten early.
    pub fn upload(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        uploader: &mut LiquidBodyBuffers,
        rows: &RegionSiteRows,
        fresh: bool,
        label: &'static str,
    ) {
        let bytes =
            std::mem::size_of_val(rows.rows()).max(std::mem::size_of::<RegionSites>()) as u64;
        if self
            .buffer
            .as_ref()
            .is_none_or(|buffer| buffer.size < bytes)
        {
            self.buffer = Some(gpu.device.create_buffer(bytes, BufferUse::Upload));
        }
        if fresh && let Some(buffer) = &self.buffer {
            uploader.write_rows(gpu, buffer, bytemuck::cast_slice(rows.rows()), label);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_water_liquid::bodies::REGION_OUTFLOW;

    const H: f32 = 0.125;

    /// A cube lattice of `dims` nodes at `spacing`, centred on the body's
    /// origin, scaled per axis.
    fn shape(dims: u32, spacing: f32, scale: [f32; 3]) -> LiquidShape {
        let half = -0.5 * (dims - 1) as f32 * spacing;
        LiquidShape {
            origin_spacing: [half, half, half, spacing],
            dims_x: dims,
            dims_y: dims,
            dims_z: dims,
            atlas_offset: 0,
            scale_min: [
                scale[0],
                scale[1],
                scale[2],
                scale[0].min(scale[1]).min(scale[2]),
            ],
        }
    }

    fn inflow(position: [f32; 3], velocity: [f32; 3], spin: [f32; 3], shape: f32) -> LiquidBody {
        LiquidBody {
            position_inv_mass: [position[0], position[1], position[2], 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            linear_velocity: [velocity[0], velocity[1], velocity[2], 0.0],
            angular_velocity: [spin[0], spin[1], spin[2], REGION_INFLOW],
            accel_shape: [0.0, 0.0, 0.0, shape],
            ..LiquidBody::default()
        }
    }

    fn rebuilt(
        rows: &[LiquidBody],
        region_count: usize,
        shapes: &[LiquidShape],
        seconds: f32,
    ) -> Result<RegionSiteRows, String> {
        let mut out = RegionSiteRows::default();
        out.rebuild(rows, region_count, shapes, H, |_| seconds)?;
        Ok(out)
    }

    /// The sites of the world box round the lattice posed at exactly `t`,
    /// as a half-cell range per axis, found without sampling.
    fn sites_at(row: &LiquidBody, shape: &LiquidShape, t: f32) -> [[i64; 2]; 3] {
        let (position, rotation) = body_pose_at(row, t);
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for corner in corners(shape) {
            let offset = rotate(rotation, corner);
            for a in 0..3 {
                let x = f64::from(position[a] + offset[a]);
                lo[a] = lo[a].min(x);
                hi[a] = hi[a].max(x);
            }
        }
        let h = f64::from(H);
        std::array::from_fn(|a| {
            [
                (2.0 * lo[a] / h + 0.5).ceil() as i64,
                (2.0 * hi[a] / h + 0.5).floor() as i64,
            ]
        })
    }

    /// Every site the region could hold at any pose time within `seconds`
    /// lies in its box: the sites of the lattice's world box at times between
    /// the samples included.
    fn assert_covers(row: &LiquidBody, shape: &LiquidShape, seconds: f32) {
        let sites = row_sites(row, std::slice::from_ref(shape), H, seconds).unwrap();
        for step in 0..=24 {
            let t = seconds * step as f32 / 24.0;
            for (a, [first, last]) in sites_at(row, shape, t).into_iter().enumerate() {
                let (min, max) = (
                    i64::from(sites.min[a]),
                    i64::from(sites.min[a]) + i64::from(sites.size[a]) - 1,
                );
                assert!(
                    min <= first && last <= max,
                    "t {t} axis {a}: sites {first}..={last} outside {min}..={max}"
                );
            }
        }
    }

    #[test]
    fn a_resting_inflow_box_is_its_lattice_with_a_site_of_slack() {
        // The lattice spans 1 m round (-1, 0.5, 2): x -1.5..-0.5, y 0..1,
        // z 1.5..2.5. The sites inside are half cells -23..=-8 on x; the box
        // takes one more each side.
        let shapes = [shape(5, 0.25, [1.0; 3])];
        let row = inflow([-1.0, 0.5, 2.0], [0.0; 3], [0.0; 3], 0.0);
        let rows = rebuilt(&[row], 1, &shapes, 1.0 / 60.0).unwrap();
        assert_eq!(rows.rows()[0].min, [-24, 0, 24]);
        assert_eq!(rows.rows()[0].size, [18, 18, 18]);
        assert_eq!(rows.rows()[0].count, 18 * 18 * 18);
        assert_eq!(rows.total(), 18 * 18 * 18);
    }

    #[test]
    fn the_box_covers_a_moving_turning_scaled_inflow_across_the_origin() {
        // A slab, scaled unequally, turned about a diagonal axis, drifting
        // across the origin through a long tick: every site it holds at any
        // pose time is in the box.
        let shapes = [shape(7, 0.25, [2.0, 0.5, 1.5])];
        let mut row = inflow([-0.4, 0.2, 0.3], [3.0, -1.0, 2.0], [4.0, 9.0, -6.0], 0.0);
        // A unit quaternion: 0.9 rad about the axis (0.3, 0.5, 0.1).
        let (sin, cos) = (0.45f32.sin(), 0.45f32.cos());
        let norm = 0.35f32.sqrt();
        row.rotation = [0.3 / norm * sin, 0.5 / norm * sin, 0.1 / norm * sin, cos];
        for seconds in [1.0 / 60.0, 0.1, 0.25] {
            assert_covers(&row, &shapes[0], seconds);
        }
        // Without the turn the box is the tight one: the same row at rest
        // is no larger than the one that swept.
        let still = LiquidBody {
            linear_velocity: [0.0; 4],
            angular_velocity: [0.0, 0.0, 0.0, REGION_INFLOW],
            ..row
        };
        let swept = row_sites(&row, &shapes, H, 0.25).unwrap();
        let rest = row_sites(&still, &shapes, H, 0.25).unwrap();
        assert!(rest.count < swept.count, "{rest:?} vs {swept:?}");
    }

    #[test]
    fn rows_count_per_tick_and_skip_what_emits_nothing() {
        let shapes = [shape(3, 0.5, [1.0; 3]), shape(5, 0.5, [1.0; 3])];
        let big = inflow([0.0; 3], [0.0; 3], [0.0; 3], 1.0);
        let small = inflow([0.5, 0.0, -0.5], [0.0; 3], [0.0; 3], 0.0);
        let mut outflow = inflow([0.0; 3], [0.0; 3], [0.0; 3], 1.0);
        outflow.angular_velocity[3] = REGION_OUTFLOW;
        let off = inflow([0.0; 3], [0.0; 3], [0.0; 3], -1.0);
        // Two ticks of four regions; the second tick's small inflow moves
        // away along x, so its box differs.
        let mut second = small;
        second.position_inv_mass[0] = 3.5;
        let rows = [small, outflow, off, big, second, outflow, off, big];
        let sites = rebuilt(&rows, 4, &shapes, 1.0 / 60.0).unwrap();
        let counts: Vec<u32> = sites.rows().iter().map(|r| r.count).collect();
        assert_eq!(counts[1], 0, "an outflow emits nothing");
        assert_eq!(counts[2], 0, "a disabled row emits nothing");
        assert_eq!(counts[0], counts[4], "the same inflow, the same box size");
        assert_eq!(
            sites.rows()[3].first,
            counts[0],
            "after the tick's earlier rows"
        );
        assert_eq!(sites.rows()[4].first, 0, "each tick counts from 0");
        assert_ne!(sites.rows()[0].min, sites.rows()[4].min);
        assert_eq!(sites.total(), counts[0] + counts[3]);
        assert_eq!(sites.rows().len(), 8);
        // No region rows: nothing to enumerate.
        assert_eq!(rebuilt(&[], 0, &shapes, 1.0).unwrap().total(), 0);
    }

    #[test]
    fn an_inflow_past_a_counts_exact_range_is_refused_by_name() {
        // A 40 m cube at 1/8 m holds 640^3 sites.
        let shapes = [shape(81, 0.5, [1.0; 3])];
        let row = inflow([0.0; 3], [0.0; 3], [0.0; 3], 0.0);
        let refused = rebuilt(&[row], 1, &shapes, 1.0 / 60.0).expect_err("too many sites");
        assert!(
            refused.contains("Inflow") && refused.contains("Cell Size"),
            "{refused}"
        );
    }
}
